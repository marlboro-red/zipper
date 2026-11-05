//! Crafted 7z archives that real writers never produce. Each must be refused
//! with nothing left behind, or extract only inside the destination, and a
//! well-formed one must extract the same bytes at every thread count.
use std::{fs, path::Path};
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
        (vec![0x04, 0xF7, 0x11, 0x01], "method 04F71101"),
        (vec![0x06, 0xF1, 0x07, 0x01], "encrypted"),
        (vec![0x02, 0x03, 0x03], "method 020303"),
        (vec![0x02, 0x03, 0x02], "020302 has properties"),
        (vec![0x02, 0x03, 0x04], "020304 has properties"),
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

#[test]
fn a_ppmd_model_beyond_the_decoder_budget_goes_to_7zip() {
    let mut properties = vec![6];
    properties.extend_from_slice(&0xffff_ff00u32.to_le_bytes());
    let bytes = copy_folder_archive(
        &[file("a", b"x")],
        HeaderDamage {
            coder: Some((vec![0x03, 0x04, 0x01], properties)),
            ..HeaderDamage::default()
        },
    );
    assert_refused("a 4 GiB PPMd model", &bytes, "decoder budget");
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("big-model.7z");
    fs::write(&archive, &bytes).unwrap();
    assert!(zipper::native_7z::needs_backend(&archive).unwrap());
}

#[test]
fn a_ppmd_folder_must_end_at_its_packed_size() {
    let archive =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/7z-hostile/ppmd-padded.7z");
    assert_refused(
        "a PPMd folder padded by one byte",
        &fs::read(archive).unwrap(),
        "packed size",
    );
}

