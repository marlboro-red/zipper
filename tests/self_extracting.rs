//! Self-extracting archives, an executable stub followed by a ZIP or a 7z.
//! Info-ZIP `zip -A` adjusts a ZIP's offsets to count the stub, and a plain
//! `cat stub a.zip` leaves them counting from the ZIP. Both must extract the
//! tree the ZIP gives alone, the names Python `zipfile` lists and the tree
//! 7-Zip extracts. A 7z whose signature starts at offset 8 MiB or earlier
//! after a PE, ELF or Mach-O stub must extract as it does alone and as 7-Zip
//! extracts it, and a start header that fails its CRC is skipped. A 7z past
//! that offset or after a prefix that is no executable is not found. A prefix
//! whose offsets do not reconcile, a prefix that holds another archive, two 7z
//! start headers that both verify, and a 7z followed by a ZIP end record must
//! fail before anything is published, with an error that names the offset.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{ExtractOptions, extract_archive};

fn data(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(path)
}

fn tree(root: &Path) -> BTreeMap<String, String> {
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
            let kind = if entry.file_type().is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                Sha256::digest(fs::read(entry.path()).unwrap())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            };
            (name, kind)
        })
        .collect()
}

fn extract(archive: &Path) -> (tempfile::TempDir, anyhow::Result<()>) {
    let root = tempfile::tempdir().unwrap();
    let result = extract_archive(
        archive,
        &root.path().join("out"),
        ExtractOptions {
            threads: Some(4),
            ..Default::default()
        },
    );
    (root, result)
}

fn extracted_tree(archive: &Path) -> BTreeMap<String, String> {
    let (root, result) = extract(archive);
    if let Err(error) = result {
        panic!("{}: {error:#}", archive.display());
    }
    tree(&root.path().join("out"))
}

fn assert_refused(archive: &Path, names: &[&str], label: &str) -> String {
    let (root, result) = extract(archive);
    let error = match result {
        Ok(()) => panic!("{label}: extracted"),
        Err(error) => format!("{error:#}"),
    };
    for name in names {
        assert!(error.contains(name), "{label}: {name} not in: {error}");
    }
    let left: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
    assert!(left.is_empty(), "{label}: left {left:?}");
    error
}

fn sevenzip() -> Option<PathBuf> {
    let found = ["7zz", "7z"]
        .into_iter()
        .find(|binary| {
            Command::new(binary)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(PathBuf::from);
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip was found"
    );
    found
}

fn tree_by_7zip(binary: &Path, archive: &Path) -> BTreeMap<String, String> {
    let out = tempfile::tempdir().unwrap();
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(out.path());
    let output = Command::new(binary)
        .args(["x", "-y", "-bd"])
        .arg(flag)
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "7-Zip on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&output.stdout)
    );
    tree(out.path())
}

fn names_by_python(archive: &Path) -> Option<Vec<String>> {
    let output = Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys, zipfile; print('\\n'.join(sorted(zipfile.ZipFile(sys.argv[1]).namelist())))",
        ])
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "Python on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|name| name.trim_end_matches('/').to_owned())
            .collect(),
    )
}

fn stub(length: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9u32;
    let mut bytes = b"MZ".to_vec();
    while bytes.len() < length {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let byte = (state >> 24) as u8;
        bytes.push(if byte == b'P' || byte == b'7' {
            b'Q'
        } else {
            byte
        });
    }
    bytes.truncate(length);
    bytes
}

