//! The compound file reader extracts streams and storages byte for byte,
//! extracts the one CAB of an installer, rejects each hostile layout with
//! nothing left behind, and honours the options the other readers take.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/cfb_writer.rs"]
mod cfb_writer;
#[path = "common/lzx_encoder.rs"]
mod lzx_encoder;
#[path = "common/quantum_encoder.rs"]
mod quantum_encoder;

use cfb_writer::{Built, Compound, END_OF_CHAIN, FREE, NO_ENTRY, Node};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zipper::{
    Control, Existing, Metadata, Strip,
    native_msi::{ReadOptions, read_archive},
};

const NAME_LENGTH: usize = 64;
const TYPE: usize = 66;
const COLOR: usize = 67;
const LEFT: usize = 68;
const RIGHT: usize = 72;
const CHILD: usize = 76;
const START: usize = 116;
const SIZE: usize = 120;

fn leftovers(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string()
        })
        .filter(|path| path != "case.msi")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.msi");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile compound file"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile compound file"
    );
    error
}

fn assert_rejected_with(case: &str, bytes: &[u8], words: &str) {
    let error = assert_rejected_without_leftovers(case, bytes);
    assert!(
        error.contains(words),
        "{case}: {error:?} does not say {words:?}"
    );
}

fn extract_with(
    bytes: &[u8],
    options: ReadOptions,
) -> (
    tempfile::TempDir,
    PathBuf,
    anyhow::Result<Vec<zipper::Entry>>,
) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.msi");
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    let result = read_archive(&archive, Some(&destination), options);
    (root, destination, result)
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let contents = if entry.file_type().is_dir() {
                b"<dir>".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, contents)
        })
        .collect()
}

fn expected(files: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    let mut tree = BTreeMap::new();
    for (name, contents) in files {
        let mut parent = Path::new(name).parent();
        while let Some(folder) = parent.filter(|folder| !folder.as_os_str().is_empty()) {
            tree.insert(folder.to_string_lossy().into_owned(), b"<dir>".to_vec());
            parent = folder.parent();
        }
        tree.insert((*name).to_owned(), contents.to_vec());
    }
    tree
}

