//! The LZH reader rejects each hostile layout with nothing left behind,
//! restores modes, times and links, and honours the options the other
//! readers take.
#[path = "common/lzh_encoder.rs"]
mod lzh_encoder;
#[path = "common/lzh_writer.rs"]
mod lzh_writer;

use lzh_encoder::{Random, Token};
use lzh_writer::Entry;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zipper::{
    Control, Existing, Metadata, Strip,
    native_lzh::{ReadOptions, read_archive},
};

fn leftovers(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path().strip_prefix(root).unwrap();
            path.display().to_string()
        })
        .filter(|path| path != "case.lzh")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.lzh");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile LZH"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile LZH"
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
    let archive = root.path().join("case.lzh");
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
            let contents = if entry.file_type().is_symlink() {
                format!("-> {}", fs::read_link(entry.path()).unwrap().display()).into_bytes()
            } else if entry.file_type().is_dir() {
                b"<dir>".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, contents)
        })
        .collect()
}

fn text(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|line| format!("line {line} {}\n", line * 7919 % 1000).into_bytes())
        .collect()
}

fn every_kind(level: u8) -> Vec<Entry> {
    let data = text(800);
    vec![
        Entry::directory(level, "folder"),
        Entry::stored(level, "folder/stored.txt", b"stored bytes"),
        Entry::compressed(level, "folder/five.txt", &data, lzh_writer::method(b'5'), 1),
        Entry::compressed(level, "six.txt", &data, lzh_writer::method(b'6'), 2),
        Entry::compressed(level, "seven.txt", &data, lzh_writer::method(b'7'), 3),
        Entry::compressed(level, "four.txt", &data, lzh_writer::method(b'4'), 4),
        Entry::stored(level, "empty", b""),
    ]
}

#[test]
fn every_level_and_method_extracts_byte_for_byte_and_verifies_without_a_destination() {
    let data = text(800);
    for level in 0..=3 {
        let bytes = lzh_writer::archive(&every_kind(level));
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        assert_eq!(result.unwrap().len(), 7, "level {level}");
        let found = tree(&destination);
        assert_eq!(found["folder"], b"<dir>");
        assert_eq!(found["folder/stored.txt"], b"stored bytes");
        for name in ["folder/five.txt", "six.txt", "seven.txt", "four.txt"] {
            assert_eq!(found[name], data, "level {level} {name}");
        }
        assert_eq!(found["empty"], b"");
        let listed =
            read_archive(&root.path().join("case.lzh"), None, ReadOptions::default()).unwrap();
        assert_eq!(listed.len(), 7);
    }
}

#[test]
fn unsafe_names_and_links_never_escape() {
    for level in 0..=3 {
        for name in [
            "../escape",
            "/absolute",
            "a/../../b",
            "C:drive",
            "a/./b",
            "a//b",
            "",
        ] {
            let bytes = lzh_writer::archive(&[Entry::stored(level, name, b"x")]);
            assert_rejected_without_leftovers(&format!("level {level} {name:?}"), &bytes);
        }
    }
    let backslashed = Entry {
        name: b"a\\..\\..\\b".to_vec(),
        ..Entry::stored(0, "", b"x")
    };
    assert_rejected_with(
        "backslashes",
        &lzh_writer::archive(&[backslashed]),
        "unsafe",
    );
    let dotted = Entry {
        extensions: vec![(0x02, b"..\xFF".to_vec())],
        ..Entry::stored(2, "file", b"x")
    };
    assert_rejected_with(
        "level 2 folder ..",
        &lzh_writer::archive(&[dotted]),
        "unsafe",
    );
    let mut parent = Entry::stored(1, "file", b"x");
    parent.extensions.push((0x02, b"..\xFF".to_vec()));
    assert_rejected_with(
        "level 1 folder ..",
        &lzh_writer::archive(&[parent]),
        "unsafe",
    );
    for target in ["../../outside", "/etc/passwd", "a/../../x"] {
        let link = Entry {
            mode: Some(0o120777),
            name: format!("link|{target}").into_bytes(),
            ..Entry::directory(2, "")
        };
        assert_rejected_without_leftovers(
            &format!("link to {target}"),
            &lzh_writer::archive(&[link]),
        );
    }
    let link = Entry {
        mode: Some(0o120777),
        name: b"link|folder".to_vec(),
        ..Entry::directory(2, "")
    };
    let through = Entry::stored(2, "link/x", b"x");
    assert_rejected_without_leftovers(
        "a file through a link",
        &lzh_writer::archive(&[Entry::directory(2, "folder"), link, through]),
    );
    for name in ["no-target", "|target", "link|"] {
        let link = Entry {
            mode: Some(0o120777),
            name: name.as_bytes().to_vec(),
            ..Entry::directory(1, "")
        };
        assert_rejected_with(name, &lzh_writer::archive(&[link]), "symbolic link");
    }
}