fn le16(bytes: &[u8], at: usize) -> usize {
    u16::from_le_bytes([bytes[at], bytes[at + 1]]).into()
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn add32(bytes: &mut [u8], at: usize, delta: u32) {
    let value = le32(bytes, at) + delta;
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn adjusted(prefix: &[u8], archive: &[u8], by: u32) -> Vec<u8> {
    let mut bytes = [prefix, archive].concat();
    let end = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    let mut position = le32(&bytes, end + 16) as usize + prefix.len();
    for _ in 0..le16(&bytes, end + 10) {
        assert_eq!(&bytes[position..position + 4], b"PK\x01\x02");
        add32(&mut bytes, position + 42, by);
        position += 46
            + le16(&bytes, position + 28)
            + le16(&bytes, position + 30)
            + le16(&bytes, position + 32);
    }
    add32(&mut bytes, end + 16, by);
    bytes
}

fn written(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = folder.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

const ZIPS: &[&str] = &[
    "zip-corpus/infozip-default.zip",
    "zip-corpus/infozip-descriptors.zip",
    "zip-corpus/7zip-deflate.zip",
    "zip-corpus/python-stored.zip",
    "zip-corpus/python-zip64.zip",
    "zip-corpus/infozip-zip64.zip",
    "zip-corpus/infozip-split-one-piece.zip",
    "parity/archives/zip.zip",
];

#[test]
fn a_zip_after_a_stub_extracts_as_the_zip_alone_whether_or_not_its_offsets_count_the_stub() {
    let seven = sevenzip();
    let folder = tempfile::tempdir().unwrap();
    for name in ZIPS {
        let archive = fs::read(data(name)).unwrap();
        let whole = extracted_tree(&data(name));
        for length in [2, 3, 4, 5, 4096, 70_000] {
            let prefix = stub(length);
            let mut cases = Vec::new();
            if !name.contains("one-piece") {
                cases.push(("prefixed", [&prefix[..], &archive].concat()));
            }
            if !name.contains("zip64") {
                cases.push(("adjusted", adjusted(&prefix, &archive, length as u32)));
            }
            for (kind, bytes) in cases {
                let label = format!("{name} {kind} after {length} bytes");
                let path = written(folder.path(), "sfx.exe", &bytes);
                let ours = extracted_tree(&path);
                assert_eq!(ours, whole, "{label}");
                if let Some(names) = names_by_python(&path) {
                    let files: Vec<&String> = ours
                        .iter()
                        .filter(|(name, kind)| name.is_ascii() && *kind != "dir")
                        .map(|(name, _)| name)
                        .collect();
                    assert!(
                        names
                            .iter()
                            .filter(|name| name.is_ascii())
                            .all(|name| ours.contains_key(name))
                            && files.iter().all(|file| names.contains(file)),
                        "{label}: Python lists {names:?}"
                    );
                }
                if let Some(binary) = &seven {
                    assert_eq!(tree_by_7zip(binary, &path), ours, "{label}");
                }
            }
        }
    }
}

#[test]
fn info_zip_adjusts_a_stub_that_zipper_then_reads() {
    if !Command::new("zip")
        .arg("-v")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let archive = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let path = written(
        folder.path(),
        "sfx.exe",
        &[&stub(5000)[..], &archive].concat(),
    );
    let status = Command::new("zip")
        .arg("-q")
        .arg("-A")
        .arg(&path)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_ne!(fs::read(&path).unwrap()[5000..], archive[..]);
    assert_eq!(
        extracted_tree(&path),
        extracted_tree(&data("zip-corpus/infozip-default.zip"))
    );
}

#[test]
fn a_prefix_that_hides_another_archive_is_refused_by_offset() {
    let folder = tempfile::tempdir().unwrap();
    let main = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let other = fs::read(data("parity/archives/zip.zip")).unwrap();
    let empty: &[u8] = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
    let seven_zip = fs::read(data("parity/archives/7z.7z")).unwrap();
    let head = stub(1000);
    for (label, prefix, at) in [
        ("a whole ZIP", other.clone(), "offset 0"),
        (
            "a stub and a ZIP",
            [&head[..], &other].concat(),
            "offset 1000",
        ),
        ("an empty ZIP", [&head[..], empty].concat(), "offset 1000"),
        ("a 7z", [&head[..], &seven_zip].concat(), "offset 1000"),
        (
            "a CAB in a stub",
            [&head[..], b"MSCF\0\0\0\0", &head[..]].concat(),
            "offset 1000",
        ),
        (
            "a RAR in a stub",
            [&head[..], b"Rar!\x1A\x07\x01\x00", &head[..]].concat(),
            "offset 1000",
        ),
        ("an ar archive", b"!<arch>\n".repeat(20), "offset 0"),
        (
            "a cpio archive",
            [&b"070701"[..], &[b'0'; 200]].concat(),
            "offset 0",
        ),
        ("zeros, as an ISO starts", vec![0; 40_000], "offset 0"),
    ] {
        let length = prefix.len() as u32;
        for (kind, bytes) in [
            ("prefixed", [&prefix[..], &main].concat()),
            ("adjusted", adjusted(&prefix, &main, length)),
        ] {
            let path = written(folder.path(), "hidden.exe", &bytes);
            assert_refused(&path, &[at], &format!("{label} {kind}"));
        }
    }
}

#[test]
fn offsets_that_count_part_of_the_prefix_are_refused() {
    let folder = tempfile::tempdir().unwrap();
    let main = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let prefix = stub(1000);
    for by in [1, 500, 999, 1001] {
        let path = written(folder.path(), "partly.exe", &adjusted(&prefix, &main, by));
        let named = if by > 1000 {
            "central-directory extent"
        } else {
            "offset 1000"
        };
        assert_refused(&path, &[named], &format!("adjusted by {by} of 1000"));
    }
}

#[test]
fn a_directory_that_moves_onto_its_own_first_record_is_refused_by_offset() {
    let folder = tempfile::tempdir().unwrap();
    let mut main = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let end = main
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    let start = le32(&main, end + 16) - 2;
    main[end + 16..end + 20].copy_from_slice(&start.to_le_bytes());
    let path = written(folder.path(), "moved.zip", &main);
    assert_refused(&path, &["ZIP local header at offset 0"], "moved by 2");
}

#[test]
fn a_zip64_locator_that_moves_by_another_amount_than_the_directory_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let main = fs::read(data("zip-corpus/infozip-zip64.zip")).unwrap();
    let prefix = stub(1000);
    let mut bytes = [&prefix[..], &main].concat();
    let locator = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x06\x07")
        .unwrap();
    let offset = u64::from_le_bytes(bytes[locator + 8..locator + 16].try_into().unwrap()) + 1000;
    bytes[locator + 8..locator + 16].copy_from_slice(&offset.to_le_bytes());
    let path = written(folder.path(), "locator.exe", &bytes);
    assert_refused(
        &path,
        &["ZIP64 locator", "1000 bytes"],
        "locator moved apart",
    );
}

#[test]
fn an_empty_or_encrypted_zip_after_a_stub_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let empty: &[u8] = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
    let path = written(
        folder.path(),
        "empty.exe",
        &[&stub(100)[..], empty].concat(),
    );
    assert_refused(&path, &["100 bytes"], "empty");
    let encrypted = fs::read(data("parity/archives/zip-zipcrypto.zip"));
    if let Ok(encrypted) = encrypted {
        let path = written(
            folder.path(),
            "locked.exe",
            &[&stub(100)[..], &encrypted].concat(),
        );
        assert_refused(&path, &["encrypted"], "encrypted");
    }
}

const SEVEN_ZIPS: &[&str] = &[
    "7z-corpus/7zip-lzma2-long.7z",
    "7z-corpus/7zip-bcj2-short.7z",
    "7z-corpus/7zip-nonsolid-links.7z",
    "7z-corpus/7zip-plain-header-short.7z",
    "7z-corpus/bsdtar-store-short.7z",
    "parity/archives/7z-copy.7z",
];

const SEARCH_LIMIT: usize = 8 << 20;

fn with_magic(magic: &[u8], length: usize) -> Vec<u8> {
    let mut bytes = stub(length);
    bytes[..magic.len()].copy_from_slice(magic);
    bytes
}

#[test]
fn a_7z_after_a_stub_extracts_as_the_7z_alone_and_as_7zip_extracts_it() {
    let seven = sevenzip();
    let folder = tempfile::tempdir().unwrap();
    for name in SEVEN_ZIPS {
        let archive = fs::read(data(name)).unwrap();
        let whole = extracted_tree(&data(name));
        for (kind, prefix) in [
            ("PE", stub(5000)),
            ("ELF", with_magic(b"\x7fELF", 70_000)),
            ("Mach-O", with_magic(b"\xcf\xfa\xed\xfe", 300)),
            ("PE at the limit", stub(SEARCH_LIMIT)),
        ] {
            let label = format!("{name} after a {kind} stub");
            let path = written(folder.path(), "sfx.exe", &[&prefix[..], &archive].concat());
            assert_eq!(extracted_tree(&path), whole, "{label}");
            if let Some(binary) = &seven {
                assert_eq!(tree_by_7zip(binary, &path), whole, "{label}");
            }
        }
    }
}

#[test]
fn a_7z_signature_past_the_search_limit_or_after_no_executable_is_not_found() {
    let folder = tempfile::tempdir().unwrap();
    let archive = fs::read(data("parity/archives/7z.7z")).unwrap();
    let far = written(
        folder.path(),
        "far.exe",
        &[&stub(SEARCH_LIMIT + 1)[..], &archive].concat(),
    );
    assert_refused(&far, &[], "past the limit");
    let mut text = b"#!/bin/sh\n".to_vec();
    text.resize(4000, b'#');
    let script = written(folder.path(), "script.sh", &[&text[..], &archive].concat());
    assert_refused(&script, &[], "after a script");
}

#[test]
fn two_7z_signatures_that_both_verify_are_refused_by_offset() {
    let folder = tempfile::tempdir().unwrap();
    let first = fs::read(data("parity/archives/7z.7z")).unwrap();
    let second = fs::read(data("7z-corpus/7zip-lzma2-short.7z")).unwrap();
    let head = stub(1000);
    let path = written(
        folder.path(),
        "two.exe",
        &[&head[..], &first, &second].concat(),
    );
    let second_at = format!("offset {}", 1000 + first.len());
    assert_refused(&path, &["offset 1000", &second_at], "two archives");
    let mut damaged = first.clone();
    damaged[8] ^= 1;
    let path = written(
        folder.path(),
        "damaged-first.exe",
        &[&head[..], &damaged, &second].concat(),
    );
    assert_eq!(
        extracted_tree(&path),
        extracted_tree(&data("7z-corpus/7zip-lzma2-short.7z"))
    );
}

#[test]
fn a_7z_after_a_stub_that_is_also_a_zip_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let path = written(
        folder.path(),
        "both.exe",
        &[&stub(1000)[..], &seven, &zip].concat(),
    );
    assert_refused(&path, &["offset 1000"], "7z then ZIP");
    let small = fs::read(data("parity/archives/zip.zip")).unwrap();
    let path = written(
        folder.path(),
        "zip-in-stub.exe",
        &[&stub(1000)[..], &small, &seven].concat(),
    );
    let at = format!("offset {}", 1000 + small.len());
    assert_refused(&path, &[&at], "ZIP in the stub, then 7z");
    let padded = [&stub(1000)[..], &seven, &zip, &[0; 65536]].concat();
    let path = written(folder.path(), "zip-in-the-tail.exe", &padded);
    assert_refused(&path, &["offset 1000"], "ZIP 65558 bytes from the end");
    let Some(binary) = sevenzip() else {
        return;
    };
    let source = folder.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), b"in the 7z\n").unwrap();
    fs::copy(data("parity/archives/zip.zip"), source.join("inner.zip")).unwrap();
    let holding = folder.path().join("holding.7z");
    let status = Command::new(binary)
        .args(["a", "-bd", "-mx0", "-ms=off"])
        .arg(&holding)
        .arg(source.join("a.txt"))
        .arg(source.join("inner.zip"))
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = [&stub(1000)[..], &fs::read(&holding).unwrap()].concat();
    let path = written(folder.path(), "holding.exe", &bytes);
    assert_refused(&path, &["offset 1000"], "7z that stores a ZIP");
}
