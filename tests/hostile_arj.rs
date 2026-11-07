//! The ARJ reader rejects each hostile layout with nothing left behind,
//! refuses encrypted entries and volumes, restores modes and times, and
//! honours the options the other readers take.
#[path = "common/arj_writer.rs"]
mod arj_writer;
#[path = "common/lzh_encoder.rs"]
mod lzh_encoder;

use arj_writer::Entry;
use lzh_encoder::{Random, Shape, Token};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use zipper::{
    Strip,
    native_arj::{ReadOptions, read_archive},
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
        .filter(|path| path != "case.arj")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.arj");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile ARJ"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile ARJ"
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
    let archive = root.path().join("case.arj");
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

fn text(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|line| format!("line {line} {}\r\n", line * 7919 % 1000).into_bytes())
        .collect()
}

fn every_method() -> Vec<Entry> {
    let data = text(800);
    let mut entries = vec![
        Entry::directory("folder"),
        Entry::stored("folder/stored.txt", b"stored"),
    ];
    for method in 1..=4 {
        entries.push(Entry::compressed(
            &format!("method{method}.txt"),
            &data,
            method,
            u64::from(method),
        ));
    }
    entries.push(Entry::stored("empty", b""));
    entries
}

#[test]
fn every_method_extracts_byte_for_byte_and_verifies_without_a_destination() {
    let bytes = arj_writer::archive(&every_method());
    let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
    assert_eq!(result.unwrap().len(), 7);
    let found = tree(&destination);
    assert_eq!(found["folder"], b"<dir>");
    assert_eq!(found["folder/stored.txt"], b"stored");
    for method in 1..=4 {
        assert_eq!(
            found[&format!("method{method}.txt")],
            text(800),
            "method {method}"
        );
    }
    assert_eq!(found["empty"], b"");
    assert_eq!(
        read_archive(&root.path().join("case.arj"), None, ReadOptions::default())
            .unwrap()
            .len(),
        7
    );
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        "../escape",
        "/absolute",
        "a/../../b",
        "a\\..\\..\\b",
        "C:drive",
        "a//b",
        "",
    ] {
        let bytes = arj_writer::archive(&[Entry::stored(name, b"x")]);
        assert_rejected_without_leftovers(&format!("{name:?}"), &bytes);
    }
    let mut latin = Entry::stored("x", b"x");
    latin.name = b"caf\xE9".to_vec();
    assert_rejected_with("latin-1", &arj_writer::archive(&[latin]), "UTF-8");
    let (_root, destination, result) = extract_with(
        &arj_writer::archive(&[Entry::stored("dos\\path.txt", b"x")]),
        ReadOptions::default(),
    );
    result.unwrap();
    assert_eq!(tree(&destination)["dos/path.txt"], b"x");
}

#[test]
fn encrypted_entries_and_volumes_are_refused() {
    let mut garbled = Entry::stored("a", b"x");
    garbled.flags |= 0x01;
    assert_rejected_with("garbled", &arj_writer::archive(&[garbled]), "encrypted");
    for flag in [0x04, 0x08] {
        let mut split = Entry::stored("a", b"x");
        split.flags |= flag;
        assert_rejected_with(
            "split entry",
            &arj_writer::archive(&[split]),
            "spans volumes",
        );
    }
    let mut bytes = arj_writer::main_header(0x14);
    bytes.extend(arj_writer::entry_bytes(&Entry::stored("a", b"x")));
    bytes.extend_from_slice(&arj_writer::END);
    assert_rejected_with("multi-volume", &bytes, "multi-volume");
}

