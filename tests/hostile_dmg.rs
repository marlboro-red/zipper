//! Hostile and damaged DMG images and HFS+ volumes. A damaged image is
//! refused whenever 7-Zip refuses it, and when 7-Zip extracts it, Zipper
//! either extracts the same tree or refuses for one of the stricter checks
//! `docs/reference/dmg-reader.md` lists. Loops, nodes outside their file,
//! shared extents and data larger than its declared size are refused, and
//! nothing is published on failure.
#[path = "common/dmg_writer.rs"]
mod dmg_writer;
#[path = "common/hfs_writer.rs"]
mod hfs_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use dmg_writer::{Chunk, Form, Options, Partition};
use hfs_writer::{Attribute, AttributeValue, Entry, Kind, ROOT, ROOT_PARENT, Volume};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_dmg::{ReadOptions, read_archive};

type Tree = BTreeMap<String, String>;

const EMPTY_CRC: &str = "00000000";

const STRICTER: &[&str] = &[
    "fails its CRC-32 check",
    "master checksum",
    "damaged block",
    "a zero block with data",
    "raw block whose data is not its size",
    "past the end of the file",
    "no single file system partition",
    "which the catalog does not hold",
    "lies outside the root folder",
    "names inode",
    "holds a NUL",
    "holds data of its own",
    "share block",
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn tree(root: &Path) -> Tree {
    if !root.exists() {
        return Tree::new();
    }
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = if entry.file_type().is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                format!("{:08x}", crc32fast::hash(&fs::read(entry.path()).unwrap()))
            };
            (name, kind)
        })
        .filter(|(name, _)| !name.contains(':'))
        .collect()
}

struct Outcome {
    zipper: Result<Tree, String>,
    sevenzip: Option<Tree>,
}

fn run_both(binary: &Path, image: &Path) -> Outcome {
    let work = tempfile::tempdir().unwrap();
    let ours = work.path().join("zipper");
    let zipper = read_archive(image, Some(&ours), ReadOptions::default())
        .map(|_| tree(&ours))
        .map_err(|error| format!("{error:#}"));
    if zipper.is_err() {
        assert!(
            !ours.exists(),
            "{}: published after a failure",
            image.display()
        );
    }
    let theirs = work.path().join("7z");
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(&theirs);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let sevenzip = run.status.success().then(|| tree(&theirs));
    hfs_writer_cleanup(work.path());
    Outcome { zipper, sevenzip }
}