#[test]
fn links_folders_modes_and_times_restore() {
    let link = Entry {
        mode: Some(0o120777),
        name: b"folder/link|../target.txt".to_vec(),
        ..Entry::directory(1, "")
    };
    let mut executable = Entry::stored(2, "folder/run", b"#!/bin/sh\n");
    executable.mode = Some(0o100755);
    executable.time = 1_700_000_000;
    let mut level_zero = Entry::stored(0, "dos.txt", b"dos");
    level_zero.time = 0x5ACF_6D55;
    let mut unix_zero = Entry::stored(0, "unix.txt", b"unix");
    unix_zero.mode = Some(0o100600);
    unix_zero.time = 1_600_000_000;
    let mut level_one_time = Entry::stored(1, "level1.txt", b"one");
    level_one_time
        .extensions
        .push((0x54, 1_500_000_000u32.to_le_bytes().to_vec()));
    let bytes = lzh_writer::archive(&[
        Entry::stored(1, "target.txt", b"target"),
        Entry::directory(1, "folder"),
        link,
        executable,
        level_zero,
        unix_zero,
        level_one_time,
    ]);
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    let found = tree(&destination);
    assert_eq!(found["folder/link"], b"-> ../target.txt");
    let seconds = |name: &str| {
        fs::metadata(destination.join(name))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(seconds("folder/run"), 1_700_000_000);
    assert_eq!(seconds("unix.txt"), 1_600_000_000);
    assert_eq!(seconds("level1.txt"), 1_500_000_000);
    assert!(
        (1_749_900_000..1_750_100_000).contains(&seconds("dos.txt")),
        "DOS time 2025-06-15 13:42:42 local, got {}",
        seconds("dos.txt")
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
        assert_eq!(mode("folder/run") & 0o100, 0o100);
        assert_eq!(mode("unix.txt"), 0o600);
    }
}

#[test]
fn names_that_are_not_utf8_or_too_long_are_refused() {
    let mut latin = Entry::stored(2, "x", b"x");
    latin.name = b"caf\xE9".to_vec();
    assert_rejected_with("latin-1", &lzh_writer::archive(&[latin]), "UTF-8");
    let long = "a/".repeat(4097) + "b";
    assert_rejected_with(
        "long",
        &lzh_writer::archive(&[Entry::stored(2, &long, b"x")]),
        "longer than 8192",
    );
    let mut repeated = Entry::stored(2, "a", b"x");
    repeated.extensions.push((0x01, b"b".to_vec()));
    assert_rejected_with(
        "two names",
        &lzh_writer::archive(&[repeated]),
        "two file names",
    );
}

fn header_length(entry: &Entry) -> usize {
    lzh_writer::entry_bytes(&Entry {
        packed: Vec::new(),
        ..entry.clone()
    })
    .len()
}

#[test]
fn headers_that_lie_are_refused() {
    let entry = Entry::stored(0, "a.txt", b"abc");
    let mut bytes = lzh_writer::archive(std::slice::from_ref(&entry));
    bytes[1] ^= 1;
    assert_rejected_with("level 0 checksum", &bytes, "checksum");
    let mut bytes = lzh_writer::archive(&[Entry::stored(1, "a.txt", b"abc")]);
    bytes[1] ^= 1;
    assert_rejected_with("level 1 checksum", &bytes, "checksum");
    for level in [2, 3] {
        let mut bytes = lzh_writer::archive(&[Entry::stored(level, "a.txt", b"abc")]);
        let at = if level == 2 { 27 } else { 33 };
        bytes[at] ^= 1;
        assert_rejected_with(&format!("level {level} header CRC"), &bytes, "header CRC");
        let mut bytes = lzh_writer::archive(&[Entry::stored(level, "a.txt", b"abc")]);
        bytes[2] ^= 0x10;
        assert_rejected_without_leftovers(&format!("level {level} method"), &bytes);
    }
    let mut bytes = lzh_writer::archive(&[Entry::stored(2, "a.txt", b"abc")]);
    let total = u16::from_le_bytes([bytes[0], bytes[1]]);
    bytes[..2].copy_from_slice(&(total + 2).to_le_bytes());
    assert_rejected_with("level 2 padding of 2", &bytes, "leaves 2 bytes");
    let mut bytes = lzh_writer::archive(&[Entry::stored(2, "a.txt", b"abc")]);
    bytes[..2].copy_from_slice(&(total - 1).to_le_bytes());
    assert_rejected_with("level 2 too short", &bytes, "pass the header size");
    let mut bytes = lzh_writer::archive(&[Entry::stored(3, "a.txt", b"abc")]);
    let total = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    bytes[24..28].copy_from_slice(&(total + 1).to_le_bytes());
    assert_rejected_with("level 3 padding", &bytes, "leaves 1 bytes");
    let mut bytes = lzh_writer::archive(&[Entry::stored(3, "a.txt", b"abc")]);
    bytes[0] = 2;
    assert_rejected_with("level 3 with 2-byte sizes", &bytes, "4-byte sizes");
    let mut bytes = lzh_writer::archive(&[Entry::stored(2, "a.txt", b"abc")]);
    bytes[20] = 4;
    assert_rejected_with("level 4", &bytes, "level 4");
    for (kind, words) in [(0x42u8, "64-bit"), (0x39, "split across disks")] {
        let mut entry = Entry::stored(2, "a.txt", b"abc");
        entry.extensions.push((kind, vec![0; 16]));
        assert_rejected_with(words, &lzh_writer::archive(&[entry]), words);
    }
    let mut short_time = Entry::stored(2, "a.txt", b"abc");
    short_time.extensions.push((0x54, vec![1, 2]));
    assert_rejected_with(
        "short time",
        &lzh_writer::archive(&[short_time]),
        "shorter than its value",
    );
    let mut many = Entry::stored(1, "a.txt", b"abc");
    many.extensions = vec![(0x7E, Vec::new()); 257];
    assert_rejected_with(
        "257 extended headers",
        &lzh_writer::archive(&[many]),
        "more than 256",
    );
    let level_one = Entry {
        mode: Some(0o100644),
        ..Entry::stored(1, "a.txt", b"abc")
    };
    let mut bytes = lzh_writer::archive(std::slice::from_ref(&level_one));
    bytes[7..11].copy_from_slice(&1u32.to_le_bytes());
    let size = usize::from(bytes[0]) + 2;
    bytes[1] = bytes[2..size]
        .iter()
        .fold(0u8, |sum, &b| sum.wrapping_add(b));
    assert_rejected_with("level 1 skip size", &bytes, "larger than the entry");
    let mut bytes = lzh_writer::archive(&[Entry::stored(0, "abc", b"abc")]);
    bytes[0] += 1;
    bytes.insert(27, 0);
    bytes[1] = bytes[2..usize::from(bytes[0]) + 2]
        .iter()
        .fold(0u8, |sum, &b| sum.wrapping_add(b));
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        tree(&destination)["abc"],
        b"abc",
        "a level 0 area that is not Unix is skipped"
    );
    let mut bytes = lzh_writer::archive(&[level_one]);
    let size = usize::from(bytes[0]) + 2;
    bytes.insert(size - 2, 0);
    bytes[0] += 1;
    bytes[1] = bytes[2..size + 1]
        .iter()
        .fold(0u8, |sum, &b| sum.wrapping_add(b));
    assert_rejected_with("level 1 extra bytes", &bytes, "after its OS byte");
}