fn bytes_of(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn sample(version: u16) -> Compound {
    Compound::new(
        version,
        vec![
            cfb_writer::stream("mini", &bytes_of(100, 1)),
            cfb_writer::stream("large", &bytes_of(9000, 2)),
            cfb_writer::storage(
                "dir",
                vec![
                    cfb_writer::stream("inner", &bytes_of(70, 3)),
                    cfb_writer::storage(
                        "deeper",
                        vec![cfb_writer::stream("big", &bytes_of(5000, 4))],
                    ),
                    cfb_writer::stream("empty", b""),
                ],
            ),
            cfb_writer::stream("tail", b"!"),
        ],
    )
}

fn sample_tree() -> BTreeMap<String, Vec<u8>> {
    expected(&[
        ("mini", &bytes_of(100, 1)),
        ("large", &bytes_of(9000, 2)),
        ("dir/inner", &bytes_of(70, 3)),
        ("dir/deeper/big", &bytes_of(5000, 4)),
        ("dir/empty", b""),
        ("tail", b"!"),
    ])
}

fn embedded_cab() -> Vec<u8> {
    let large: Vec<u8> = (0..4000)
        .flat_map(|line| format!("line {line} of the payload\r\n").into_bytes())
        .collect();
    cab_writer::one_folder_in_order(
        &[
            ("readme.txt", b"hello from the cab\r\n"),
            ("docs/large.txt", &large),
        ],
        &cab_writer::Method::Mszip,
    )
    .build()
}

fn installer(version: u16, cab_name: &str, cab: &[u8]) -> Compound {
    Compound::new(
        version,
        vec![
            cfb_writer::raw_stream(cfb_writer::msi_name("_Tables", true), b"tables"),
            cfb_writer::raw_stream(cfb_writer::msi_name(cab_name, false), cab),
            cfb_writer::raw_stream(cfb_writer::msi_name("Binary.Icon", false), &[1; 700]),
        ],
    )
}

#[test]
fn both_versions_extract_byte_for_byte_and_verify_without_a_destination() {
    for version in [3, 4] {
        for scatter in [None, Some(5), Some(77)] {
            let mut compound = sample(version);
            compound.scatter = scatter;
            let bytes = compound.build().bytes;
            let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
            let entries = result.unwrap_or_else(|error| panic!("v{version}: {error:#}"));
            assert_eq!(tree(&destination), sample_tree(), "v{version} {scatter:?}");
            assert_eq!(entries.len(), 8);
            let archive = root.path().join("case.msi");
            assert_eq!(
                read_archive(&archive, None, ReadOptions::default())
                    .unwrap()
                    .len(),
                8
            );
        }
    }
}

#[test]
fn an_installer_extracts_its_one_cab_and_leaves_no_copy_behind() {
    let cab = embedded_cab();
    for version in [3, 4] {
        let bytes = installer(version, "media1.cab", &cab).build().bytes;
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap();
        let large: Vec<u8> = (0..4000)
            .flat_map(|line| format!("line {line} of the payload\r\n").into_bytes())
            .collect();
        assert_eq!(
            tree(&destination),
            expected(&[
                ("readme.txt", b"hello from the cab\r\n"),
                ("docs/large.txt", &large)
            ])
        );
        let mut left: Vec<String> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["case.msi", "out"], "v{version}");
        let archive = root.path().join("case.msi");
        assert_eq!(
            read_archive(&archive, None, ReadOptions::default())
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn an_installer_with_two_cabs_or_none_extracts_its_streams() {
    let cab = embedded_cab();
    let mut two = installer(3, "one.cab", &cab);
    two.children.push(cfb_writer::raw_stream(
        cfb_writer::msi_name("two.cab", false),
        &cab,
    ));
    let (_root, destination, result) = extract_with(&two.build().bytes, ReadOptions::default());
    result.unwrap();
    let names: Vec<String> = tree(&destination).into_keys().collect();
    assert_eq!(names, ["!_Tables", "Binary.Icon", "one.cab", "two.cab"]);
    let mut cab_and_exe = installer(3, "one.cab", &cab);
    cab_and_exe.children.push(cfb_writer::raw_stream(
        cfb_writer::msi_name("Icon.setup.exe", false),
        b"MZ",
    ));
    let (_root, destination, result) =
        extract_with(&cab_and_exe.build().bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        tree(&destination).len(),
        4,
        "a cab and an exe stream are two candidates"
    );
    let plain = Compound::new(3, vec![cfb_writer::stream("only.cab", &cab)]);
    let (_root, destination, result) = extract_with(&plain.build().bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("only.cab", &cab)]),
        "without MSI-encoded names a compound file is not an installer"
    );
}

#[test]
fn an_installer_whose_main_stream_is_not_a_cab_goes_to_7zip() {
    let root = tempfile::tempdir().unwrap();
    for (label, name, data) in [
        ("junk cab", "media1.cab", &b"not a cabinet at all"[..]),
        ("exe", "Binary.setupexe", b"MZ\x90\0"),
        ("empty cab", "media1.cab", b""),
    ] {
        let bytes = installer(3, name, data).build().bytes;
        assert_rejected_with(label, &bytes, "--zip-engine backend");
        let path = root.path().join(format!("{label}.msi"));
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
            zipper::Reader::Backend { format: None },
            "{label}"
        );
        assert_eq!(
            zipper::choose_reader(zipper::Engine::Native, &path).unwrap(),
            zipper::Reader::Msi,
            "{label}"
        );
    }
    let path = root.path().join("good.msi");
    fs::write(
        &path,
        installer(4, "media1.cab", &embedded_cab()).build().bytes,
    )
    .unwrap();
    let docs = root.path().join("doc.doc");
    fs::write(&docs, sample(3).build().bytes).unwrap();
    let broken = root.path().join("broken.msi");
    let mut header = sample(3).build().bytes;
    header.truncate(600);
    fs::write(&broken, header).unwrap();
    for file in [&path, &docs, &broken] {
        for engine in [zipper::Engine::Auto, zipper::Engine::Native] {
            assert_eq!(
                zipper::choose_reader(engine, file).unwrap(),
                zipper::Reader::Msi
            );
        }
    }
}