fn hfs_writer_cleanup(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if entry.file_type().is_dir() {
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

fn judge(binary: &Path, image: &Path, label: &str) -> Option<String> {
    let outcome = run_both(binary, image);
    match (&outcome.zipper, &outcome.sevenzip) {
        (Err(_), None) => None,
        (Ok(_), None) => Some(format!("{label}: 7-Zip refused and Zipper extracted")),
        (Err(error), Some(_)) => match STRICTER.iter().find(|known| error.contains(*known)) {
            Some(known) => {
                eprintln!("stricter: {known}");
                None
            }
            None => Some(format!(
                "{label}: Zipper refused with {error:?} and 7-Zip extracted"
            )),
        },
        (Ok(ours), Some(theirs)) => {
            let mut theirs = theirs.clone();
            let inodes: Vec<&String> = ours
                .iter()
                .filter(|(path, _)| path.contains("[HFS+ Private Data]/iNode"))
                .map(|(_, kind)| kind)
                .collect();
            for (path, slot) in theirs.iter_mut() {
                let linked = ours.get(path).filter(|kind| inodes.contains(kind));
                if let Some(ours) = linked.filter(|_| slot == EMPTY_CRC) {
                    *slot = ours.clone();
                }
            }
            (ours != &theirs)
                .then(|| format!("{label}: trees differ\n  zipper {ours:?}\n  7-Zip  {theirs:?}"))
        }
    }
}

fn small_volume() -> Volume {
    let mut volume = Volume::new("Small");
    volume.entries = vec![
        hfs_writer::folder(ROOT, ROOT_PARENT, "Small"),
        hfs_writer::folder(16, ROOT, "folder"),
        hfs_writer::file(17, ROOT, "hello.txt", b"hello, world\n"),
        hfs_writer::file(18, 16, "data.bin", &[7u8; 9000]),
        hfs_writer::symlink(19, 16, "link", "data.bin"),
        hfs_writer::private_folder(20),
        hfs_writer::file(21, 20, "iNode30", b"linked\n"),
        hfs_writer::hard_link(22, ROOT, "linked", 30),
    ];
    let mut packed = hfs_writer::file(23, ROOT, "packed", &[]);
    packed.kind = Kind::File {
        data: Vec::new(),
        resource: Vec::new(),
    };
    volume.entries.push(packed);
    let text = b"packed text ".repeat(200);
    volume.attributes = vec![Attribute {
        id: 23,
        name: "com.apple.decmpfs".into(),
        value: AttributeValue::Inline(hfs_writer::decmpfs_header(
            7,
            text.len() as u64,
            &dmg_writer::lzvn(&text),
        )),
    }];
    volume.records_per_leaf = 4;
    volume.extra_blocks = 4;
    volume
}

fn dmg_of(volume: &[u8], options: &Options) -> (Vec<u8>, dmg_writer::Layout) {
    let partitions = [
        Partition {
            name: "Protective Master Boot Record (MBR : 0)".into(),
            data: vec![0; 512],
        },
        Partition {
            name: "disk image (Apple_HFS : 1)".into(),
            data: volume.to_vec(),
        },
    ];
    dmg_writer::write(&partitions, options)
}

fn positions(range: std::ops::Range<usize>, step: usize) -> impl Iterator<Item = usize> {
    range.step_by(step.max(1))
}

#[test]
fn damaged_trailers_property_lists_and_chunks_are_refused_when_7zip_refuses_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (volume, _) = hfs_writer::build(&small_volume());
    let mut problems = Vec::new();
    let mut checked = 0;
    for form in [Form::Plist, Form::ResourceFork] {
        let options = Options {
            chunk_sectors: 16,
            methods: vec![Chunk::Zlib, Chunk::Raw, Chunk::Adc, Chunk::Lzfse],
            form,
            data_checksum: form == Form::ResourceFork,
            ..Options::default()
        };
        let (image, layout) = dmg_of(&volume, &options);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("damaged.dmg");
        let mut targets: Vec<usize> = positions(layout.koly..layout.koly + 512, 1).collect();
        let (forks, length) = match form {
            Form::Plist => layout.xml,
            Form::ResourceFork => layout.resource,
        };
        targets.extend(positions(forks..forks + length, 5));
        for &(_, at, length) in &layout.chunks {
            targets.extend(positions(at..at + length, 89));
        }
        for at in targets {
            for flip in [0x01u8, 0x80] {
                let mut damaged = image.clone();
                damaged[at] ^= flip;
                fs::write(&path, &damaged).unwrap();
                if let Some(problem) =
                    judge(&binary, &path, &format!("{form:?} byte {at} ^ {flip:#x}"))
                {
                    problems.push(problem);
                }
                checked += 1;
            }
        }
        for cut in positions(0..image.len(), 97) {
            fs::write(&path, &image[..cut]).unwrap();
            if let Some(problem) = judge(&binary, &path, &format!("{form:?} cut at {cut}")) {
                problems.push(problem);
            }
            checked += 1;
        }
    }
    assert!(checked > 2500, "{checked}");
    assert!(
        problems.is_empty(),
        "{} of {checked} disagree:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn damaged_volume_headers_btree_nodes_and_extents_are_refused_when_7zip_refuses_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut volume = small_volume();
    volume.fragment = true;
    let (image, layout) = hfs_writer::build(&volume);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("damaged.hfs");
    let node = layout.node_size;
    let mut targets: Vec<usize> = positions(layout.header..layout.header + 512, 1).collect();
    for start in [
        layout.catalog_file,
        layout.extents_file,
        layout.attributes_file,
    ] {
        targets.extend(positions(start..start + 140, 1));
        targets.extend(positions(start + node - 16..start + node, 1));
    }
    for &leaf in layout
        .catalog_leaves
        .iter()
        .chain(&layout.extents_leaves)
        .chain(&layout.attribute_leaves)
    {
        targets.extend(positions(leaf..leaf + 64, 1));
        targets.extend(positions(leaf + 64..leaf + node - 32, 41));
        targets.extend(positions(leaf + node - 32..leaf + node, 1));
    }
    let mut problems = Vec::new();
    let mut checked = 0;
    for at in targets {
        for flip in [0x01u8, 0x80] {
            let mut damaged = image.clone();
            damaged[at] ^= flip;
            fs::write(&path, &damaged).unwrap();
            if let Some(problem) = judge(&binary, &path, &format!("byte {at} ^ {flip:#x}")) {
                problems.push(problem);
            }
            checked += 1;
        }
    }
    assert!(checked > 2500, "{checked}");
    assert!(
        problems.is_empty(),
        "{} of {checked} disagree:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

fn refusal(image: &[u8]) -> String {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("hostile.dmg");
    fs::write(&path, image).unwrap();
    let out = work.path().join("out");
    let error = read_archive(&path, Some(&out), ReadOptions::default())
        .map(|_| tree(&out))
        .expect_err("a hostile image was extracted");
    assert!(!out.exists(), "published after a failure");
    format!("{error:#}")
}

fn extracted(image: &[u8]) -> Tree {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("image.dmg");
    fs::write(&path, image).unwrap();
    let out = work.path().join("out");
    read_archive(&path, Some(&out), ReadOptions::default())
        .unwrap_or_else(|error| panic!("{error:#}"));
    let found = tree(&out);
    hfs_writer_cleanup(work.path());
    found
}

fn be32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn record_body(image: &[u8], layout: &hfs_writer::Layout, name: &str) -> usize {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let start = layout.catalog_leaves[0];
    let end = *layout.catalog_leaves.last().unwrap() + layout.node_size;
    let found = image[start..end]
        .windows(units.len() + 2)
        .position(|window| {
            window[..2] == (units.len() as u16 / 2).to_be_bytes() && window[2..] == units[..]
        })
        .unwrap_or_else(|| panic!("no record named {name}"));
    let at = start + found + 2 + units.len();
    assert!(matches!(
        u16::from_be_bytes([image[at], image[at + 1]]),
        1 | 2
    ));
    at
}

#[test]
fn btree_leaf_loops_and_links_outside_their_file_are_refused() {
    let (image, layout) = hfs_writer::build(&small_volume());
    assert!(layout.catalog_leaves.len() >= 3);
    let total = (layout.catalog_leaves.len() + 1) as u32;
    let second = layout.catalog_leaves[1];
    for (next, expected) in [
        (2u32, "in a loop"),
        (1, "in a loop"),
        (total, "outside the file"),
        (u32::MAX, "outside the file"),
        (0x7FFF, "outside the file"),
    ] {
        let mut damaged = image.clone();
        damaged[second..second + 4].copy_from_slice(&next.to_be_bytes());
        let error = refusal(&damaged);
        assert!(error.contains(expected), "{next}: {error}");
    }
    let mut damaged = image.clone();
    damaged[second..second + 4].copy_from_slice(&0u32.to_be_bytes());
    let first = layout.catalog_file;
    let header_link = first + 14 + 0xA;
    damaged[header_link..header_link + 4].copy_from_slice(&0u32.to_be_bytes());
    let error = refusal(&damaged);
    assert!(error.contains("catalog holds 0"), "{error}");
    let mut damaged = image.clone();
    damaged[second + 8] = 0;
    let error = refusal(&damaged);
    assert!(error.contains("not a leaf"), "{error}");
}

#[test]
fn extents_that_share_blocks_or_leave_the_volume_are_refused() {
    let (image, layout) = hfs_writer::build(&small_volume());
    let hello = record_body(&image, &layout, "hello.txt");
    let data = record_body(&image, &layout, "data.bin");
    let hello_start = be32_at(&image, hello + 88 + 16);
    let mut damaged = image.clone();
    damaged[data + 88 + 16..data + 88 + 20].copy_from_slice(&hello_start.to_be_bytes());
    let error = refusal(&damaged);
    assert!(error.contains("share block"), "{error}");
    let blocks = be32_at(&image, 1024 + 0x2C);
    let mut damaged = image.clone();
    damaged[data + 88 + 16..data + 88 + 20].copy_from_slice(&(blocks - 1).to_be_bytes());
    let error = refusal(&damaged);
    assert!(error.contains("past the end of the volume"), "{error}");
    let mut damaged = image.clone();
    damaged[1024 + 0x2C..1024 + 0x30].copy_from_slice(&(blocks + 1).to_be_bytes());
    let error = refusal(&damaged);
    assert!(error.contains("more blocks than its image"), "{error}");
}

#[test]
fn data_larger_than_its_declared_size_is_refused() {
    let text = b"packed text ".repeat(200);
    let mut volume = small_volume();
    volume.attributes[0].value = AttributeValue::Inline(hfs_writer::decmpfs_header(
        7,
        text.len() as u64 - 1,
        &dmg_writer::lzvn(&text),
    ));
    let (image, _) = hfs_writer::build(&volume);
    assert!(refusal(&image).contains("past its declared size"));
    let mut volume = small_volume();
    let chunk = dmg_writer::lzvn(&[1u8; 70_000]);
    let mut packed = hfs_writer::file(40, ROOT, "big chunk", &[]);
    packed.kind = Kind::File {
        data: Vec::new(),
        resource: hfs_writer::lzvn_fork(&[chunk]),
    };
    volume.entries.push(packed);
    volume.attributes.push(Attribute {
        id: 40,
        name: "com.apple.decmpfs".into(),
        value: AttributeValue::Inline(hfs_writer::decmpfs_header(8, 65_536, &[])),
    });
    let (image, _) = hfs_writer::build(&volume);
    let error = refusal(&image);
    assert!(
        error.contains("larger than 64 KiB") || error.contains("past its declared size"),
        "{error}"
    );
    let mut volume = small_volume();
    volume.attributes[0].value = AttributeValue::Inline(hfs_writer::decmpfs_header(
        7,
        1 << 40,
        &dmg_writer::lzvn(&text),
    ));
    let (image, _) = hfs_writer::build(&volume);
    assert!(refusal(&image).contains("declares more bytes than its data can hold"));
    let (volume, _) = hfs_writer::build(&small_volume());
    let (image, layout, table, span) = main_table(&volume);
    let shrunk = resize_first_block(&table, |sectors| sectors - 1);
    let error = refusal(&with_table(&image, &layout, span.0, span.1, &shrunk));
    assert!(error.contains("damaged block"), "{error}");
}

#[test]
fn a_file_with_a_resource_fork_is_refused_with_its_name() {
    let mut volume = small_volume();
    volume.entries.push(Entry {
        kind: Kind::File {
            data: b"data".to_vec(),
            resource: b"resource".to_vec(),
        },
        ..hfs_writer::file(40, ROOT, "forked", b"")
    });
    let (image, _) = hfs_writer::build(&volume);
    let error = refusal(&image);
    assert!(
        error.contains("\"Small/forked\" has a resource fork"),
        "{error}"
    );
    assert!(error.contains("--zip-engine backend"), "{error}");
}

#[test]
fn apfs_images_go_to_7zip_and_are_refused_by_name_under_the_native_engine() {
    let archive = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/parity/archives/dmg-zlib.dmg"
    ));
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, archive).unwrap(),
        zipper::Reader::Backend { format: None }
    );
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Native, archive).unwrap(),
        zipper::Reader::Dmg
    );
    let work = tempfile::tempdir().unwrap();
    let error = read_archive(
        archive,
        Some(&work.path().join("out")),
        ReadOptions::default(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("APFS"), "{error:#}");
    assert!(!work.path().join("out").exists());
    let (volume, _) = hfs_writer::build(&small_volume());
    let (image, _) = dmg_of(&volume, &Options::default());
    let path = work.path().join("hfs.dmg");
    fs::write(&path, image).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
        zipper::Reader::Dmg
    );
}