fn bcj2_archive(streams: [&[u8]; 4], packed_inputs: [u64; 4], output: &[u8]) -> Vec<u8> {
    let packed: Vec<u8> = streams.concat();
    let mut header = vec![1u8, 4, 6];
    header.extend(number(0));
    header.extend(number(4));
    header.push(9);
    for stream in streams {
        header.extend(number(stream.len() as u64));
    }
    header.extend([0, 7, 11]);
    header.extend(number(1));
    header.extend([0, 1, 0x14, 0x03, 0x03, 0x01, 0x1B]);
    header.extend(number(4));
    header.extend(number(1));
    for input in packed_inputs {
        header.extend(number(input));
    }
    header.push(12);
    header.extend(number(output.len() as u64));
    header.extend([10, 1]);
    header.extend_from_slice(&crc32fast::hash(output).to_le_bytes());
    header.extend([0, 0, 5]);
    header.extend(number(1));
    let mut names = vec![0u8];
    for unit in "a".encode_utf16().chain([0]) {
        names.extend_from_slice(&unit.to_le_bytes());
    }
    header.extend(property(17, &names));
    let mut attributes = vec![1u8, 0];
    attributes.extend_from_slice(&UNIX_FILE.to_le_bytes());
    header.extend(property(21, &attributes));
    header.extend([0, 0]);
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

#[test]
fn bcj2_folders_use_every_input_byte_and_nothing_more() {
    let data = b"plain \xe8 and \xe9 and \x0f\x85 left as they are";
    let no_conversions = [0u8; 5];
    let order = [0, 1, 2, 3];
    let (root, result) = extract(&bcj2_archive(
        [data, b"", b"", &no_conversions],
        order,
        data,
    ));
    result.unwrap();
    assert_eq!(fs::read(root.path().join("parent/out/a")).unwrap(), data);
    let (root, result) = extract(&bcj2_archive(
        [&no_conversions, b"", b"", data],
        [3, 1, 2, 0],
        data,
    ));
    result.unwrap();
    assert_eq!(fs::read(root.path().join("parent/out/a")).unwrap(), data);
    let mut longer = data.to_vec();
    longer.push(b'!');
    let mut padded = no_conversions.to_vec();
    padded.push(0);
    let unused = "hold bytes the output does not use";
    assert_refused(
        "a main stream one byte long",
        &bcj2_archive([&longer, b"", b"", &no_conversions], order, data),
        unused,
    );
    assert_refused(
        "an unused call target",
        &bcj2_archive([data, b"\0\0\0\0", b"", &no_conversions], order, data),
        unused,
    );
    assert_refused(
        "an unused jump target",
        &bcj2_archive([data, b"", b"\0\0\0\0", &no_conversions], order, data),
        unused,
    );
    assert_refused(
        "a padded range coder",
        &bcj2_archive([data, b"", b"", &padded], order, data),
        unused,
    );
    assert_refused(
        "a short main stream",
        &bcj2_archive(
            [&data[..data.len() - 1], b"", b"", &no_conversions],
            order,
            data,
        ),
        "truncated BCJ2 main stream",
    );
    assert_refused(
        "a short range coder",
        &bcj2_archive([data, b"", b"", &no_conversions[..4]], order, data),
        "range coder",
    );
    assert_refused(
        "a packed stream used twice",
        &bcj2_archive([data, b"", b"", &no_conversions], [0, 1, 2, 2], data),
        "7z BCJ2",
    );
    assert_refused(
        "a packed input that does not exist",
        &bcj2_archive([data, b"", b"", &no_conversions], [0, 1, 2, 9], data),
        "7z BCJ2",
    );
}

#[test]
fn bcj2_folders_that_7zip_refuses_are_refused() {
    let hostile = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/7z-hostile");
    for (name, words) in [
        ("bcj2-main-overrun.7z", "hold bytes the output does not use"),
        (
            "bcj2-calls-overrun.7z",
            "hold bytes the output does not use",
        ),
        ("bcj2-tail-converted.7z", "past the end of its output"),
    ] {
        assert_refused(name, &fs::read(hostile.join(name)).unwrap(), words);
    }
}

#[test]
fn lzma_streams_end_where_7zip_ends_them() {
    let hostile = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/7z-hostile");
    let refused = [
        ("lzma2-cut-in-last-chunk.7z", "truncated LZMA stream"),
        ("bcj2-empty-lzma-branch-cut.7z", "truncated LZMA stream"),
        (
            "lzma-match-crosses-its-size.7z",
            "LZMA match runs past the declared size",
        ),
        (
            "lzma-goes-past-its-size.7z",
            "past its declared size without an end marker",
        ),
        (
            "bcj2-lzma-branch-unclean-at-its-size.7z",
            "past its declared size without an end marker",
        ),
        (
            "lzma-damaged-end-marker.7z",
            "past its declared size without an end marker",
        ),
    ];
    for (name, words) in refused {
        assert_refused(name, &fs::read(hostile.join(name)).unwrap(), words);
    }
    let unmarked = "lzma2-without-end-marker.7z";
    let (root, result) = extract(&fs::read(hostile.join(unmarked)).unwrap());
    result.unwrap();
    assert_eq!(
        fs::read(root.path().join("parent/out/src/a.txt")).unwrap(),
        b"linked text that repeats, linked text that repeats, linked text that repeats again.\n"
    );
    let Some(seven_zip) = which("7zz") else {
        return;
    };
    let verdicts = refused
        .iter()
        .map(|&(name, _)| (name, false))
        .chain([(unmarked, true)]);
    for (name, accepted) in verdicts {
        assert_eq!(
            seven_zip_accepts(&seven_zip, &fs::read(hostile.join(name)).unwrap()),
            accepted,
            "7-Zip on {name}"
        );
    }
}

#[test]
fn bcj2_with_every_input_bound_to_its_own_coder_matches_the_source() {
    let Some(seven_zip) = which("7zz") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("src");
    fs::create_dir(&source).unwrap();
    let mut code = Vec::new();
    for index in 0u32..40_000 {
        code.extend_from_slice(&[0xE8, 0x10, 0, 0, 0, 0x48, 0x89, 0xE9]);
        code.extend_from_slice(&(index % 300).to_le_bytes());
        code.extend_from_slice(&[0x0F, 0x84, 0x20, 0, 0, 0]);
    }
    fs::write(source.join("code.bin"), &code).unwrap();
    let archive = root.path().join("all-bound.7z");
    let status = std::process::Command::new(seven_zip)
        .current_dir(&source)
        .args([
            "a",
            "-bd",
            "-m0=BCJ2",
            "-m1=LZMA2",
            "-m2=LZMA",
            "-m3=LZMA",
            "-m4=LZMA",
        ])
        .args(["-mb0s0:1", "-mb0s1:2", "-mb0s2:3", "-mb0s3:4"])
        .arg(&archive)
        .arg("code.bin")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let (out, result) = extract(&fs::read(&archive).unwrap());
    result.unwrap();
    assert_eq!(
        fs::read(out.path().join("parent/out/code.bin")).unwrap(),
        code
    );
}

#[test]
fn byte_swap_archives_from_7zip_match_the_source() {
    let Some(seven_zip) = which("7zz") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("src");
    fs::create_dir(&source).unwrap();
    let data: Vec<u8> = (0..300_007u32)
        .map(|index| (index * 7 % 251) as u8)
        .collect();
    fs::write(source.join("odd.bin"), &data).unwrap();
    fs::write(source.join("three.bin"), &data[..3]).unwrap();
    for (index, methods) in [
        &["-mf=Swap2"][..],
        &["-mf=Swap4"],
        &["-m0=Swap4"],
        &["-m0=Swap2", "-m1=Swap4", "-m2=LZMA2"],
        &["-m0=Swap4", "-m1=Delta:4", "-m2=LZMA2"],
        &["-m0=Swap2", "-m1=BZip2"],
        &["-m0=Swap2", "-m1=BCJ", "-m2=LZMA"],
        &["-m0=ARM64", "-m1=LZMA"],
    ]
    .into_iter()
    .enumerate()
    {
        let archive = root.path().join(format!("{index}.7z"));
        let status = std::process::Command::new(&seven_zip)
            .current_dir(&source)
            .args(["a", "-bd"])
            .args(methods)
            .arg(&archive)
            .args(["odd.bin", "three.bin"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{methods:?}");
        let (out, result) = extract(&fs::read(&archive).unwrap());
        result.unwrap_or_else(|error| panic!("{methods:?}: {error:#}"));
        assert_eq!(
            fs::read(out.path().join("parent/out/odd.bin")).unwrap(),
            data,
            "{methods:?}"
        );
        assert_eq!(
            fs::read(out.path().join("parent/out/three.bin")).unwrap(),
            &data[..3],
            "{methods:?}"
        );
    }
}

fn seven_zip_accepts(seven_zip: &Path, bytes: &[u8]) -> bool {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.7z");
    fs::write(&archive, bytes).unwrap();
    std::process::Command::new(seven_zip)
        .args(["t", "-bd"])
        .arg(&archive)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_branch_opcode_that_ends_the_output_still_takes_a_range_coder_bit() {
    let data = b"\xe8\x00\xe8\x00\x41\xe8\xe8\x41\x0f\xe9\xe8\x41\x00\x41\xe8\xe9\xe9\x00\x41\xe8\xe9\xe9\xe9\xe8";
    let main: &[u8] = b"\xe8\xe8\xe8\xe9\xe9\xe9\xe9\xe8";
    let calls: &[u8] = b"\x41\x00\xe8\x05\xe9\x0f\x41\xf2\xe8\x41\x00\x50";
    let jumps: &[u8] = b"\xe8\x41\x00\xfd";
    let decisions: &[u8] = b"\x00\xef\xff\xfc\x00\x00";
    let order = [0, 1, 2, 3];
    let accepted = bcj2_archive([main, calls, jumps, decisions], order, data);
    let refused = [
        ("no bit for the last opcode", &decisions[..5], "range coder"),
        (
            "a padded range coder",
            b"\x00\xef\xff\xfc\x00\x00\x00".as_slice(),
            "hold bytes the output does not use",
        ),
        (
            "the last opcode converted",
            b"\x00\xf0\xf7\xf8\x20".as_slice(),
            "past the end of its output",
        ),
        (
            "a range coder that does not end at zero",
            b"\x00\xef\xff\xfc\x00\x01".as_slice(),
            "does not end at zero",
        ),
        (
            "a range coder that does not start at zero",
            b"\x01\xef\xff\xfc\x00\x00".as_slice(),
            "does not start at zero",
        ),
    ]
    .map(|(case, decisions, words)| {
        (
            case,
            bcj2_archive([main, calls, jumps, decisions], order, data),
            words,
        )
    });
    if let Some(seven_zip) = which("7zz") {
        assert!(seven_zip_accepts(&seven_zip, &accepted));
        for (case, bytes, _) in &refused {
            assert!(
                !seven_zip_accepts(&seven_zip, bytes),
                "7-Zip accepts {case}"
            );
        }
    }
    let (root, result) = extract(&accepted);
    result.unwrap();
    assert_eq!(fs::read(root.path().join("parent/out/a")).unwrap(), data);
    for (case, bytes, words) in &refused {
        assert_refused(case, bytes, words);
    }
}

#[test]
fn a_range_coder_that_starts_with_its_code_at_the_range_is_refused() {
    let jumps = 326;
    let data: Vec<u8> = (0..jumps)
        .flat_map(|index: u32| {
            [0xE9]
                .into_iter()
                .chain(0u32.wrapping_sub(index * 5 + 5).to_le_bytes())
        })
        .collect();
    let main = vec![0xE9; jumps as usize];
    let targets = vec![0; 4 * jumps as usize];
    let honest = b"\x00\xff\xff\xff\xfe\x02\xff\x8f\x00";
    let at_the_range = b"\x00\xff\xff\xff\xff\x02\xff\x8f\x00";
    let order = [0, 1, 2, 3];
    let accepted = bcj2_archive([&main, b"", &targets, honest], order, &data);
    let refused = bcj2_archive([&main, b"", &targets, at_the_range], order, &data);
    if let Some(seven_zip) = which("7zz") {
        assert!(seven_zip_accepts(&seven_zip, &accepted));
        assert!(!seven_zip_accepts(&seven_zip, &refused));
    }
    let (root, result) = extract(&accepted);
    result.unwrap();
    assert_eq!(fs::read(root.path().join("parent/out/a")).unwrap(), data);
    assert_refused("a code equal to the range", &refused, "range coder");
}

fn files(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                found.insert(name + "/", Vec::new());
                folders.push(path);
            } else {
                found.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn x86_like_code(length: usize, last: &[u8]) -> Vec<u8> {
    let mut state = 1u32;
    let mut code = Vec::new();
    while code.len() < length {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7FFF_FFFF;
        let target = (state >> 8) % 4096;
        match state % 7 {
            0 => code.extend([0xE8].into_iter().chain(target.to_le_bytes())),
            1 => code.extend(
                [0xE9]
                    .into_iter()
                    .chain(target.wrapping_neg().to_le_bytes()),
            ),
            2 => code.extend(
                [0x0F, 0x80 | (state % 16) as u8]
                    .into_iter()
                    .chain(target.to_le_bytes()),
            ),
            pick => code.extend(&[0x48, 0x89, state as u8, 0xC3][..1 + pick as usize % 4]),
        }
    }
    code.extend_from_slice(last);
    code
}

#[test]
fn bcj2_archives_from_7zip_extract_as_7zip_extracts_them() {
    let Some(seven_zip) = which("7zz") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("src");
    fs::create_dir_all(source.join("ends")).unwrap();
    fs::write(source.join("code.bin"), x86_like_code(200_000, b"\xe8")).unwrap();
    fs::write(
        source.join("plain.txt"),
        "no branch opcodes here\n".repeat(500),
    )
    .unwrap();
    fs::write(source.join("empty"), b"").unwrap();
    for (name, data) in [
        ("e8", &b"\xe8"[..]),
        ("e9", b"\xe9"),
        ("jcc", b"\x0f\x85"),
        ("a", b"a"),
    ] {
        fs::write(source.join(format!("tiny-{name}")), data).unwrap();
    }
    let endings: [&[u8]; 5] = [b"\xe8", b"\xe9", b"\x0f\x84", b"\xe8\x00\x00", b"\xe9\x01"];
    for index in 0..48usize {
        fs::write(
            source.join(format!("ends/{index}.bin")),
            x86_like_code(index * 37, endings[index % endings.len()]),
        )
        .unwrap();
    }
    let layouts: &[&[&str]] = &[
        &["-m0=BCJ2", "-m1=LZMA2"],
        &["-m0=BCJ2", "-m1=LZMA2:d=64k"],
        &["-m0=BCJ2", "-m1=LZMA2:d=1m"],
        &["-m0=BCJ2", "-m1=LZMA2:d=32m"],
        &["-m0=BCJ2", "-m1=LZMA"],
        &["-m0=BCJ2", "-m1=Copy"],
        &["-mf=BCJ2"],
        &[
            "-m0=BCJ2", "-m1=Copy", "-m2=Copy", "-m3=Copy", "-m4=Copy", "-mb0s0:1", "-mb0s1:2",
            "-mb0s2:3", "-mb0s3:4",
        ],
    ];
    for (index, methods) in layouts.iter().enumerate() {
        for solid in ["-ms=on", "-ms=off"] {
            let archive = root.path().join(format!("{index}{solid}.7z"));
            let status = std::process::Command::new(&seven_zip)
                .current_dir(&source)
                .args(["a", "-bd", solid])
                .args(*methods)
                .arg(&archive)
                .arg(".")
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{methods:?} {solid}");
            let expected = root.path().join(format!("7zip-{index}{solid}"));
            let status = std::process::Command::new(&seven_zip)
                .args(["x", "-bd", "-y"])
                .arg(format!("-o{}", expected.display()))
                .arg(&archive)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "7-Zip refuses {methods:?} {solid}");
            let (out, result) = extract(&fs::read(&archive).unwrap());
            result.unwrap_or_else(|error| panic!("{methods:?} {solid}: {error:#}"));
            assert_eq!(
                files(&out.path().join("parent/out")),
                files(&expected),
                "{methods:?} {solid}"
            );
        }
    }
}
