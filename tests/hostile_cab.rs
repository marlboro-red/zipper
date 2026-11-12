//! The CAB reader extracts stored, MSZIP and Quantum folders byte for byte,
//! refuses LZX folders by name,
//! rejects each hostile layout with nothing left behind, and honours the
//! options the other readers take.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/quantum_encoder.rs"]
mod quantum_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use cab_writer::{Block, Cabinet, Method};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zipper::{
    Control, Existing, Metadata, Strip,
    native_cab::{ReadOptions, read_archive},
};

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
        .filter(|path| path != "case.cab")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.cab");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile CAB"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile CAB"
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
    let archive = root.path().join("case.cab");
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

fn text(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|line| format!("line {line} {}\n", line * 7919 % 1000).into_bytes())
        .collect()
}

fn methods() -> Vec<(&'static str, Method)> {
    let mut quantum = quantum_encoder::Options::plain(16);
    quantum.literal_odds = 5;
    quantum.shorten_odds = 20;
    vec![
        ("stored", Method::Stored),
        ("mszip", Method::Mszip),
        ("quantum", Method::Quantum(quantum)),
    ]
}

fn small(method: &Method) -> Cabinet {
    cab_writer::one_folder_in_order(
        &[
            ("a.txt", b"hello, cabinet\r\n"),
            ("dir/b.txt", b"second file\r\n"),
        ],
        method,
    )
}

