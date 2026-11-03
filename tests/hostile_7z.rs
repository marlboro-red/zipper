//! Crafted 7z archives that real writers never produce. Each must be refused
//! with nothing left behind, or extract only inside the destination, and a
//! well-formed one must extract the same bytes at every thread count.
use std::fs;
use zipper::native_7z::{ReadOptions, read_archive};

const UNIX_FILE: u32 = 0o100644 << 16 | 0x8000;
const UNIX_LINK: u32 = 0o120777 << 16 | 0x8000;
const DIRECTORY: u32 = 0x10;

#[derive(Clone)]
struct Member {
    name: String,
    data: Option<Vec<u8>>,
    attribute: u32,
}

fn file(name: &str, data: &[u8]) -> Member {
    Member {
        name: name.into(),
        data: Some(data.to_vec()),
        attribute: UNIX_FILE,
    }
}

fn directory(name: &str) -> Member {
    Member {
        name: name.into(),
        data: None,
        attribute: DIRECTORY,
    }
}

fn link(name: &str, target: &str) -> Member {
    Member {
        name: name.into(),
        data: Some(target.as_bytes().to_vec()),
        attribute: UNIX_LINK,
    }
}

fn number(value: u64) -> Vec<u8> {
    let mut bytes = vec![0xFF];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

fn bits(values: &[bool]) -> Vec<u8> {
    let mut bytes = vec![0u8; values.len().div_ceil(8)];
    for (index, &value) in values.iter().enumerate() {
        if value {
            bytes[index / 8] |= 0x80 >> (index % 8);
        }
    }
    bytes
}

fn property(id: u8, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![id];
    bytes.extend(number(data.len() as u64));
    bytes.extend_from_slice(data);
    bytes
}

#[derive(Default)]
struct HeaderDamage {
    coder: Option<(Vec<u8>, Vec<u8>)>,
    unpack_size_delta: i64,
    pack_position: u64,
    pack_size_delta: i64,
    crc: Option<u32>,
}

fn copy_folder_archive(members: &[Member], damage: HeaderDamage) -> Vec<u8> {
    let streams: Vec<&Vec<u8>> = members
        .iter()
        .filter_map(|m| m.data.as_ref().filter(|data| !data.is_empty()))
        .collect();
    let packed: Vec<u8> = streams
        .iter()
        .flat_map(|data| data.iter().copied())
        .collect();
    let mut header = vec![1u8];
    if !streams.is_empty() {
        header.push(4);
        header.push(6);
        header.extend(number(damage.pack_position));
        header.extend(number(streams.len() as u64));
        header.push(9);
        for data in &streams {
            header.extend(number((data.len() as i64 + damage.pack_size_delta) as u64));
        }
        header.push(0);
        header.push(7);
        header.push(11);
        header.extend(number(streams.len() as u64));
        header.push(0);
        let (id, properties) = damage.coder.clone().unwrap_or((vec![0], Vec::new()));
        for _ in &streams {
            header.extend(number(1));
            let flags = id.len() as u8 | if properties.is_empty() { 0 } else { 0x20 };
            header.push(flags);
            header.extend_from_slice(&id);
            if !properties.is_empty() {
                header.extend(number(properties.len() as u64));
                header.extend_from_slice(&properties);
            }
        }
        header.push(12);
        for data in &streams {
            header.extend(number(
                (data.len() as i64 + damage.unpack_size_delta) as u64,
            ));
        }
        header.push(10);
        header.push(1);
        for data in &streams {
            header.extend_from_slice(&damage.crc.unwrap_or(crc32fast::hash(data)).to_le_bytes());
        }
        header.push(0);
        header.push(0);
    }
    header.push(5);
    header.extend(number(members.len() as u64));
    let empty: Vec<bool> = members
        .iter()
        .map(|m| m.data.as_ref().is_none_or(|data| data.is_empty()))
        .collect();
    if empty.iter().any(|&e| e) {
        header.extend(property(14, &bits(&empty)));
        let empty_files: Vec<bool> = members
            .iter()
            .filter(|m| m.data.as_ref().is_none_or(|data| data.is_empty()))
            .map(|m| m.attribute & DIRECTORY == 0)
            .collect();
        header.extend(property(15, &bits(&empty_files)));
    }
    let mut names = vec![0u8];
    for member in members {
        for unit in member.name.encode_utf16().chain([0]) {
            names.extend_from_slice(&unit.to_le_bytes());
        }
    }
    header.extend(property(17, &names));
    let mut attributes = vec![1u8, 0];
    for member in members {
        attributes.extend_from_slice(&member.attribute.to_le_bytes());
    }
    header.extend(property(21, &attributes));
    header.push(0);
    header.push(0);
    let mut archive = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    let mut start = Vec::new();
    start.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    start.extend_from_slice(&(header.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
    archive.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
    archive.extend_from_slice(&start);
    archive.extend_from_slice(&packed);
    archive.extend_from_slice(&header);
    archive
}

fn extract(bytes: &[u8]) -> (tempfile::TempDir, anyhow::Result<Vec<zipper::Entry>>) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.7z");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir(root.path().join("parent")).unwrap();
    let result = read_archive(
        &archive,
        Some(&root.path().join("parent/out")),
        ReadOptions::default(),
    );
    (root, result)
}

fn assert_refused(case: &str, bytes: &[u8], words: &str) {
    let (root, result) = extract(bytes);
    let error = format!("{:#}", result.expect_err(case));
    assert!(error.contains(words), "{case}: {error}");
    let left: Vec<_> = fs::read_dir(root.path().join("parent")).unwrap().collect();
    assert!(left.is_empty(), "{case} left output");
}

#[test]
fn the_builder_writes_archives_7zip_reads() {
    let bytes = copy_folder_archive(
        &[directory("d"), file("d/a", b"alpha"), file("b", b"")],
        HeaderDamage::default(),
    );
    let (root, result) = extract(&bytes);
    result.unwrap();
    let out = root.path().join("parent/out");
    assert_eq!(fs::read(out.join("d/a")).unwrap(), b"alpha");
    assert_eq!(fs::read(out.join("b")).unwrap(), b"");
    if let Some(seven_zip) = which("7zz") {
        let archive = root.path().join("check.7z");
        fs::write(&archive, &bytes).unwrap();
        let status = std::process::Command::new(seven_zip)
            .args(["t", "-bd"])
            .arg(&archive)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "7-Zip rejects the builder's archive");
    }
}

fn which(tool: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|folder| folder.join(tool))
            .find(|path| path.is_file())
    })
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        "../escape",
        "/absolute",
        "a/../../b",
        "a\\b",
        "C:drive",
        "a//b",
        ".",
        "",
    ] {
        assert_refused(
            name,
            &copy_folder_archive(&[file(name, b"x")], HeaderDamage::default()),
            "",
        );
    }
}