#[test]
fn headers_that_lie_are_refused() {
    let entry = Entry::stored("a.txt", b"abc");
    let main = arj_writer::main_header(0x10).len();
    let mut bytes = arj_writer::archive(std::slice::from_ref(&entry));
    bytes[main + 10] ^= 1;
    assert_rejected_with("header CRC", &bytes, "CRC-32");
    let mut bytes = arj_writer::archive(std::slice::from_ref(&entry));
    bytes[10] ^= 1;
    assert_rejected_with("main header CRC", &bytes, "CRC-32");
    let mut extended = entry.clone();
    extended.extended = vec![b"extra".to_vec()];
    let mut bytes = arj_writer::archive(&[extended]);
    let at = bytes.len() - 4 - 3 - 4 - 1;
    bytes[at] ^= 1;
    assert_rejected_with("extended header CRC", &bytes, "extended header fails");
    for (kind, words) in [(2u8, "file type"), (4, "file type"), (5, "file type")] {
        let mut odd = entry.clone();
        odd.kind = kind;
        assert_rejected_with(words, &arj_writer::archive(&[odd]), words);
    }
    for method in [5u8, 8, 9] {
        let mut odd = entry.clone();
        odd.method = method;
        assert_rejected_with("method", &arj_writer::archive(&[odd]), "method");
    }
    let mut short = entry.clone();
    short.first_size = 29;
    assert_rejected_with(
        "first size",
        &arj_writer::archive(&[short]),
        "first header size",
    );
    let mut bytes = arj_writer::archive(std::slice::from_ref(&entry));
    bytes[main + 2..main + 4].copy_from_slice(&29u16.to_le_bytes());
    assert_rejected_with("small header", &bytes, "outside 30 to 2600");
    let mut long = entry.clone();
    long.comment = vec![b'c'; 2600];
    assert_rejected_with(
        "large header",
        &arj_writer::archive(&[long]),
        "outside 30 to 2600",
    );
    let mut bytes = arj_writer::archive(std::slice::from_ref(&entry));
    bytes.truncate(bytes.len() - 4);
    assert_rejected_with("no end header", &bytes, "truncated");
    let mut bytes = arj_writer::archive(std::slice::from_ref(&entry));
    bytes[main] = 0x61;
    assert_rejected_with("signature", &bytes, "signature");
    let mut unended = arj_writer::entry_header(&entry);
    let size = u16::from_le_bytes([unended[2], unended[3]]) as usize;
    unended[4 + size - 1] = b'x';
    let crc = crc32fast::hash(&unended[4..4 + size]);
    unended[4 + size..8 + size].copy_from_slice(&crc.to_le_bytes());
    let mut bytes = arj_writer::main_header(0x10);
    bytes.extend(unended);
    bytes.extend_from_slice(b"abc");
    bytes.extend_from_slice(&arj_writer::END);
    assert_rejected_with("comment without its end", &bytes, "terminator");
    let mut trailing = arj_writer::archive(&[entry]);
    trailing.extend_from_slice(b"bytes after the end");
    let (_root, destination, result) = extract_with(&trailing, ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination)["a.txt"], b"abc");
}

#[test]
fn entries_whose_sizes_or_checks_lie_are_refused() {
    let data = text(400);
    let mut stored = Entry::stored("a", b"abcdef");
    stored.size = 5;
    assert_rejected_with(
        "stored sizes",
        &arj_writer::archive(&[stored]),
        "packs 6 bytes for 5",
    );
    let mut folder = Entry::directory("dir");
    folder.size = 1;
    assert_rejected_with(
        "folder size",
        &arj_writer::archive(&[folder]),
        "carries data",
    );
    let mut crc = Entry::stored("a", b"abc");
    crc.crc ^= 1;
    assert_rejected_with("stored CRC", &arj_writer::archive(&[crc]), "CRC-32");
    let mut empty = Entry::stored("a", b"");
    empty.crc = 1;
    assert_rejected_with("empty CRC", &arj_writer::archive(&[empty]), "CRC-32");
    for method in 1..=4 {
        let mut empty = Entry::compressed("a", b"", method, 1);
        empty.packed = vec![0];
        assert_rejected_with(
            "empty with data",
            &arj_writer::archive(&[empty]),
            "packed size",
        );
        let mut wrong = Entry::compressed("a", &data, method, 9);
        wrong.crc ^= 0x100;
        assert_rejected_with("compressed CRC", &arj_writer::archive(&[wrong]), "CRC-32");
        let mut long = Entry::compressed("a", &data, method, 9);
        long.size += 1;
        assert_rejected_without_leftovers("longer size", &arj_writer::archive(&[long]));
        let mut padded = Entry::compressed("a", &data, method, 9);
        padded.packed.push(0);
        assert_rejected_with(
            "trailing byte",
            &arj_writer::archive(&[padded]),
            "packed size",
        );
        let mut cut = Entry::compressed("a", &data, method, 9);
        cut.packed.pop();
        assert_rejected_without_leftovers("cut stream", &arj_writer::archive(&[cut]));
    }
    let far = [
        Token::Literal(b'a'),
        Token::Match {
            length: 3,
            distance: 1,
        },
    ];
    for method in [1u8, 4] {
        let packed = if method == 4 {
            lzh_encoder::encode_fastest(&far)
        } else {
            lzh_encoder::encode(
                &far,
                5,
                &[],
                Shape {
                    single_when_possible: true,
                },
            )
        };
        let entry = Entry {
            method,
            packed,
            size: 4,
            crc: 0,
            ..Entry::stored("a", b"")
        };
        assert_rejected_with(
            "before the window",
            &arj_writer::archive(&[entry]),
            "before its window",
        );
    }
    let past = [
        Token::Literal(b'a'),
        Token::Match {
            length: 10,
            distance: 0,
        },
    ];
    for method in [1u8, 4] {
        let packed = if method == 4 {
            lzh_encoder::encode_fastest(&past)
        } else {
            lzh_encoder::encode(
                &past,
                5,
                &[],
                Shape {
                    single_when_possible: true,
                },
            )
        };
        let entry = Entry {
            method,
            packed,
            size: 5,
            crc: 0,
            ..Entry::stored("a", b"")
        };
        assert_rejected_with(
            "a match past the size",
            &arj_writer::archive(&[entry]),
            "runs past the declared size",
        );
    }
    let wide: Vec<Token> = std::iter::repeat_n(Token::Literal(b'z'), 27_000)
        .chain([Token::Match {
            length: 3,
            distance: 26_624,
        }])
        .collect();
    let entry = Entry {
        method: 1,
        packed: lzh_encoder::encode(
            &wide,
            5,
            &[],
            Shape {
                single_when_possible: true,
            },
        ),
        size: 27_003,
        crc: crc32fast::hash(&[b'z'; 27_003]),
        ..Entry::stored("a", b"")
    };
    assert_rejected_with(
        "past the 26,624-byte window",
        &arj_writer::archive(&[entry]),
        "before its window",
    );
}