#[test]
fn every_method_extracts_byte_for_byte_and_verifies_without_a_destination() {
    let large = text(20_000);
    let files: [(&str, &[u8]); 4] = [
        ("first.txt", b"first"),
        ("docs/large.txt", &large),
        ("docs/empty", b""),
        ("last.bin", &[0xE8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
    ];
    for (label, method) in methods() {
        let bytes = cab_writer::one_folder_in_order(&files, &method).build();
        let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
        let entries = result.unwrap_or_else(|error| panic!("{label}: {error:#}"));
        assert_eq!(tree(&destination), expected(&files), "{label}");
        assert_eq!(entries.len(), files.len(), "{label}");
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("verify.cab");
        fs::write(&archive, &bytes).unwrap();
        assert_eq!(
            read_archive(&archive, None, ReadOptions::default())
                .unwrap()
                .len(),
            files.len(),
            "{label}"
        );
    }
}

#[test]
fn several_folders_reserves_directories_and_trailing_data_extract() {
    let large = text(9000);
    let mut cabinet = cab_writer::one_folder_in_order(
        &[("one/a.txt", b"aaa"), ("one/b.txt", &large)],
        &Method::Mszip,
    );
    let second = cab_writer::one_folder_in_order(
        &[("two/c.txt", &large)],
        &Method::Quantum(quantum_encoder::Options::plain(21)),
    );
    cabinet.folders.extend(second.folders);
    cabinet
        .files
        .extend(second.files.into_iter().map(|mut file| {
            file.folder = 1;
            file
        }));
    cabinet.files.push(cab_writer::File {
        attributes: cab_writer::DIRECTORY,
        ..cab_writer::file("made/by/record", 0, 0, 0)
    });
    cabinet.reserve = Some((vec![0xAA; 300], vec![0xBB; 2], vec![0xCC; 7]));
    cabinet.trailing = b"signature bytes after the cabinet".to_vec();
    let (_root, destination, result) = extract_with(&cabinet.build(), ReadOptions::default());
    result.unwrap();
    let mut wanted = expected(&[
        ("one/a.txt", b"aaa"),
        ("one/b.txt", &large),
        ("two/c.txt", &large),
    ]);
    for folder in ["made", "made/by", "made/by/record"] {
        wanted.insert(folder.to_owned(), b"<dir>".to_vec());
    }
    assert_eq!(tree(&destination), wanted);
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        &b"../escape.txt"[..],
        b"..\\escape.txt",
        b"dir\\..\\..\\escape.txt",
        b"/absolute.txt",
        b"\\absolute.txt",
        b"C:\\drive.txt",
        b"C:drive.txt",
        b"",
        b"dir\\",
        b"a\\\\b",
        b"a\\.\\b",
    ] {
        let mut cabinet = small(&Method::Stored);
        cabinet.files[0].name = name.to_vec();
        assert_rejected_without_leftovers(
            &format!("{:?}", String::from_utf8_lossy(name)),
            &cabinet.build(),
        );
    }
}

#[test]
fn names_that_are_not_utf8_or_not_ended_or_too_long_are_refused() {
    let mut latin = small(&Method::Stored);
    latin.files[0].name = b"caf\xE9.txt".to_vec();
    assert_rejected_with("latin-1 name", &latin.build(), "not ASCII or UTF-8");
    latin.files[0].attributes |= cab_writer::NAME_IS_UTF8;
    assert_rejected_with("flagged latin-1 name", &latin.build(), "not valid UTF-8");
    let mut long = small(&Method::Stored);
    long.files[0].name = vec![b'a'; 8193];
    assert_rejected_with("long name", &long.build(), "longer than 8192 bytes");
    long.files[0].name = vec![b'a'; 8192];
    let (_root, _destination, result) = extract_with(&long.build(), ReadOptions::default());
    assert!(
        result.is_ok() || !format!("{:#}", result.unwrap_err()).contains("8192"),
        "an 8192-byte name passes the CAB name limit"
    );
    let built = cab_writer::one_folder_in_order(&[("a.txt", b"one")], &Method::Stored).build();
    let name_end = built.windows(6).position(|w| w == b"a.txt\0").unwrap() + 5;
    let mut unterminated = built[..name_end].to_vec();
    unterminated[8..12].copy_from_slice(&(name_end as u32).to_le_bytes());
    assert_rejected_with("unterminated name", &unterminated, "truncated CAB name");
}

#[test]
fn repeated_names_follow_the_last_and_shared_or_overlapping_data_is_refused() {
    let mut repeated = cab_writer::one_folder_in_order(
        &[("same.txt", b"first"), ("same.txt", b"second")],
        &Method::Stored,
    );
    let (_root, destination, result) = extract_with(&repeated.build(), ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination), expected(&[("same.txt", b"second")]));

    repeated.files.push(repeated.files[1].clone());
    let (_root, destination, result) = extract_with(&repeated.build(), ReadOptions::default());
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("same.txt", b"second")]),
        "an exact repeat is dropped"
    );

    let mut shared = small(&Method::Stored);
    shared.files[1].offset = shared.files[0].offset;
    shared.files[1].size = shared.files[0].size;
    assert_rejected_with("shared range", &shared.build(), "share bytes");
    let mut overlap = small(&Method::Stored);
    overlap.files[1].offset = 3;
    assert_rejected_with("overlap", &overlap.build(), "share bytes");

    let file_and_folder =
        cab_writer::one_folder_in_order(&[("a", b"file"), ("a/b", b"inside")], &Method::Stored);
    assert_rejected_without_leftovers("file then folder", &file_and_folder.build());
    let mut kinds = small(&Method::Stored);
    kinds.files.push(cab_writer::File {
        attributes: cab_writer::DIRECTORY,
        ..cab_writer::file("a.txt", 0, 0, 0)
    });
    assert_rejected_without_leftovers("folder and file of one name", &kinds.build());
    let mut with_data = small(&Method::Stored);
    with_data.files[1].attributes = cab_writer::DIRECTORY;
    assert_rejected_with("folder entry with data", &with_data.build(), "carries data");
}

#[test]
fn every_cut_before_the_cabinet_end_is_refused_and_bytes_after_it_are_ignored() {
    for (label, method) in methods() {
        let bytes = small(&method).build();
        for length in 0..bytes.len() {
            assert_rejected_without_leftovers(
                &format!("{label} cut to {length}"),
                &bytes[..length],
            );
        }
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[0xFF; 9]);
        let (_root, _destination, result) = extract_with(&longer, ReadOptions::default());
        result.unwrap();
    }
}