#[test]
fn unsafe_names_and_links_never_escape_the_destination() {
    for (name, target) in [
        ("..", None),
        (".", None),
        ("a:b", None),
        ("up", Some("../../outside")),
        ("root", Some("/etc/passwd")),
    ] {
        let mut volume = small_volume();
        volume.entries.push(match target {
            Some(target) => hfs_writer::symlink(40, ROOT, name, target),
            None => hfs_writer::file(40, ROOT, name, b"x"),
        });
        let (image, _) = hfs_writer::build(&volume);
        let error = refusal(&image);
        assert!(
            error.contains("unsafe") || error.contains("outside") || error.contains("escapes"),
            "{name}: {error}"
        );
    }
    let mut volume = small_volume();
    volume
        .entries
        .push(hfs_writer::symlink(40, 16, "inside", "../hello.txt"));
    let (image, _) = hfs_writer::build(&volume);
    let found = extracted(&image);
    assert_eq!(
        found.get("Small/folder/inside").map(String::as_str),
        Some("link:../hello.txt")
    );
}

fn main_table(volume: &[u8]) -> (Vec<u8>, dmg_writer::Layout, Vec<u8>, (usize, usize)) {
    let options = Options {
        chunk_sectors: 16,
        ..Options::default()
    };
    let (image, layout) = dmg_of(volume, &options);
    let text =
        String::from_utf8(image[layout.xml.0..layout.xml.0 + layout.xml.1].to_vec()).unwrap();
    let first = text.find("<data>").unwrap() + 6;
    let start = first + text[first..].find("<data>").unwrap() + 6;
    let end = start + text[start..].find("</data>").unwrap();
    let digits: String = text[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let table = decode_base64(&digits);
    (image, layout, table, (start, end))
}

fn resize_first_block(table: &[u8], resize: impl Fn(u64) -> u64) -> Vec<u8> {
    let mut table = table.to_vec();
    let field = |table: &[u8], at: usize| u64::from_be_bytes(table[at..at + 8].try_into().unwrap());
    let first = 0xCC;
    let old = field(&table, first + 16);
    let new = resize(old);
    table[first + 16..first + 24].copy_from_slice(&new.to_be_bytes());
    for block in (first + 40..table.len()).step_by(40) {
        let start = field(&table, block + 8) + new - old;
        table[block + 8..block + 16].copy_from_slice(&start.to_be_bytes());
    }
    let sectors = field(&table, 0x10) + new - old;
    table[0x10..0x18].copy_from_slice(&sectors.to_be_bytes());
    table
}

#[test]
fn unknown_and_oversized_chunks_are_refused() {
    let (volume, _) = hfs_writer::build(&small_volume());
    let (image, layout, table, span) = main_table(&volume);
    assert_eq!(&table[0xCC..0xD0], &0x8000_0005u32.to_be_bytes());
    let mut unknown = table.clone();
    unknown[0xCC..0xD0].copy_from_slice(&0x8000_0009u32.to_be_bytes());
    let error = refusal(&with_table(&image, &layout, span.0, span.1, &unknown));
    assert!(error.contains("which 7-Zip does not decode"), "{error}");
    let huge = resize_first_block(&table, |_| (257u64 << 20) >> 9);
    let error = refusal(&with_table(&image, &layout, span.0, span.1, &huge));
    assert!(error.contains("larger than 256 MiB"), "{error}");
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("huge.dmg");
    fs::write(&path, with_table(&image, &layout, span.0, span.1, &huge)).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
        zipper::Reader::Backend { format: None }
    );
}