#[test]
fn a_damaged_embedded_cab_is_refused_without_leftovers() {
    let mut cab = embedded_cab();
    let last = cab.len() - 10;
    cab[last] ^= 0xFF;
    assert_rejected_without_leftovers(
        "damaged cab",
        &installer(3, "media1.cab", &cab).build().bytes,
    );
    let mut cab = embedded_cab();
    cab.truncate(cab.len() - 1);
    assert_rejected_with(
        "truncated cab",
        &installer(4, "media1.cab", &cab).build().bytes,
        "CAB",
    );
}

fn with(version: u16, change: impl FnOnce(&mut Built)) -> Vec<u8> {
    let mut built = sample(version).build();
    change(&mut built);
    built.bytes
}

#[test]
fn headers_that_lie_are_refused() {
    for version in [3u16, 4] {
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "minor",
                with(version, |b| b.set_u16(0x18, 0x3D)),
                "minor version",
            ),
            (
                "major",
                with(version, |b| b.set_u16(0x1A, 5)),
                "major version",
            ),
            (
                "order",
                with(version, |b| b.set_u16(0x1C, 0xFEFF)),
                "little-endian",
            ),
            (
                "shift",
                with(version, |b| {
                    b.set_u16(0x1E, if version == 3 { 12 } else { 9 })
                }),
                "sector size",
            ),
            (
                "mini shift",
                with(version, |b| b.set_u16(0x20, 7)),
                "64 bytes",
            ),
            (
                "reserved",
                with(version, |b| b.set_u16(0x22, 1)),
                "reserved",
            ),
            (
                "directory count",
                with(version, |b| {
                    let value = if version == 3 { 1 } else { 0 };
                    b.set_u32(0x28, value)
                }),
                "directory sector count",
            ),
            (
                "no fat",
                with(version, |b| b.set_u32(0x2C, 0)),
                "impossible size",
            ),
            ("cutoff", with(version, |b| b.set_u32(0x38, 4095)), "cutoff"),
            ("fat count", with(version, |b| b.set_u32(0x2C, 2)), "FAT"),
            (
                "huge fat",
                with(version, |b| b.set_u32(0x2C, 1 << 14)),
                "truncated",
            ),
            (
                "huge mini fat",
                with(version, |b| b.set_u32(0x40, 1 << 20)),
                "truncated",
            ),
            (
                "huge difat",
                with(version, |b| b.set_u32(0x48, 1 << 10)),
                "truncated",
            ),
            (
                "fat sector past the end",
                with(version, |b| b.set_u32(0x4C, 9999)),
                "FAT sector",
            ),
            (
                "extra difat entry",
                with(version, |b| b.set_u32(0x50, 3)),
                "DIFAT does not list exactly",
            ),
            (
                "difat chain",
                with(version, |b| {
                    b.set_u32(0x48, 1);
                    b.set_u32(0x44, 0);
                }),
                "DIFAT",
            ),
            (
                "signature",
                with(version, |b| b.bytes[7] = 0),
                "not a compound file",
            ),
        ];
        for (label, bytes, words) in cases {
            assert_rejected_with(&format!("v{version} {label}"), &bytes, words);
        }
    }
}