#[test]
fn links_stay_inside() {
    for target in ["../outside", "/etc/passwd", "a/../../x"] {
        assert_refused(
            target,
            &copy_folder_archive(&[link("l", target)], HeaderDamage::default()),
            "symlink",
        );
    }
    assert_refused(
        "write through a link",
        &copy_folder_archive(
            &[link("l", "."), file("l/x", b"x")],
            HeaderDamage::default(),
        ),
        "file used as directory",
    );
}

#[test]
fn ambiguous_entry_sets_are_refused() {
    assert_refused(
        "duplicate file",
        &copy_folder_archive(&[file("a", b"1"), file("a", b"2")], HeaderDamage::default()),
        "duplicate 7z path",
    );
    assert_refused(
        "file then child",
        &copy_folder_archive(
            &[file("a", b"1"), file("a/b", b"2")],
            HeaderDamage::default(),
        ),
        "file used as directory",
    );
    assert_refused(
        "child then file",
        &copy_folder_archive(
            &[file("a/b", b"2"), file("a", b"1")],
            HeaderDamage::default(),
        ),
        "file used as directory",
    );
    assert_refused(
        "directory with data",
        &copy_folder_archive(
            &[Member {
                attribute: DIRECTORY,
                ..file("d", b"data")
            }],
            HeaderDamage::default(),
        ),
        "folder with data",
    );
}