#[test]
fn entries_whose_sizes_or_checks_lie_are_refused() {
    let data = text(400);
    let mut stored = Entry::stored(1, "a", b"abcdef");
    stored.size = 5;
    assert_rejected_with(
        "stored sizes",
        &lzh_writer::archive(&[stored]),
        "packs 6 bytes for 5",
    );
    let mut folder = Entry::directory(2, "dir");
    folder.size = 1;
    assert_rejected_with(
        "folder size",
        &lzh_writer::archive(&[folder]),
        "carries data",
    );
    let mut folder = Entry::directory(2, "dir");
    folder.packed = b"x".to_vec();
    assert_rejected_with(
        "folder data",
        &lzh_writer::archive(&[folder]),
        "carries data",
    );
    let mut crc = Entry::stored(2, "a", b"abc");
    crc.crc ^= 1;
    assert_rejected_with("stored CRC", &lzh_writer::archive(&[crc]), "CRC-16");
    let mut empty = Entry::stored(2, "a", b"");
    empty.crc = 1;
    assert_rejected_with("empty CRC", &lzh_writer::archive(&[empty]), "CRC-16");
    let mut empty = Entry::compressed(2, "a", b"", lzh_writer::method(b'5'), 1);
    empty.packed = vec![0];
    assert_rejected_with(
        "empty with data",
        &lzh_writer::archive(&[empty]),
        "packed size",
    );
    for digit in [b'4', b'5', b'6', b'7'] {
        let mut wrong = Entry::compressed(2, "a", &data, lzh_writer::method(digit), 9);
        wrong.crc ^= 0x100;
        assert_rejected_with("compressed CRC", &lzh_writer::archive(&[wrong]), "CRC-16");
        let mut long = Entry::compressed(2, "a", &data, lzh_writer::method(digit), 9);
        long.size += 1;
        assert_rejected_without_leftovers("longer size", &lzh_writer::archive(&[long]));
        let mut short = Entry::compressed(2, "a", &data, lzh_writer::method(digit), 9);
        short.size -= 1;
        assert_rejected_without_leftovers("shorter size", &lzh_writer::archive(&[short]));
        let mut padded = Entry::compressed(2, "a", &data, lzh_writer::method(digit), 9);
        padded.packed.push(0);
        assert_rejected_with(
            "trailing byte",
            &lzh_writer::archive(&[padded]),
            "packed size",
        );
    }
    let far = vec![
        Token::Literal(b'a'),
        Token::Match {
            length: 3,
            distance: 1,
        },
    ];
    let entry = unchecked(&far, 4, b'5');
    assert_rejected_with(
        "before the window",
        &lzh_writer::archive(&[entry]),
        "before its window",
    );
    let wide: Vec<Token> = std::iter::repeat_n(Token::Literal(b'z'), 9000)
        .chain([Token::Match {
            length: 3,
            distance: 8192,
        }])
        .collect();
    let entry = unchecked(&wide, 9003, b'5');
    assert_rejected_with(
        "past the -lh5- window",
        &lzh_writer::archive(&[entry]),
        "before its window",
    );
    let mut hidden = unchecked(&[Token::Literal(b'b'), Token::Literal(b'a')], 1, b'5');
    hidden.crc = lzh_encoder::crc16(b"b");
    assert_rejected_with(
        "a symbol hidden in the padding",
        &lzh_writer::archive(&[hidden]),
        "more symbols than the file",
    );
    let past = [
        Token::Literal(b'a'),
        Token::Match {
            length: 10,
            distance: 0,
        },
    ];
    assert_rejected_with(
        "a match past the size",
        &lzh_writer::archive(&[unchecked(&past, 5, b'5')]),
        "runs past the declared size",
    );
    let entry = Entry::from_tokens(2, "a", &wide, lzh_writer::method(b'6'), 1);
    let (_root, destination, result) =
        extract_with(&lzh_writer::archive(&[entry]), ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination)["a"], vec![b'z'; 9003]);
}