#[test]
fn allocation_tables_that_lie_are_refused() {
    for version in [3u16, 4] {
        let built = sample(version).build();
        let large = built.index_of("large");
        let large_start = built.entries[large as usize].start;
        let second = built.fat(large_start);
        let directory = built.directory_sectors[0];
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "two links to one sector",
                with(version, |b| b.set_fat(directory, second)),
                "links two sectors to one",
            ),
            (
                "a cycle back to the start",
                with(version, |b| b.set_fat(second, large_start)),
                "already in use",
            ),
            (
                "a chain that stops early",
                with(version, |b| b.set_fat(second, END_OF_CHAIN)),
                "does not match how its sectors are used",
            ),
            (
                "fat sector not marked",
                with(version, |b| {
                    let fat = b.fat_sectors[0];
                    b.set_fat(fat, END_OF_CHAIN)
                }),
                "FAT",
            ),
            (
                "a link past the file",
                with(version, |b| b.set_fat(second, 5000)),
                "past the end",
            ),
            (
                "a sector past the file allocated",
                with(version, |b| {
                    let last = (b.bytes.len() / b.sector_size) as u32 + 3;
                    b.set_fat(last, END_OF_CHAIN)
                }),
                "past the end of the file",
            ),
            (
                "a used sector marked free",
                with(version, |b| b.set_fat(second, FREE)),
                "does not match",
            ),
            (
                "a stray difat mark",
                with(version, |b| {
                    let last = (b.bytes.len() / b.sector_size) as u32 - 2;
                    let free = (0..last).find(|&sector| b.fat(sector) == FREE);
                    if let Some(free) = free {
                        b.set_fat(free, cfb_writer::DIFAT_SECTOR);
                    } else {
                        b.set_fat(second, cfb_writer::DIFAT_SECTOR);
                    }
                }),
                "",
            ),
            (
                "directory start past the file",
                with(version, |b| b.set_u32(0x30, 4000)),
                "past the end",
            ),
            (
                "directory starts at the fat",
                with(version, |b| b.set_u32(0x30, 0)),
                "already in use",
            ),
            (
                "mini fat start in use",
                with(version, |b| {
                    let directory = b.directory_sectors[0];
                    b.set_u32(0x3C, directory)
                }),
                "",
            ),
            (
                "mini fat longer than its count",
                with(version, |b| b.set_u32(0x40, 0)),
                "mini FAT",
            ),
        ];
        for (label, bytes, words) in cases {
            assert_rejected_with(&format!("v{version} {label}"), &bytes, words);
        }
    }
}