#[test]
fn sizes_and_crcs_must_agree() {
    let members = [file("a", b"alpha"), file("b", b"beta")];
    for (case, damage, words) in [
        (
            "crc",
            HeaderDamage {
                crc: Some(0),
                ..HeaderDamage::default()
            },
            "CRC mismatch",
        ),
        (
            "unpack larger than pack",
            HeaderDamage {
                unpack_size_delta: 1,
                ..HeaderDamage::default()
            },
            "",
        ),
        (
            "pack past the header",
            HeaderDamage {
                pack_size_delta: 100,
                ..HeaderDamage::default()
            },
            "overlaps the header",
        ),
        (
            "pack position past the header",
            HeaderDamage {
                pack_position: u64::MAX - 2,
                ..HeaderDamage::default()
            },
            "overflow",
        ),
    ] {
        assert_refused(case, &copy_folder_archive(&members, damage), words);
    }
}

#[test]
fn unsupported_methods_are_refused() {
    for (id, words) in [
        (vec![0x02, 0x03, 0x02], "method 020302"),
        (vec![0x06, 0xF1, 0x07, 0x01], "encrypted"),
        (vec![0x03, 0x04, 0x01], "method 030401"),
    ] {
        assert_refused(
            &format!("{id:?}"),
            &copy_folder_archive(
                &[file("a", b"x")],
                HeaderDamage {
                    coder: Some((id, vec![1])),
                    ..HeaderDamage::default()
                },
            ),
            words,
        );
    }
}

#[test]
fn a_huge_declared_dictionary_still_decodes() {
    let data = b"small";
    let mut encoder = liblzma::stream::Stream::new_lzma_encoder(
        &liblzma::stream::LzmaOptions::new_preset(1).unwrap(),
    )
    .unwrap();
    let mut packed = Vec::with_capacity(1024);
    encoder
        .process_vec(data, &mut packed, liblzma::stream::Action::Finish)
        .unwrap();
    let raw = packed[13..].to_vec();
    let mut properties = packed[..5].to_vec();
    properties[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
    let members = [Member {
        data: Some(raw.clone()),
        ..file("a", data)
    }];
    let mut bytes = copy_folder_archive(
        &members,
        HeaderDamage {
            coder: Some((vec![0x03, 0x01, 0x01], properties)),
            unpack_size_delta: data.len() as i64 - raw.len() as i64,
            crc: Some(crc32fast::hash(data)),
            ..HeaderDamage::default()
        },
    );
    let (root, result) = extract(&bytes);
    result.unwrap();
    assert_eq!(fs::read(root.path().join("parent/out/a")).unwrap(), data);
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_refused("header crc", &bytes, "header CRC mismatch");
}

#[test]
fn truncation_at_every_length_is_refused() {
    let bytes = copy_folder_archive(
        &[directory("d"), file("d/a", b"alpha"), file("b", b"beta")],
        HeaderDamage::default(),
    );
    for length in 0..bytes.len() {
        let (root, result) = extract(&bytes[..length]);
        assert!(result.is_err(), "length {length}");
        assert!(
            fs::read_dir(root.path().join("parent"))
                .unwrap()
                .next()
                .is_none(),
            "length {length}"
        );
    }
}

#[test]
fn unsafe_archives_stay_with_the_native_reader_under_auto() {
    let root = tempfile::tempdir().unwrap();
    for (case, members) in [
        ("duplicate", vec![file("x", b"1"), file("x", b"2")]),
        ("parent", vec![file("../escape", b"1")]),
    ] {
        let archive = root.path().join(format!("{case}.7z"));
        fs::write(
            &archive,
            copy_folder_archive(&members, HeaderDamage::default()),
        )
        .unwrap();
        assert_eq!(
            zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
            zipper::Reader::SevenZip,
            "{case}"
        );
        let destination = root.path().join(format!("{case}-out"));
        assert!(
            zipper::extract_archive(&archive, &destination, Default::default()).is_err(),
            "{case}"
        );
        assert!(!destination.exists(), "{case}");
    }
}

#[test]
fn empty_folders_and_trailing_data_are_refused() {
    assert_refused(
        "empty folder",
        &copy_folder_archive(
            &[file("a", b"x")],
            HeaderDamage {
                unpack_size_delta: -1,
                crc: Some(0),
                ..HeaderDamage::default()
            },
        ),
        "folder is empty",
    );
    assert_refused(
        "trailing bytes",
        &copy_folder_archive(
            &[file("a", b"xy")],
            HeaderDamage {
                unpack_size_delta: -1,
                crc: Some(crc32fast::hash(b"x")),
                ..HeaderDamage::default()
            },
        ),
        "more data than its files",
    );
}

#[test]
fn a_callback_that_runs_rayon_work_does_not_deadlock() {
    use rayon::prelude::*;
    let archive = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/7z-corpus/7zip-nonsolid-short.7z");
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("out");
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let busy = |_: zipper::Progress| {
            let _: u64 = (0..200_000u32)
                .into_par_iter()
                .with_max_len(1)
                .map(u64::from)
                .sum();
        };
        let options = ReadOptions {
            control: zipper::Control {
                progress: Some(&busy),
                cancel: None,
            },
            ..ReadOptions::default()
        };
        let _ = done.send(read_archive(&archive, Some(&destination), options).is_ok());
    });
    let succeeded = finished
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("7z extraction deadlocked in the callback");
    assert!(succeeded);
}