fn decode_base64(text: &str) -> Vec<u8> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let digits: Vec<u8> = text
        .bytes()
        .filter(|&byte| byte != b'=')
        .map(value)
        .collect();
    let mut out = Vec::new();
    for group in digits.chunks(4) {
        let mut bits = 0u32;
        for &digit in group {
            bits = bits << 6 | u32::from(digit);
        }
        bits <<= 6 * (4 - group.len() as u32);
        out.extend(&bits.to_be_bytes()[1..group.len()]);
    }
    out
}

fn encode_base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let value = group
            .iter()
            .fold(0u32, |value, &byte| value << 8 | u32::from(byte))
            << (8 * (3 - group.len()));
        for digit in 0..4 {
            out.push(if digit <= group.len() {
                DIGITS[(value >> (18 - 6 * digit) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

fn with_table(
    image: &[u8],
    layout: &dmg_writer::Layout,
    start: usize,
    end: usize,
    table: &[u8],
) -> Vec<u8> {
    let xml_at = layout.xml.0;
    let mut xml = image[xml_at..xml_at + layout.xml.1].to_vec();
    xml.splice(start..end, encode_base64(table).into_bytes());
    let mut out = image[..xml_at].to_vec();
    out.extend(&xml);
    let mut koly = image[layout.koly..layout.koly + 512].to_vec();
    koly[0xE0..0xE8].copy_from_slice(&(xml.len() as u64).to_be_bytes());
    koly[0x160..0x168].fill(0);
    out.extend(koly);
    out
}

#[test]
fn folders_that_nest_too_deep_loop_or_repeat_a_name_are_refused() {
    let mut volume = small_volume();
    let mut parent = ROOT;
    for depth in 0..1030 {
        volume
            .entries
            .push(hfs_writer::folder(100 + depth, parent, "d"));
        parent = 100 + depth;
    }
    let (image, _) = hfs_writer::build(&volume);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("deep.hfs");
    fs::write(&path, &image).unwrap();
    let error = format!(
        "{:#}",
        read_archive(&path, None, ReadOptions::default()).unwrap_err()
    );
    assert!(error.contains("deeper than 1024"), "{error}");
    assert!(refusal(&image).contains("too long") || error.contains("deeper"));
    let mut volume = small_volume();
    volume.entries.push(hfs_writer::folder(40, 41, "a"));
    volume.entries.push(hfs_writer::folder(41, 40, "b"));
    volume.counts = None;
    let (image, _) = hfs_writer::build(&volume);
    assert!(refusal(&image).contains("form a loop"));
    let mut volume = small_volume();
    volume
        .entries
        .push(hfs_writer::file(40, ROOT, "twice", b"1"));
    volume
        .entries
        .push(hfs_writer::file(41, ROOT, "twice", b"2"));
    let (image, _) = hfs_writer::build(&volume);
    assert!(refusal(&image).contains("holds two entries named"));
    let mut volume = small_volume();
    volume
        .entries
        .push(hfs_writer::file(17, 16, "same id", b"1"));
    let (image, _) = hfs_writer::build(&volume);
    assert!(refusal(&image).contains("two entries numbered 17"));
}

#[test]
fn partitions_and_trailers_whose_checksums_fail_are_refused() {
    let (volume, _) = hfs_writer::build(&small_volume());
    let options = Options {
        chunk_sectors: 16,
        methods: vec![Chunk::Raw],
        ..Options::default()
    };
    let (image, layout) = dmg_of(&volume, &options);
    extracted(&image);
    let hello = image
        .windows(13)
        .position(|window| window == b"hello, world\n")
        .unwrap();
    let mut damaged = image.clone();
    damaged[hello] = b'j';
    let error = refusal(&damaged);
    assert!(error.contains("fails its CRC-32 check"), "{error}");
    let mut damaged = image.clone();
    damaged[layout.koly + 0x168] ^= 1;
    let error = refusal(&damaged);
    assert!(error.contains("master checksum"), "{error}");
    let options = Options {
        data_checksum: true,
        ..options
    };
    let (image, _) = dmg_of(&volume, &options);
    let mut damaged = image.clone();
    damaged[hello] = b'j';
    let error = refusal(&damaged);
    assert!(
        error.contains("data fork fails its CRC-32 check"),
        "{error}"
    );
}

fn apm_with_partition(partition: &[u8]) -> Vec<u8> {
    let mut disk = vec![0u8; 4 * 512];
    disk[..2].copy_from_slice(b"ER");
    disk[2..4].copy_from_slice(&512u16.to_be_bytes());
    let entries = [
        ("Apple", "Apple_partition_map", 1u32, 3u32),
        ("disk", "Apple_HFS", 4, (partition.len() / 512) as u32),
    ];
    for (index, (name, kind, first, count)) in entries.iter().enumerate() {
        let sector = &mut disk[(index + 1) * 512..(index + 2) * 512];
        sector[..2].copy_from_slice(b"PM");
        sector[4..8].copy_from_slice(&2u32.to_be_bytes());
        sector[8..12].copy_from_slice(&first.to_be_bytes());
        sector[12..16].copy_from_slice(&count.to_be_bytes());
        sector[16..16 + name.len()].copy_from_slice(name.as_bytes());
        sector[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        sector[0x58..0x5C].copy_from_slice(&0x33u32.to_be_bytes());
    }
    disk.extend(partition);
    disk
}

#[test]
fn a_gpt_with_4_kib_sectors_in_a_short_partition_is_refused_without_a_panic() {
    for length in [4608usize, 5120, 5632, 8192, 10240] {
        let mut partition = vec![0u8; length];
        partition[0x1FE] = 0x55;
        partition[0x1FF] = 0xAA;
        partition[4096..4104].copy_from_slice(b"EFI PART");
        let error = refusal(&apm_with_partition(&partition));
        assert!(error.contains("GPT"), "{length}: {error}");
    }
}

#[test]
fn a_gpt_table_larger_than_its_disk_is_refused_before_it_is_read() {
    let mut partition = vec![0u8; 4096];
    partition[0x1FE] = 0x55;
    partition[0x1FF] = 0xAA;
    let header = &mut partition[512..1024];
    header[..8].copy_from_slice(b"EFI PART");
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[0x18..0x20].copy_from_slice(&1u64.to_le_bytes());
    header[0x48..0x50].copy_from_slice(&2u64.to_le_bytes());
    header[0x50..0x54].copy_from_slice(&65_536u32.to_le_bytes());
    header[0x54..0x58].copy_from_slice(&4096u32.to_le_bytes());
    let crc = crc32fast::hash(&header[..92]);
    header[0x10..0x14].copy_from_slice(&crc.to_le_bytes());
    let error = refusal(&apm_with_partition(&partition));
    assert!(error.contains("GPT table lies past"), "{error}");
}

#[test]
fn every_byte_of_a_resource_fork_list_and_trailer_set_to_any_value_never_panics() {
    let (volume, _) = hfs_writer::build(&small_volume());
    let options = Options {
        chunk_sectors: 64,
        form: Form::ResourceFork,
        ..Options::default()
    };
    let (image, layout) = dmg_of(&volume, &options);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("fork.dmg");
    let (fork, length) = layout.resource;
    let mut tried = 0;
    for at in (fork..fork + length).chain(layout.koly..layout.koly + 512) {
        for value in [0x00u8, 0x01, 0x7F, 0x80, 0xFF, image[at] ^ 0x10] {
            let mut damaged = image.clone();
            damaged[at] = value;
            fs::write(&path, &damaged).unwrap();
            let _ = read_archive(&path, None, ReadOptions::default());
            tried += 1;
        }
    }
    assert!(tried > 3000, "{tried}");
}