#[test]
fn every_damaged_data_byte_is_refused_and_no_damaged_byte_panics() {
    for (label, method) in methods() {
        let cabinet = small(&method);
        let bytes = cabinet.build();
        let data_start = bytes.len()
            - cabinet.folders[0]
                .blocks
                .iter()
                .map(|b| b.packed.len() + 8)
                .sum::<usize>();
        for index in 0..bytes.len() {
            for bit in [0, 3, 7] {
                let mut damaged = bytes.clone();
                damaged[index] ^= 1 << bit;
                let root = tempfile::tempdir().unwrap();
                let archive = root.path().join("case.cab");
                fs::write(&archive, &damaged).unwrap();
                let result = read_archive(
                    &archive,
                    Some(&root.path().join("out")),
                    ReadOptions::default(),
                );
                if index >= data_start {
                    assert!(
                        result.is_err(),
                        "{label}: damage at {index} bit {bit} passed"
                    );
                }
            }
        }
        let mut unchecked = cabinet.clone();
        unchecked.checksums = false;
        let (_root, destination, result) = extract_with(&unchecked.build(), ReadOptions::default());
        result.unwrap();
        assert_eq!(
            tree(&destination).len(),
            3,
            "{label}: a zero checksum is not checked"
        );
    }
}

#[test]
fn layouts_that_lie_are_refused() {
    let base = small(&Method::Stored);
    let build = |change: &dyn Fn(&mut Cabinet)| {
        let mut cabinet = base.clone();
        change(&mut cabinet);
        cabinet.build()
    };
    let patched = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = base.build();
        change(&mut bytes);
        bytes
    };
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "missing folder",
            build(&|c| c.files[1].folder = 1),
            "names folder 1",
        ),
        (
            "continued folder",
            build(&|c| c.files[0].folder = 0xFFFD),
            "continues from a previous cabinet",
        ),
        (
            "continued into a next cabinet",
            build(&|c| c.files[0].folder = 0xFFFE),
            "continues into a next cabinet",
        ),
        (
            "previous cabinet",
            build(&|c| c.previous = Some(("a.cab".into(), "disk".into()))),
            "open the set's first cabinet",
        ),
        (
            "missing next cabinet",
            build(&|c| c.next = Some(("b.cab".into(), "disk".into()))),
            "cannot open cabinet b.cab",
        ),
        (
            "file past its folder",
            build(&|c| c.files[1].size += 1),
            "truncated",
        ),
        (
            "file far past its folder",
            build(&|c| c.files[1].offset = u32::MAX),
            "truncated",
        ),
        ("huge file", build(&|c| c.files[1].size = u32::MAX), ""),
        (
            "no blocks",
            build(&|c| c.folders[0].declared_blocks = Some(0)),
            "truncated",
        ),
        (
            "unknown method",
            build(&|c| c.folders[0].type_compress = 4),
            "unknown compression method",
        ),
        (
            "Quantum window 22",
            build(&|c| c.folders[0].type_compress = 2 | (4 << 4) | (22 << 8)),
            "CAB Quantum window",
        ),
        (
            "Quantum window 255",
            build(&|c| c.folders[0].type_compress = 2 | (255 << 8)),
            "CAB Quantum window",
        ),
        (
            "LZX window 22",
            build(&|c| c.folders[0].type_compress = 3 | (22 << 8)),
            "compressed with LZX",
        ),
        (
            "LZX window 14",
            build(&|c| c.folders[0].type_compress = 3 | (14 << 8)),
            "compressed with LZX",
        ),
        (
            "stored size mismatch",
            build(&|c| c.folders[0].blocks[0].size -= 1),
            "packs",
        ),
        (
            "empty block",
            build(&|c| c.folders[0].blocks[0].size = 0),
            "another cabinet",
        ),
        (
            "block over 32 KiB",
            build(&|c| c.folders[0].blocks[0].size = 32769),
            "32 KiB",
        ),
        ("unknown flag", patched(&|b| b[30] |= 8), "unknown flags"),
        ("reserved field", patched(&|b| b[12] = 1), "reserved fields"),
        (
            "cabinet larger than the file",
            patched(&|b| b[8] += 1),
            "truncated CAB file",
        ),
        (
            "cabinet smaller than a header",
            patched(&|b| b[8..12].copy_from_slice(&35u32.to_le_bytes())),
            "smaller than its header",
        ),
        (
            "files outside the cabinet",
            patched(&|b| b[16..20].copy_from_slice(&u32::MAX.to_le_bytes())),
            "outside the cabinet",
        ),
        (
            "too many folders",
            patched(&|b| b[26..28].copy_from_slice(&u16::MAX.to_le_bytes())),
            "folder table",
        ),
        (
            "too many files",
            patched(&|b| b[28..30].copy_from_slice(&u16::MAX.to_le_bytes())),
            "file table",
        ),
        (
            "bad checksum",
            patched(&|b| {
                let at = b.len() - 2;
                b[at] ^= 1
            }),
            "checksum",
        ),
    ];
    for (case, bytes, words) in cases {
        assert_rejected_with(case, &bytes, words);
    }
    let short_first = Cabinet {
        folders: vec![cab_writer::Folder {
            type_compress: 0,
            blocks: vec![
                Block {
                    packed: b"short".to_vec(),
                    size: 5,
                },
                Block {
                    packed: b"later".to_vec(),
                    size: 5,
                },
            ],
            declared_blocks: None,
        }],
        files: vec![cab_writer::file("a", 0, 0, 10)],
        checksums: true,
        ..Cabinet::default()
    };
    assert_rejected_with(
        "short block first",
        &short_first.build(),
        "shorter than 32 KiB",
    );
}