#[test]
fn directories_that_lie_are_refused() {
    for version in [3u16, 4] {
        let built = sample(version).build();
        let dir = built.index_of("dir");
        let inner = built.index_of("inner");
        let mini = built.index_of("mini");
        let large = built.index_of("large");
        let empty = built.index_of("empty");
        let deeper = built.index_of("deeper");
        let unused = built.entries.len() as u32;
        let field = |entry: u32, at: usize| built.entry_field(entry, at);
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "name length",
                with(version, |b| b.set_u16(field(mini, NAME_LENGTH), 4)),
                "name does not match its length",
            ),
            (
                "unterminated name",
                with(version, |b| {
                    for unit in 0..32 {
                        b.set_u16(field(mini, 2 * unit), u16::from(b'x'));
                    }
                }),
                "name does not match its length",
            ),
            (
                "color",
                with(version, |b| b.bytes[field(mini, COLOR)] = 2),
                "color",
            ),
            (
                "type",
                with(version, |b| b.bytes[field(mini, TYPE)] = 3),
                "unknown type",
            ),
            (
                "first entry",
                with(version, |b| b.bytes[field(0, TYPE)] = 1),
                "root entry",
            ),
            (
                "second root",
                with(version, |b| b.bytes[field(mini, TYPE)] = 5),
                "second root",
            ),
            (
                "root siblings",
                with(version, |b| b.set_u32(field(0, LEFT), mini)),
                "root entry",
            ),
            (
                "root child zero",
                with(version, |b| b.set_u32(field(0, CHILD), 0)),
                "root entry",
            ),
            (
                "stream with children",
                with(version, |b| b.set_u32(field(mini, CHILD), inner)),
                "has children",
            ),
            (
                "empty stream with a chain",
                with(version, |b| b.set_u32(field(empty, START), 0)),
                "empty stream",
            ),
            (
                "unused entry with links",
                with(version, |b| b.set_u32(field(unused, LEFT), 1)),
                "unused entry",
            ),
            (
                "unused entry with data",
                with(version, |b| b.set_u32(field(unused, START), 7)),
                "unused entry",
            ),
            (
                "a loop to an ancestor",
                with(version, |b| b.set_u32(field(inner, LEFT), dir)),
                "loops",
            ),
            (
                "a loop to itself",
                with(version, |b| b.set_u32(field(inner, RIGHT), inner)),
                "loops",
            ),
            (
                "an entry linked twice",
                with(version, |b| b.set_u32(field(deeper, RIGHT), inner)),
                "loops",
            ),
            (
                "a link outside the directory",
                with(version, |b| b.set_u32(field(inner, LEFT), 400_000)),
                "outside",
            ),
            (
                "a link to an unused entry",
                with(version, |b| b.set_u32(field(inner, LEFT), unused)),
                "unused entry",
            ),
            (
                "an orphan entry",
                with(version, |b| {
                    let parent_link = (0..unused)
                        .flat_map(|entry| [(entry, LEFT), (entry, RIGHT), (entry, CHILD)])
                        .find(|&(entry, at)| b.get_u32(field(entry, at)) == mini)
                        .unwrap();
                    b.set_u32(field(parent_link.0, parent_link.1), NO_ENTRY);
                }),
                "outside its directory tree",
            ),
            (
                "used entries under a root without children",
                with(version, |b| b.set_u32(field(0, CHILD), NO_ENTRY)),
                "outside its directory tree",
            ),
            (
                "a stream longer than its chain",
                with(version, |b| {
                    b.set_u32(field(large, SIZE), 9000 + b.sector_size as u32)
                }),
                "entry",
            ),
            (
                "a stream shorter than its chain",
                with(version, |b| b.set_u32(field(large, SIZE), 4096)),
                "chain does not end",
            ),
            (
                "a mini stream past the mini stream",
                with(version, |b| b.set_u32(field(mini, START), 1000)),
                "entry",
            ),
            (
                "a mini stream turned regular",
                with(version, |b| b.set_u32(field(mini, SIZE), 5000)),
                "",
            ),
            (
                "two streams sharing mini sectors",
                with(version, |b| {
                    let start = b.get_u32(field(mini, START));
                    b.set_u32(field(inner, START), start)
                }),
                "share a mini sector",
            ),
            (
                "the root mini stream longer than its chain",
                with(version, |b| {
                    let size = b.get_u32(field(0, SIZE));
                    b.set_u32(field(0, SIZE), size + b.sector_size as u32)
                }),
                "",
            ),
            (
                "an unpaired surrogate",
                with(version, |b| b.set_u16(field(mini, 0), 0xD800)),
                "surrogate",
            ),
        ];
        for (label, bytes, words) in cases {
            assert_rejected_with(&format!("v{version} {label}"), &bytes, words);
        }
        let huge = with(version, |b| b.set_u32(field(large, SIZE + 4), 7));
        if version == 3 {
            let (_root, destination, result) = extract_with(&huge, ReadOptions::default());
            result.unwrap();
            assert_eq!(
                tree(&destination),
                sample_tree(),
                "v3 ignores the high size half"
            );
        } else {
            assert_rejected_without_leftovers("v4 size past its chain", &huge);
            let beyond = with(version, |b| b.set_u32(field(large, SIZE + 4), 4096));
            assert_rejected_with("v4 size past the FAT", &beyond, "larger than its FAT");
        }
    }
}