fn unchecked(tokens: &[Token], size: u32, digit: u8) -> Entry {
    let method = lzh_writer::method(digit);
    let shape = lzh_encoder::Shape {
        single_when_possible: true,
    };
    Entry {
        method,
        packed: lzh_encoder::encode(tokens, lzh_writer::position_count_bits(method), &[], shape),
        size,
        ..Entry::stored(2, "a", b"")
    }
}

#[test]
fn every_cut_is_refused_or_extracts_the_entries_before_it() {
    for level in 0..=3 {
        let entries = every_kind(level);
        let mut ends = vec![0];
        for entry in &entries {
            ends.push(ends.last().unwrap() + lzh_writer::entry_bytes(entry).len());
        }
        let bytes = lzh_writer::archive(&entries);
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.lzh");
        for cut in 1..bytes.len() - 1 {
            fs::write(&archive, &bytes[..cut]).unwrap();
            let result = read_archive(&archive, None, ReadOptions::default());
            match ends.iter().position(|&end| end == cut) {
                Some(count) => assert_eq!(result.unwrap().len(), count, "level {level} cut {cut}"),
                None => assert!(result.is_err(), "level {level} cut {cut}"),
            }
        }
        assert_eq!(bytes.len(), ends.last().unwrap() + 1);
    }
}