#[test]
fn mszip_blocks_must_be_whole_deflate_streams() {
    let base = small(&Method::Mszip);
    let mut no_signature = base.clone();
    no_signature.folders[0].blocks[0].packed[0] = b'X';
    assert_rejected_with("no CK", &no_signature.build(), "CK");
    let mut trailing = base.clone();
    trailing.folders[0].blocks[0]
        .packed
        .extend_from_slice(&[0, 0]);
    assert_rejected_with("trailing bytes", &trailing.build(), "MSZIP");
    let mut unfinished = base.clone();
    let mut compress = flate2::Compress::new(flate2::Compression::new(6), false);
    let mut packed = vec![0; 200];
    compress
        .compress(
            b"hello, cabinet\r\nsecond file\r\n",
            &mut packed,
            flate2::FlushCompress::Sync,
        )
        .unwrap();
    packed.truncate(compress.total_out() as usize);
    packed.splice(0..0, *b"CK");
    unfinished.folders[0].blocks[0].packed = packed;
    assert_rejected_with("no final block", &unfinished.build(), "MSZIP");
    let mut longer = base.clone();
    longer.folders[0].blocks[0].size -= 1;
    assert_rejected_with("decodes past its size", &longer.build(), "MSZIP");
}

#[test]
fn lzx_folders_are_refused_by_name_with_nothing_written() {
    let large = text(6000);
    for window_bits in [15, 21] {
        let lone =
            cab_writer::one_folder_in_order(&[("a", &large)], &Method::LzxLabel { window_bits });
        assert_rejected_with("one LZX folder", &lone.build(), "compressed with LZX");
    }
    let mut mixed = small(&Method::Mszip);
    let second = small(&Method::LzxLabel { window_bits: 16 });
    mixed.folders.extend(second.folders);
    mixed.files.extend(second.files.into_iter().map(|mut file| {
        file.folder = 1;
        file.name.insert(0, b'z');
        file
    }));
    assert_rejected_with(
        "an MSZIP folder before an LZX one",
        &mixed.build(),
        "compressed with LZX",
    );
}

fn quantum_cab(window_bits: u8, chunks: &[Vec<quantum_encoder::Token>]) -> Cabinet {
    let blocks: Vec<Block> = quantum_encoder::encode_tokens(window_bits, chunks)
        .into_iter()
        .map(|chunk| Block {
            packed: chunk.bytes,
            size: chunk.size,
        })
        .collect();
    let size = blocks.iter().map(|block| block.size as u32).sum();
    Cabinet {
        folders: vec![cab_writer::Folder {
            type_compress: 2 | (4 << 4) | (u16::from(window_bits) << 8),
            blocks,
            declared_blocks: None,
        }],
        files: vec![cab_writer::file("a", 0, 0, size)],
        checksums: true,
        ..Cabinet::default()
    }
}