#[test]
fn storages_nested_past_256_are_refused() {
    let mut node = cfb_writer::stream("leaf", b"deep");
    for level in 0..256 {
        node = cfb_writer::storage(&format!("{}", level % 10), vec![node]);
    }
    let deep = Compound::new(4, vec![node]).build().bytes;
    assert_rejected_with("257 levels", &deep, "256 deep");
    let mut node = cfb_writer::stream("leaf", b"deep");
    for level in 0..100 {
        node = cfb_writer::storage(&format!("{}", level % 10), vec![node]);
    }
    let (_root, _destination, result) = extract_with(
        &Compound::new(4, vec![node]).build().bytes,
        ReadOptions::default(),
    );
    result.unwrap();
}

#[test]
fn unsafe_and_colliding_names_are_refused() {
    for name in ["..", ".", "a:b", "back\\slash", "nul\0byte"] {
        let units: Vec<u16> = name.encode_utf16().collect();
        let compound = Compound::new(3, vec![cfb_writer::raw_stream(units, b"data")]);
        assert_rejected_without_leftovers(name, &compound.build().bytes);
    }
    for name in [".", ".."] {
        let dot = Compound::new(
            3,
            vec![
                cfb_writer::stream("x", b"outer"),
                cfb_writer::storage(name, vec![cfb_writer::stream("x", b"inner")]),
            ],
        );
        assert_rejected_with(&format!("storage {name}"), &dot.build().bytes, "unsafe");
    }
    let both = Compound::new(
        3,
        vec![
            cfb_writer::stream("same", b"file"),
            Node::Storage {
                name: cfb_writer::msi_name("same", false),
                children: vec![cfb_writer::stream("x", b"inside")],
                modified: 0,
            },
        ],
    );
    assert_rejected_without_leftovers("a stream and a storage of one name", &both.build().bytes);
    let twice = Compound::new(
        3,
        vec![
            cfb_writer::stream("same", b"first"),
            cfb_writer::raw_stream(cfb_writer::msi_name("same", false), b"second"),
        ],
    );
    let (_root, destination, result) = extract_with(&twice.build().bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        tree(&destination).len(),
        1,
        "a repeated name keeps one file"
    );
    let slashed = Compound::new(3, vec![cfb_writer::stream("a/b", b"slash")]);
    let (_root, destination, result) = extract_with(&slashed.build().bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination), expected(&[("a_b", b"slash")]));
}

#[test]
fn a_stream_cut_short_is_refused_before_anything_is_read() {
    for version in [3u16, 4] {
        let built = sample(version).build();
        let mut bytes = built.bytes.clone();
        bytes.truncate(bytes.len() - built.sector_size + 1);
        assert_rejected_with(
            &format!("v{version} last sector cut"),
            &bytes,
            "past its end",
        );
    }
}

#[test]
fn every_cut_is_refused_or_extracts_the_whole_tree() {
    let cab = embedded_cab();
    let files = [
        sample(3).build().bytes,
        sample(4).build().bytes,
        installer(3, "media1.cab", &cab).build().bytes,
    ];
    for (index, bytes) in files.iter().enumerate() {
        let (_root, full, result) = extract_with(bytes, ReadOptions::default());
        result.unwrap();
        let whole = tree(&full);
        let step = if bytes.len() > 40_000 { 7 } else { 1 };
        for length in (0..bytes.len()).step_by(step) {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.msi");
            fs::write(&archive, &bytes[..length]).unwrap();
            let destination = root.path().join("out");
            match read_archive(&archive, Some(&destination), ReadOptions::default()) {
                Ok(_) => assert_eq!(tree(&destination), whole, "file {index} cut to {length}"),
                Err(_) => assert_eq!(leftovers(root.path()), Vec::<String>::new(), "{length}"),
            }
        }
    }
}