#[test]
fn every_damaged_byte_is_refused_or_read_without_a_panic() {
    for level in 0..=3 {
        let bytes = lzh_writer::archive(&every_kind(level));
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.lzh");
        let mut refused = 0;
        for at in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[at] ^= 1 << (at % 8);
            fs::write(&archive, &damaged).unwrap();
            refused += usize::from(read_archive(&archive, None, ReadOptions::default()).is_err());
        }
        assert!(
            refused * 10 > bytes.len() * 9,
            "level {level}: {refused} of {}",
            bytes.len()
        );
    }
    let mut random = Random(3);
    for _ in 0..20_000 {
        let length = 22 + random.below(60);
        let mut bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
        bytes[2..7].copy_from_slice(b"-lh5-");
        bytes[20] = random.below(4) as u8;
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.lzh");
        fs::write(&archive, &bytes).unwrap();
        let _ = read_archive(&archive, None, ReadOptions::default());
    }
}

#[test]
fn level_2_headers_padded_to_avoid_a_zero_low_byte_extract() {
    for length in 1..300 {
        let name = "n".repeat(length);
        let entry = Entry::stored(2, &name, b"padded");
        if header_length(&entry) & 0xFF != 1 {
            continue;
        }
        let (_root, destination, result) =
            extract_with(&lzh_writer::archive(&[entry]), ReadOptions::default());
        result.unwrap();
        assert_eq!(tree(&destination)[&name], b"padded");
        return;
    }
    panic!("no name length gave a padded header");
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let large = text(5000);
    let bytes = lzh_writer::archive(&[
        Entry::stored(1, "a", b"small"),
        Entry::compressed(1, "b", &large, lzh_writer::method(b'5'), 1),
    ]);
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(large.len() as u64),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
}

#[test]
fn selection_strip_and_existing_options_apply() {
    let bytes = lzh_writer::archive(&[
        Entry::stored(2, "keep/a.txt", b"a"),
        Entry::stored(2, "keep/b.txt", b"b"),
        Entry::stored(2, "drop/c.txt", b"c"),
    ]);
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
        tree(&destination).keys().collect::<Vec<_>>(),
        ["keep", "keep/a.txt", "keep/b.txt"]
    );
    let include = ["c.txt".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            include: &include,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination).keys().collect::<Vec<_>>(),
        ["drop", "drop/c.txt"]
    );
    let (root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: Strip::Components(1),
            metadata: Metadata::ContentOnly,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination).keys().collect::<Vec<_>>(),
        ["a.txt", "b.txt", "c.txt"]
    );
    let archive = root.path().join("case.lzh");
    assert!(read_archive(&archive, Some(&destination), ReadOptions::default()).is_err());
    let overwrite = ReadOptions {
        existing: Existing::Overwrite,
        ..ReadOptions::default()
    };
    assert!(read_archive(&archive, Some(&destination), overwrite).is_ok());
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
fn the_default_engine_reads_lzh_in_process() {
    let root = tempfile::tempdir().unwrap();
    for level in 0..=3 {
        let path = root.path().join(format!("level{level}.lzh"));
        fs::write(&path, lzh_writer::archive(&every_kind(level))).unwrap();
        for engine in [zipper::Engine::Auto, zipper::Engine::Native] {
            assert_eq!(
                zipper::choose_reader(engine, &path).unwrap(),
                zipper::Reader::Lzh
            );
        }
        let destination = root.path().join(format!("out{level}"));
        zipper::extract_archive(&path, &destination, zipper::ExtractOptions::default()).unwrap();
        assert_eq!(
            fs::read(destination.join("folder/stored.txt")).unwrap(),
            b"stored bytes"
        );
    }
    let broken = root.path().join("broken.lzh");
    fs::write(&broken, b"\x20\x00-lh5-garbage that \x01ends early").unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &broken).unwrap(),
        zipper::Reader::Lzh
    );
}

#[test]
fn an_archive_of_many_entries_with_long_names_reads_within_its_limits() {
    let entries: Vec<Entry> = (0..20_000)
        .map(|index| Entry::stored(2, &format!("{index:05}{}", "n".repeat(200)), b""))
        .collect();
    let (_root, destination, result) =
        extract_with(&lzh_writer::archive(&entries), ReadOptions::default());
    assert_eq!(result.unwrap().len(), 20_000);
    assert_eq!(fs::read_dir(destination).unwrap().count(), 20_000);
}