#[test]
fn every_cut_is_refused_and_no_damaged_byte_panics() {
    let bytes = arj_writer::archive(&every_method());
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.arj");
    for cut in 0..bytes.len() {
        fs::write(&archive, &bytes[..cut]).unwrap();
        assert!(
            read_archive(&archive, None, ReadOptions::default()).is_err(),
            "cut {cut}"
        );
    }
    let mut refused = 0;
    for at in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 1 << (at % 8);
        fs::write(&archive, &damaged).unwrap();
        refused += usize::from(read_archive(&archive, None, ReadOptions::default()).is_err());
    }
    assert!(
        refused * 10 > bytes.len() * 9,
        "{refused} of {}",
        bytes.len()
    );
    let mut random = Random(4);
    for _ in 0..5_000 {
        let entry = Entry::compressed("a", &text(3), 1 + random.below(4) as u8, 1);
        let mut entry = entry.clone();
        let length = random.below(40);
        entry.packed = (0..length).map(|_| random.next() as u8).collect();
        entry.size = random.below(400) as u32;
        let _ = extract_with(&arj_writer::archive(&[entry]), ReadOptions::default());
    }
}

#[test]
fn modes_times_and_options_apply() {
    let mut read_only = Entry::stored("keep/ro.txt", b"ro");
    read_only.mode = 0x21;
    let mut unix = Entry::stored("keep/run", b"#!/bin/sh\n");
    unix.host = arj_writer::HOST_UNIX;
    unix.mode = 0o100755;
    let bytes = arj_writer::archive(&[read_only, unix, Entry::stored("drop/c.txt", b"c")]);
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    let seconds = fs::metadata(destination.join("drop/c.txt"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        (1_735_600_000..1_735_800_000).contains(&seconds),
        "DOS time 2025-01-01 12:00 local, got {seconds}"
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
        assert_eq!(mode("keep/ro.txt") & 0o222, 0);
        assert_eq!(mode("keep/run") & 0o100, 0o100);
    }
    let selected = ["keep".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &selected,
            strip: Strip::Components(1),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination).keys().collect::<Vec<_>>(),
        ["ro.txt", "run"]
    );
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(3),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
}

#[test]
fn the_default_engine_reads_arj_in_process() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("case.arj");
    fs::write(&path, arj_writer::archive(&every_method())).unwrap();
    for engine in [zipper::Engine::Auto, zipper::Engine::Native] {
        assert_eq!(
            zipper::choose_reader(engine, &path).unwrap(),
            zipper::Reader::Arj
        );
    }
    let destination = root.path().join("out");
    zipper::extract_archive(&path, &destination, zipper::ExtractOptions::default()).unwrap();
    assert_eq!(
        fs::read(destination.join("folder/stored.txt")).unwrap(),
        b"stored"
    );
    let mut damaged = arj_writer::archive(&every_method());
    damaged[10] ^= 1;
    fs::write(&path, &damaged).unwrap();
    assert_ne!(
        zipper::choose_reader(zipper::Engine::Native, &path).unwrap(),
        zipper::Reader::Arj
    );
}