#[test]
fn every_damaged_metadata_byte_is_refused_or_extracts_without_a_panic() {
    let cab = embedded_cab();
    for built in [sample(3).build(), installer(4, "media1.cab", &cab).build()] {
        let sectors = built
            .fat_sectors
            .iter()
            .chain(&built.difat_sectors)
            .chain(&built.directory_sectors)
            .chain(&built.mini_fat_sectors);
        let mut positions: Vec<usize> = (0..512).collect();
        for &sector in sectors {
            let start = built.sector_offset(sector);
            positions.extend(start..start + built.sector_size);
        }
        positions.extend((0..built.bytes.len()).step_by(97));
        for at in positions {
            let mut damaged = built.bytes.clone();
            damaged[at] ^= 0xA5;
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.msi");
            fs::write(&archive, &damaged).unwrap();
            let destination = root.path().join("out");
            if read_archive(&archive, Some(&destination), ReadOptions::default()).is_err() {
                assert_eq!(leftovers(root.path()), Vec::<String>::new(), "byte {at}");
            }
        }
    }
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let bytes = sample(3).build().bytes;
    let (root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(9000),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
    drop(root);
    let installer = installer(3, "media1.cab", &embedded_cab()).build().bytes;
    let (_root, destination, result) = extract_with(
        &installer,
        ReadOptions {
            max_output_bytes: Some(100),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
}

#[test]
fn times_and_options_apply() {
    let mut compound = sample(4);
    if let Node::Storage { modified, .. } = &mut compound.children[2] {
        *modified = 133_000_000_000_000_000;
    }
    let bytes = compound.build().bytes;
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    let seconds = |name: &str| {
        fs::metadata(destination.join(name))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(seconds("dir"), 1_655_526_400);
    let selected = ["dir/deeper".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &selected,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("dir/deeper/big", &bytes_of(5000, 4))])
    );
    let include = ["inner".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            include: &include,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("dir/inner", &bytes_of(70, 3))])
    );
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: Strip::Prefix("dir"),
            metadata: Metadata::ContentOnly,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[
            ("inner", &bytes_of(70, 3)),
            ("deeper/big", &bytes_of(5000, 4)),
            ("empty", b""),
        ])
    );
    let installer = installer(3, "media1.cab", &embedded_cab()).build().bytes;
    let include = ["readme.txt".to_owned()];
    let (_root, destination, result) = extract_with(
        &installer,
        ReadOptions {
            include: &include,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("readme.txt", b"hello from the cab\r\n")])
    );
    for bytes in [&bytes, &installer] {
        let (root, destination, result) = extract_with(bytes, ReadOptions::default());
        result.unwrap();
        let archive = root.path().join("case.msi");
        assert!(read_archive(&archive, Some(&destination), ReadOptions::default()).is_err());
        assert!(
            read_archive(
                &archive,
                Some(&destination),
                ReadOptions {
                    existing: Existing::Overwrite,
                    ..ReadOptions::default()
                }
            )
            .is_ok()
        );
        let cancel = AtomicBool::new(true);
        let (root, destination, result) = extract_with(
            bytes,
            ReadOptions {
                control: Control {
                    cancel: Some(&cancel),
                    ..Control::default()
                },
                ..ReadOptions::default()
            },
        );
        assert!(result.unwrap_err().is::<zipper::Cancelled>());
        assert!(!destination.exists());
        assert_eq!(leftovers(root.path()), Vec::<String>::new());
        cancel.store(false, Ordering::Relaxed);
    }
}

#[test]
fn the_default_engine_extracts_compound_files_in_process() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("sample.doc");
    fs::write(&archive, sample(3).build().bytes).unwrap();
    let destination = root.path().join("out");
    zipper::extract_archive(&archive, &destination, zipper::ExtractOptions::default()).unwrap();
    assert_eq!(tree(&destination), sample_tree());
}

#[test]
#[ignore = "writes a 128 MB file to ZIPPER_MSI_LIMIT_FILE for a peak memory measurement"]
fn write_a_directory_at_the_entry_limit() {
    let path = std::env::var_os("ZIPPER_MSI_LIMIT_FILE").expect("set ZIPPER_MSI_LIMIT_FILE");
    let children = (0..999_999u32)
        .map(|index| cfb_writer::stream(&format!("stream{index:024}"), b""))
        .collect();
    fs::write(path, Compound::new(4, children).build().bytes).unwrap();
}