#[test]
fn quantum_windows_below_2_10_extract_as_7zip_reads_them() {
    let data = text(3000);
    for window_bits in [0u8, 1, 5, 9] {
        let method = Method::Quantum(quantum_encoder::Options::plain(window_bits));
        let bytes = cab_writer::one_folder_in_order(&[("a.txt", &data)], &method).build();
        let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("window {window_bits}: {error:#}"));
        assert_eq!(fs::read(destination.join("a.txt")).unwrap(), data);
    }
}

#[test]
fn quantum_streams_that_lie_are_refused() {
    use quantum_encoder::Token::{Literal, Match};
    let literals = vec![Literal(b'q'); cab_writer::BLOCK];
    let reach = |length, distance| Match { length, distance };
    let before_start = quantum_cab(16, &[vec![Literal(1), reach(3, 2)]]);
    assert_rejected_with(
        "match before the start",
        &before_start.build(),
        "before the start",
    );
    let across = quantum_cab(
        16,
        &[literals.clone(), vec![reach(259, cab_writer::BLOCK + 1)]],
    );
    assert_rejected_with(
        "match before the start of a second block",
        &across.build(),
        "before the start",
    );
    let fine = quantum_cab(16, &[literals.clone(), vec![reach(259, cab_writer::BLOCK)]]);
    let (_root, destination, result) = extract_with(&fine.build(), ReadOptions::default());
    result.unwrap();
    assert_eq!(
        fs::read(destination.join("a")).unwrap(),
        vec![b'q'; cab_writer::BLOCK + 259]
    );
    let mut past_block = quantum_cab(16, &[vec![Literal(1), reach(5, 1)]]);
    past_block.folders[0].blocks[0].size = 4;
    past_block.files[0].size = 4;
    assert_rejected_with(
        "match past its block",
        &past_block.build(),
        "past the end of its block",
    );
    let mut padded = quantum_cab(16, &[vec![Literal(1); 9]]);
    padded.folders[0].blocks[0].packed.push(0);
    assert_rejected_with("byte after a block", &padded.build(), "after its data");
    let mut marked = quantum_cab(16, &[vec![Literal(1); 9]]);
    *marked.folders[0].blocks[0].packed.last_mut().unwrap() |= 1;
    assert_rejected_with("padding that is not zero", &marked.build(), "not zero");
    let mut one_byte = quantum_cab(16, &[vec![Literal(1)]]);
    one_byte.folders[0].blocks[0].packed.truncate(1);
    assert_rejected_with("one-byte block", &one_byte.build(), "truncated Quantum");
    let mut smaller_window = quantum_cab(21, &[literals, vec![reach(259, 40_000)]]);
    smaller_window.folders[0].type_compress = 2 | (15 << 8);
    assert_rejected_without_leftovers("window smaller than its matches", &smaller_window.build());
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let large = text(5000);
    let bytes =
        cab_writer::one_folder_in_order(&[("a", b"small"), ("b", &large)], &Method::Mszip).build();
    let (root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(large.len() as u64),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
    drop(root);
}

#[test]
fn read_only_attributes_times_and_options_apply() {
    let mut cabinet = cab_writer::one_folder_in_order(
        &[
            ("keep/a.txt", b"a"),
            ("keep/b.txt", b"b"),
            ("drop/c.txt", b"c"),
        ],
        &Method::Mszip,
    );
    cabinet.files[0].attributes |= cab_writer::READ_ONLY;
    let bytes = cabinet.build();
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    let modified = fs::metadata(destination.join("keep/b.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        (1_749_900_000..1_750_100_000).contains(&seconds),
        "DOS time 2025-06-15 13:37:42 local, got {seconds}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |name: &str| {
            fs::metadata(destination.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("keep/a.txt") & 0o222, 0, "read-only stays unwritable");
        assert_ne!(mode("keep/b.txt") & 0o200, 0);
    }
    let selected = ["keep".to_owned()];
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
        expected(&[("keep/a.txt", b"a"), ("keep/b.txt", b"b")])
    );
    let exclude = ["*.txt".to_owned()];
    let include = ["c.txt".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            include: &include,
            exclude: &exclude[..0],
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(tree(&destination), expected(&[("drop/c.txt", b"c")]));
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: Strip::Components(1),
            metadata: Metadata::ContentOnly,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        expected(&[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")])
    );
    let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    let archive = root.path().join("case.cab");
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
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            control: Control {
                cancel: Some(&cancel),
                ..Control::default()
            },
            ..ReadOptions::default()
        },
    );
    assert!(result.is_err());
    assert!(!destination.exists());
    cancel.store(false, Ordering::Relaxed);
}

#[test]
fn cabs_and_cabinet_sets_read_in_process() {
    let root = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: Vec<u8>| {
        let path = root.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    };
    for (label, method) in methods() {
        let path = write(&format!("{label}.cab"), small(&method).build());
        assert_eq!(
            zipper::choose_reader(&path).unwrap(),
            zipper::Reader::Cab,
            "{label}"
        );
    }
    let mut set = small(&Method::Stored);
    set.next = Some(("two.cab".into(), "disk 2".into()));
    let path = write("set.cab", set.build());
    assert_eq!(zipper::choose_reader(&path).unwrap(), zipper::Reader::Cab);
    assert_eq!(zipper::choose_reader(&path).unwrap(), zipper::Reader::Cab);
    let broken = write("broken.cab", b"MSCF\0\0\0\0garbage".to_vec());
    assert_eq!(zipper::choose_reader(&broken).unwrap(), zipper::Reader::Cab);
}

#[test]
fn a_cab_of_many_empty_files_and_folders_stays_within_its_limits() {
    let mut cabinet = Cabinet {
        checksums: true,
        ..Cabinet::default()
    };
    for _ in 0..2000 {
        cabinet.folders.push(cab_writer::Folder {
            type_compress: 2 | (4 << 4) | (21 << 8),
            blocks: Vec::new(),
            declared_blocks: None,
        });
    }
    for index in 0..u16::MAX as u32 {
        cabinet.files.push(cab_writer::file(
            &format!("f{index}"),
            (index % 2000) as u16,
            0,
            0,
        ));
    }
    let (_root, destination, result) = extract_with(&cabinet.build(), ReadOptions::default());
    assert_eq!(result.unwrap().len(), u16::MAX as usize);
    assert_eq!(
        fs::read_dir(destination).unwrap().count(),
        u16::MAX as usize
    );
}

#[test]
fn folders_that_share_data_blocks_are_refused_before_their_blocks_are_decoded_twice() {
    let large = text(4000);
    let mut cabinet = cab_writer::one_folder_in_order(&[("a.txt", &large)], &Method::Mszip);
    for folder in 1..4 {
        cabinet.folders.push(cab_writer::Folder {
            type_compress: 0,
            blocks: Vec::new(),
            declared_blocks: None,
        });
        cabinet
            .files
            .push(cab_writer::file(&format!("{folder}.txt"), folder, 0, 10));
    }
    let mut bytes = cabinet.build();
    let first = bytes[36..44].to_vec();
    for folder in 1..4 {
        bytes[36 + 8 * folder..44 + 8 * folder].copy_from_slice(&first);
    }
    assert_rejected_with("shared blocks", &bytes, "share data blocks");
}

fn sevenzip_tree(bytes: &[u8]) -> Option<Option<BTreeMap<String, Vec<u8>>>> {
    let binary = sevenzip::sevenzip()?;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.cab");
    fs::write(&archive, bytes).unwrap();
    let into = root.path().join("out");
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(&into);
    let run = std::process::Command::new(binary)
        .args(["x", "-y", "-tcab"])
        .arg(flag)
        .arg(&archive)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    Some(run.status.success().then(|| tree(&into)))
}

fn zipper_tree(bytes: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let (_root, destination, result) = extract_with(bytes, ReadOptions::default());
    result.ok().map(|_| tree(&destination))
}

fn assert_matches_7zip(case: &str, bytes: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let ours = zipper_tree(bytes);
    if let Some(theirs) = sevenzip_tree(bytes) {
        assert_eq!(ours, theirs, "{case}: Zipper and 7-Zip disagree");
    }
    ours
}

#[test]
fn folders_that_share_one_block_are_refused_even_when_the_cabinet_has_spare_bytes() {
    let data = text(3000);
    let block = &data[..20_000];
    let mut cabinet = cab_writer::one_folder_in_order(&[("a.txt", block)], &Method::Stored);
    cabinet.folders.push(cab_writer::Folder {
        type_compress: 0,
        blocks: Vec::new(),
        declared_blocks: None,
    });
    cabinet.files.push(cab_writer::file("b.txt", 1, 0, 20_000));
    cabinet.reserve = Some((vec![0; 30_000], Vec::new(), Vec::new()));
    let mut bytes = cabinet.build();
    let table = 36 + 4 + 30_000;
    let first = bytes[table..table + 8].to_vec();
    bytes[table + 8..table + 16].copy_from_slice(&first);
    assert_rejected_with("one block in two folders", &bytes, "share data blocks");
}

#[test]
fn a_folder_is_decoded_only_as_far_as_its_last_file_and_every_block_read_is_checked() {
    let mut extra = small(&Method::Stored);
    extra.folders[0].declared_blocks = Some(2);
    let ours = assert_matches_7zip("a declared block past the last file", &extra.build());
    assert!(ours.is_some(), "a missing block no file needs is not read");
    let data = text(9000);
    let mut tail = cab_writer::one_folder_in_order(&[("a.txt", &data)], &Method::Mszip);
    assert!(tail.folders[0].blocks.len() > 2);
    tail.files[0].size = 1000;
    let last = tail.folders[0].blocks.len() - 1;
    tail.folders[0].blocks[last].packed[3] ^= 0xFF;
    let mut unchecked = tail.clone();
    unchecked.checksums = false;
    for (label, cabinet) in [("checked", &tail), ("unchecked", &unchecked)] {
        let ours = assert_matches_7zip(
            &format!("damage past the last file, {label}"),
            &cabinet.build(),
        );
        assert_eq!(ours, Some(expected(&[("a.txt", &data[..1000])])), "{label}");
    }
    let mut needed = tail.clone();
    needed.folders[0].blocks[0].packed[3] ^= 0xFF;
    assert_rejected_with("damage inside the last file", &needed.build(), "");
    let mut empty = cab_writer::one_folder_in_order(&[("a.txt", &data)], &Method::Mszip);
    empty.files.clear();
    empty.folders[0].blocks[0].packed[3] ^= 0xFF;
    let (_root, _destination, result) = extract_with(&empty.build(), ReadOptions::default());
    assert!(
        result.unwrap().is_empty(),
        "a folder no file needs is not decoded"
    );
}

#[test]
fn cabinet_sets_whose_names_leave_their_folder_are_refused() {
    for name in [
        "../other.cab",
        "..\\other.cab",
        "dir/other.cab",
        "C:other.cab",
        "..",
        "",
    ] {
        for previous in [false, true] {
            let mut set = small(&Method::Stored);
            let names = Some((name.to_owned(), "disk".to_owned()));
            if previous {
                set.previous = names;
            } else {
                set.next = names;
            }
            let bytes = set.build();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("set.cab");
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                zipper::choose_reader(&path).unwrap(),
                zipper::Reader::Cab,
                "{name:?} stays with the native reader"
            );
            assert_rejected_with(&format!("{name:?}"), &bytes, "not a plain file name");
        }
    }
    let mut disk = small(&Method::Stored);
    disk.next = Some(("two.cab".to_owned(), "../disk".to_owned()));
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("set.cab");
    fs::write(&path, disk.build()).unwrap();
    let error = read_archive(&path, None, ReadOptions::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot open cabinet two.cab"),
        "a disk label is not a path: {error:#}"
    );
}