fn lzma2_segments(parts: &[Vec<u8>]) -> (Vec<u8>, u8) {
    let mut stream = Vec::new();
    for part in parts {
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma2(&liblzma::stream::LzmaOptions::new_preset(1).unwrap());
        let mut encoder = liblzma::stream::Stream::new_raw_encoder(&filters).unwrap();
        let mut encoded = Vec::with_capacity(part.len() + 1024);
        encoder
            .process_vec(part, &mut encoded, liblzma::stream::Action::Finish)
            .unwrap();
        assert_eq!(encoded.pop(), Some(0), "a raw LZMA2 stream ends with 0x00");
        stream.extend_from_slice(&encoded);
    }
    stream.push(0);
    (stream, 18)
}

fn segmented_archive(parts: &[Vec<u8>], damage: impl FnOnce(&mut Vec<u8>)) -> (Vec<u8>, Vec<u8>) {
    let data: Vec<u8> = parts.concat();
    let (mut packed, property) = lzma2_segments(parts);
    damage(&mut packed);
    let bytes = copy_folder_archive(
        &[Member {
            data: Some(packed.clone()),
            ..file("big", b"")
        }],
        HeaderDamage {
            coder: Some((vec![0x21], vec![property])),
            unpack_size_delta: data.len() as i64 - packed.len() as i64,
            crc: Some(crc32fast::hash(&data)),
            ..HeaderDamage::default()
        },
    );
    (bytes, data)
}

fn parts() -> Vec<Vec<u8>> {
    (0..3u8)
        .map(|part| {
            (0..3_500_000u32)
                .map(|index| (index.wrapping_mul(2_654_435_761) >> 24) as u8 ^ part)
                .collect()
        })
        .collect()
}

#[test]
fn solid_lzma2_segments_decode_in_parallel_to_the_same_bytes() {
    let (bytes, data) = segmented_archive(&parts(), |_| {});
    for threads in [1, 2, 4] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("a.7z");
        fs::write(&archive, &bytes).unwrap();
        let destination = root.path().join("out");
        read_archive(
            &archive,
            Some(&destination),
            ReadOptions {
                threads: Some(threads),
                ..ReadOptions::default()
            },
        )
        .unwrap();
        assert!(
            fs::read(destination.join("big")).unwrap() == data,
            "{threads} threads"
        );
    }
}

#[test]
fn a_damaged_lzma2_segment_fails_on_every_thread_count() {
    for (case, damage) in [
        ("flipped byte in the second segment", 0usize),
        ("truncated last segment", 1),
        ("extra end marker", 2),
    ] {
        let (bytes, _) = segmented_archive(&parts(), |packed| match damage {
            0 => {
                let at = packed.len() / 2;
                packed[at] ^= 0x40;
            }
            1 => {
                let length = packed.len();
                packed.truncate(length - 100);
                packed.push(0);
            }
            _ => packed.insert(packed.len() / 3, 0),
        });
        for threads in [1, 4] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("a.7z");
            fs::write(&archive, &bytes).unwrap();
            fs::create_dir(root.path().join("parent")).unwrap();
            let result = read_archive(
                &archive,
                Some(&root.path().join("parent/out")),
                ReadOptions {
                    threads: Some(threads),
                    ..ReadOptions::default()
                },
            );
            assert!(result.is_err(), "{case}, {threads} threads");
            assert!(
                fs::read_dir(root.path().join("parent"))
                    .unwrap()
                    .next()
                    .is_none(),
                "{case}, {threads} threads left output"
            );
        }
    }
}