#[cfg(unix)]
#[test]
fn an_ending_signal_during_list_or_test_removes_the_cab_copy() {
    let payload: Vec<u8> = (0..48 * 1024 * 1024u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let cab =
        cab_writer::one_folder_in_order(&[("big.bin", &payload)], &cab_writer::Method::Stored)
            .build();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("big.msi");
    fs::write(&archive, installer(4, "big.cab", &cab).build().bytes).unwrap();
    for (command, signal) in [("test", libc::SIGTERM), ("list", libc::SIGINT)] {
        let temporary = root.path().join(format!("tmp-{command}"));
        fs::create_dir(&temporary).unwrap();
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_zipper"))
            .arg(command)
            .arg(&archive)
            .env("TMPDIR", &temporary)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let copies = || fs::read_dir(&temporary).unwrap().count();
        while copies() == 0 && child.try_wait().unwrap().is_none() {
            std::thread::yield_now();
        }
        // SAFETY: kill takes the child's process id, which wait has not yet reaped, and a signal.
        unsafe { libc::kill(child.id() as libc::pid_t, signal) };
        let status = child.wait().unwrap();
        assert!(!status.success(), "{command} finished before the signal");
        assert_eq!(copies(), 0, "{command} left its CAB copy after a signal");
    }
}

#[test]
fn paths_much_longer_than_the_directory_are_refused_before_extraction() {
    let long = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let named = |index: usize, children| Node::Storage {
        name: cfb_writer::msi_name(&format!("{index:02}{}", &long[2..]), false),
        children,
        modified: 0,
    };
    let leaves = (0..3000)
        .map(|leaf| cfb_writer::storage(&format!("leaf{leaf}"), Vec::new()))
        .collect();
    let mut node = named(0, leaves);
    for level in 1..15 {
        node = named(level, vec![node]);
    }
    let deep = Compound::new(3, vec![node]).build().bytes;
    assert_rejected_with("deep long paths", &deep, "paths");
    let mut node = named(0, vec![cfb_writer::stream("leaf", b"x")]);
    for level in 1..15 {
        node = named(level, vec![node]);
    }
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.msi");
    fs::write(&archive, Compound::new(3, vec![node]).build().bytes).unwrap();
    assert_eq!(
        read_archive(&archive, None, ReadOptions::default())
            .unwrap()
            .len(),
        16
    );
}

#[test]
fn an_existing_destination_is_refused_before_the_cab_is_copied() {
    let bytes = installer(3, "media1.cab", &embedded_cab()).build().bytes;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.msi");
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    fs::create_dir(&destination).unwrap();
    let error = read_archive(&archive, Some(&destination), ReadOptions::default()).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("must not exist"), "{message}");
    assert!(!message.contains("CAB stream"), "copied first: {message}");
}

#[test]
fn an_embedded_cab_that_starts_a_set_never_reads_a_cabinet_beside_its_copy() {
    let data: Vec<u8> = (0..20_000u32).flat_map(|word| word.to_le_bytes()).collect();
    let set = cab_writer::cabinet_set(
        &[cab_writer::folder(&cab_writer::Method::Stored, &data)],
        &[(0, cab_writer::file("spans.bin", 0, 0, data.len() as u32))],
        &[cab_writer::Cut {
            folder: 0,
            block: 1,
            at: 7,
        }],
        &["media1.cab", "media2.cab"],
    );
    let bytes = installer(4, "media1.cab", &set[0].build()).build().bytes;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.msi");
    fs::write(&archive, &bytes).unwrap();
    fs::write(root.path().join("media2.cab"), set[1].build()).unwrap();
    for destination in [Some(root.path().join("out")), None] {
        let error = read_archive(&archive, destination.as_deref(), ReadOptions::default())
            .expect_err("read a cabinet beside the copy");
        assert!(format!("{error:#}").contains("cabinet set"), "{error:#}");
    }
    let mut left: Vec<String> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["case.msi", "media2.cab"]);
}
