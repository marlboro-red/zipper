//! Self-extracting archives, an executable stub followed by a ZIP or a 7z.
//! Info-ZIP `zip -A` adjusts a ZIP's offsets to count the stub, and a plain
//! `cat stub a.zip` leaves them counting from the ZIP. Both must extract the
//! tree the ZIP gives alone, the names Python `zipfile` lists and the tree
//! 7-Zip extracts. The archive must start exactly where the stub
//! executable's own data ends, and no range its headers name may reach the
//! archive. A 7z whose signature starts there, at offset 8 MiB or earlier,
//! after a PE, ELF or Mach-O stub must extract as it does alone and as 7-Zip
//! extracts it, and a start header that fails its CRC is skipped. A 7z past
//! that offset or after a prefix that is no executable is not found. A prefix
//! whose offsets do not reconcile, a prefix that holds another archive, two 7z
//! start headers that both verify, and a 7z followed by a ZIP end record must
//! fail before anything is published, with an error that names the offset.
#[path = "common/sevenzip.rs"]
mod sevenzip;
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

#[cfg(unix)]
fn cpu_time() -> std::time::Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec for the duration of the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
    assert_eq!(status, 0, "clock_gettime failed");
    std::time::Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

#[cfg(not(unix))]
fn cpu_time() -> std::time::Duration {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
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
            "import sys, zipfile; sys.stdout.buffer.write('\\n'.join(sorted(zipfile.ZipFile(sys.argv[1]).namelist())).encode())",
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

fn noise(length: usize) -> Vec<u8> {
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

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

fn stub(length: usize) -> Vec<u8> {
    let mut bytes = noise(length.max(PE_DATA));
    bytes[PE_HEADER..PE_DATA].fill(0);
    put(&mut bytes, PE_POINTER, &(PE_HEADER as u32).to_le_bytes());
    put(&mut bytes, PE_HEADER, b"PE\0\0");
    put(&mut bytes, PE_HEADER + 6, &1u16.to_le_bytes());
    put(
        &mut bytes,
        PE_HEADER + 20,
        &(PE_OPTIONAL as u16).to_le_bytes(),
    );
    put(&mut bytes, PE_OPTIONAL_START, &0x10Bu16.to_le_bytes());
    put(&mut bytes, PE_DIRECTORIES - 4, &16u32.to_le_bytes());
    put(
        &mut bytes,
        PE_SECTION + 8,
        &((length - PE_DATA) as u32).to_le_bytes(),
    );
    put(
        &mut bytes,
        PE_SECTION + 12,
        &(PE_SECTION_ADDRESS as u32).to_le_bytes(),
    );
    put(
        &mut bytes,
        PE_SECTION + 16,
        &((length - PE_DATA) as u32).to_le_bytes(),
    );
    put(&mut bytes, PE_SECTION + 20, &(PE_DATA as u32).to_le_bytes());
    bytes.truncate(length);
    bytes
}

fn with_directory(mut bytes: Vec<u8>, directory: usize, address: usize, size: usize) -> Vec<u8> {
    let at = PE_DIRECTORIES + directory * 8;
    put(&mut bytes, at, &(address as u32).to_le_bytes());
    put(&mut bytes, at + 4, &(size as u32).to_le_bytes());
    bytes
}

fn elf_stub(length: usize) -> Vec<u8> {
    let mut bytes = noise(length);
    bytes[..ELF_DATA].fill(0);
    put(&mut bytes, 0, b"\x7fELF\x02\x01\x01");
    put(&mut bytes, 32, &64u64.to_le_bytes());
    put(&mut bytes, 54, &56u16.to_le_bytes());
    put(&mut bytes, 56, &1u16.to_le_bytes());
    put(&mut bytes, 64, &1u32.to_le_bytes());
    put(&mut bytes, 96, &(length as u64).to_le_bytes());
    bytes
}

fn elf_stub_with_null_segment(length: usize, start: usize, size: usize) -> Vec<u8> {
    let mut bytes = elf_stub(length);
    put(&mut bytes, 56, &2u16.to_le_bytes());
    let null = 64 + 56;
    bytes[null..null + 56].fill(0);
    put(&mut bytes, null + 8, &(start as u64).to_le_bytes());
    put(&mut bytes, null + 32, &(size as u64).to_le_bytes());
    bytes
}

fn macho_stub_with_symbols(length: usize, start: usize) -> Vec<u8> {
    let mut bytes = macho_stub(length);
    put(&mut bytes, 16, &2u32.to_le_bytes());
    put(&mut bytes, 20, &(72u32 + 24).to_le_bytes());
    let symtab = 32 + 72;
    bytes[symtab..symtab + 24].fill(0);
    put(&mut bytes, symtab, &2u32.to_le_bytes());
    put(&mut bytes, symtab + 4, &24u32.to_le_bytes());
    put(&mut bytes, symtab + 8, &(start as u32).to_le_bytes());
    put(&mut bytes, symtab + 12, &2u32.to_le_bytes());
    bytes
}

fn macho_stub(length: usize) -> Vec<u8> {
    let mut bytes = noise(length);
    bytes[..MACHO_DATA].fill(0);
    put(&mut bytes, 0, b"\xcf\xfa\xed\xfe");
    put(&mut bytes, 16, &1u32.to_le_bytes());
    put(&mut bytes, 20, &72u32.to_le_bytes());
    put(&mut bytes, 32, &0x19u32.to_le_bytes());
    put(&mut bytes, 36, &72u32.to_le_bytes());
    put(&mut bytes, 32 + 48, &(length as u64).to_le_bytes());
    bytes
}

fn fat_stub(length: usize) -> Vec<u8> {
    let mut bytes = noise(length);
    bytes[..FAT_DATA].fill(0);
    put(&mut bytes, 0, b"\xca\xfe\xba\xbe");
    put(&mut bytes, 4, &1u32.to_be_bytes());
    put(&mut bytes, 16, &(FAT_DATA as u32).to_be_bytes());
    put(&mut bytes, 20, &((length - FAT_DATA) as u32).to_be_bytes());
    bytes
}

const PE_POINTER: usize = 0x3C;
const PE_HEADER: usize = 0x40;
const PE_SIGNATURE_END: usize = PE_HEADER + 4;
const PE_OPTIONAL_START: usize = PE_HEADER + 24;
const PE_OPTIONAL: usize = 96 + 16 * 8;
const PE_DIRECTORIES: usize = PE_OPTIONAL_START + 96;
const PE_SECTION: usize = PE_OPTIONAL_START + PE_OPTIONAL;
const PE_DATA: usize = PE_SECTION + 40;
const PE_SECTION_ADDRESS: usize = 0x1000;
const PE_CERTIFICATES: usize = 4;
const PE_DEBUG: usize = 6;
const ELF_DATA: usize = 64 + 56;
const MACHO_DATA: usize = 32 + 72;
const FAT_DATA: usize = 28;

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
    let seven = sevenzip::sevenzip();
    let folder = tempfile::tempdir().unwrap();
    for name in ZIPS {
        let archive = fs::read(data(name)).unwrap();
        let whole = extracted_tree(&data(name));
        for length in [PE_DATA, PE_DATA + 1, 4096, 70_000] {
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
        (
            "an ar archive, which the ar reader refuses",
            b"!<arch>\n".repeat(20),
            "ar member header",
        ),
        (
            "a cpio archive, which the cpio reader refuses",
            [&b"070701"[..], &[b'0'; 200]].concat(),
            "cpio name size",
        ),
        ("zeros, as an ISO starts", vec![0; 40_000], "no whole PE"),
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
        &[&stub(1000)[..], empty].concat(),
    );
    assert_refused(&path, &["1000 bytes"], "empty");
    let encrypted = fs::read(data("parity/archives/zip-zipcrypto.zip"));
    if let Ok(encrypted) = encrypted {
        let path = written(
            folder.path(),
            "locked.exe",
            &[&stub(1000)[..], &encrypted].concat(),
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
    let seven = sevenzip::sevenzip();
    let folder = tempfile::tempdir().unwrap();
    for name in SEVEN_ZIPS {
        let archive = fs::read(data(name)).unwrap();
        let whole = extracted_tree(&data(name));
        for (kind, prefix) in [
            ("PE", stub(5000)),
            ("ELF", elf_stub(70_000)),
            ("Mach-O", macho_stub(300)),
            ("fat Mach-O", fat_stub(5000)),
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
    assert_refused(
        &path,
        &["7z signature at offset 1000"],
        "a damaged 7z in the stub",
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
    assert_refused(&path, &[], "ZIP in the stub, then 7z");
    let padded = [&stub(1000)[..], &seven, &zip, &[0; 65536]].concat();
    let path = written(folder.path(), "zip-in-the-tail.exe", &padded);
    assert_refused(&path, &["offset 1000"], "ZIP 65558 bytes from the end");
    let Some(binary) = sevenzip::sevenzip() else {
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

fn refused_before_a_zip_and_a_7z(prefix: &[u8], names: &[&str], label: &str) {
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    let length = prefix.len() as u32;
    for (kind, bytes) in [
        ("ZIP", [prefix, &zip].concat()),
        ("adjusted ZIP", adjusted(prefix, &zip, length)),
        ("7z", [prefix, &seven].concat()),
    ] {
        let path = written(folder.path(), "stub.exe", &bytes);
        assert_refused(&path, names, &format!("{label} before a {kind}"));
    }
}

#[test]
fn a_pe_header_pointer_must_land_inside_the_stub() {
    use std::io::Write;
    let folder = tempfile::tempdir().unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("header.bin", stored).unwrap();
    zip.write_all(b"PE\0\0 inside the archive").unwrap();
    let zip = zip.finish().unwrap().into_inner();
    let inside = zip
        .windows(4)
        .position(|window| window == b"PE\0\0")
        .unwrap();
    let mut prefix = stub(1000);
    prefix[PE_POINTER..PE_POINTER + 4].copy_from_slice(&((1000 + inside) as u32).to_le_bytes());
    let path = written(folder.path(), "pointer.exe", &[&prefix[..], &zip].concat());
    assert_refused(&path, &["no whole PE"], "a PE pointer into the archive");
}

#[test]
fn a_stub_without_a_whole_executable_header_is_refused() {
    let mut pointer_past_the_stub = stub(1000);
    pointer_past_the_stub[PE_POINTER..PE_POINTER + 4].copy_from_slice(&5000u32.to_le_bytes());
    let mut no_pe_signature = stub(1000);
    no_pe_signature[PE_HEADER..PE_SIGNATURE_END].copy_from_slice(b"\0\0\0\0");
    let mut iso_with_mz = vec![0u8; 0x9000];
    iso_with_mz[..2].copy_from_slice(b"MZ");
    for (label, prefix) in [
        ("MZ with no PE header", no_pe_signature),
        ("two PE sections over the same bytes", {
            let mut bytes = stub(1000);
            put(&mut bytes, PE_HEADER + 6, &2u16.to_le_bytes());
            let first = bytes[PE_SECTION..PE_SECTION + 40].to_vec();
            put(&mut bytes, PE_SECTION + 40, &first);
            bytes
        }),
        ("a PE optional header with no known magic", {
            let mut bytes = stub(1000);
            put(&mut bytes, PE_OPTIONAL_START, &0u16.to_le_bytes());
            bytes
        }),
        ("a PE debug directory that is no whole number of entries", {
            let mut bytes = with_directory(stub(1000), PE_DEBUG, PE_SECTION_ADDRESS + 100, 30);
            put(&mut bytes, PE_DATA + 100, &[0; 30]);
            bytes
        }),
        ("Mach-O load commands longer than they declare", {
            let mut bytes = macho_stub(1000);
            put(&mut bytes, 20, &8u32.to_le_bytes());
            bytes
        }),
        ("a PE with no sections", {
            let mut bytes = stub(1000);
            put(&mut bytes, PE_HEADER + 6, &0u16.to_le_bytes());
            bytes
        }),
        ("a PE pointer past the stub", pointer_past_the_stub),
        ("MZ over an empty system area", iso_with_mz),
        (
            "an ELF ident of version 0",
            with_magic(b"\x7fELF\x02\x01\x00", 1000),
        ),
        (
            "a Java class",
            with_magic(b"\xca\xfe\xba\xbe\0\0\0\x34", 5000),
        ),
    ] {
        refused_before_a_zip_and_a_7z(&prefix, &["no whole PE, ELF or Mach-O header"], label);
    }
}

#[test]
fn every_archive_signature_in_a_stub_is_refused_before_a_zip_or_a_7z() {
    let tail = &noise(600)[2..];
    for signature in [
        &b"MSCF\0\0\0\0"[..],
        b"Rar!\x1A\x07\x00",
        b"Rar!\x1A\x07\x01\x00",
        b"\xEF\xBE\xAD\xDENullsoftInst",
    ] {
        let prefix = [&stub(1000)[..], signature, tail].concat();
        refused_before_a_zip_and_a_7z(&prefix, &["offset 1000"], &format!("{signature:?}"));
    }
    for (at, identifier) in [(0x8001, b"CD001"), (0x8801, b"BEA01"), (0x9001, b"NSR02")] {
        let mut prefix = stub(0xA000);
        prefix[at..at + 5].copy_from_slice(identifier);
        let named = format!("offset {at}");
        refused_before_a_zip_and_a_7z(&prefix, &[&named], &format!("{identifier:?}"));
    }
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    let prefix = [&stub(1000)[..], &seven, tail].concat();
    let path = written(folder.path(), "7z-stub.exe", &[&prefix[..], &zip].concat());
    assert_refused(&path, &["offset 1000"], "a 7z in a ZIP's stub");
}

#[test]
fn a_self_extracting_zip_that_stores_a_7z_reads_as_the_zip() {
    use std::io::Write;
    let folder = tempfile::tempdir().unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("payload.7z", stored).unwrap();
    zip.write_all(&fs::read(data("parity/archives/7z.7z")).unwrap())
        .unwrap();
    zip.start_file("readme.txt", stored).unwrap();
    zip.write_all(b"outside the 7z\n").unwrap();
    let zip = zip.finish().unwrap().into_inner();
    let path = written(
        folder.path(),
        "stores.exe",
        &[&stub(1000)[..], &zip].concat(),
    );
    let tree = extracted_tree(&path);
    assert_eq!(
        tree.keys().collect::<Vec<_>>(),
        ["payload.7z", "readme.txt"],
        "{tree:?}"
    );
}

#[cfg(unix)]
fn with_an_unknown_method(archive: &[u8]) -> Vec<u8> {
    let mut patched = archive.to_vec();
    let header_start = 32 + u64::from_le_bytes(patched[12..20].try_into().unwrap()) as usize;
    let coder = header_start
        + patched[header_start..]
            .windows(4)
            .position(|window| window == [0x01, 0x21, 0x21, 0x01])
            .unwrap();
    patched[coder + 2] = 0x7F;
    let header_crc = crc32fast::hash(&patched[header_start..]);
    patched[28..32].copy_from_slice(&header_crc.to_le_bytes());
    let start_crc = crc32fast::hash(&patched[12..32]);
    patched[8..12].copy_from_slice(&start_crc.to_le_bytes());
    patched
}

#[cfg(unix)]
#[test]
fn a_7z_zipper_does_not_decode_after_a_stub_reaches_7zip_pinned_as_7z_and_only_after_its_stub_passes()
 {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    let backend = folder.path().join("backend");
    fs::write(
        &backend,
        "#!/bin/sh\necho \"$*\" >> \"$0.log\"\nfor a; do case $a in -o*) d=${a#-o};; esac; done\nmkdir -p \"$d\"\n",
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
    let undecoded =
        with_an_unknown_method(&fs::read(data("7z-corpus/7zip-plain-header-short.7z")).unwrap());
    let cab = [&stub(1000)[..], b"MSCF\0\0\0\0", &noise(600)[2..]].concat();
    for (name, prefix, reaches) in [("clean.exe", stub(1000), true), ("cab.exe", cab, false)] {
        let path = written(folder.path(), name, &[&prefix[..], &undecoded].concat());
        let log = folder.path().join("backend.log");
        let _ = fs::remove_file(&log);
        let output = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["extract", "-o"])
            .arg(folder.path().join(format!("out-{name}")))
            .arg(&path)
            .env("ZIPPER_7Z", &backend)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let logged = fs::read_to_string(&log).unwrap_or_default();
        if reaches {
            assert!(logged.contains("-t7z "), "{name}: {logged}");
        } else {
            assert!(logged.is_empty(), "{name} reached 7-Zip: {logged}");
            assert!(!output.status.success(), "{name}");
        }
    }
}

fn ustar_block() -> Vec<u8> {
    let mut block = vec![0u8; 512];
    put(&mut block, 0, b"from-tar.txt");
    put(&mut block, 257, b"ustar\x0000");
    block
}

fn uefi_volume() -> Vec<u8> {
    let mut volume = vec![0xFFu8; 0x1000];
    volume[..0x48].fill(0);
    put(&mut volume, 40, b"_FVH");
    volume
}

fn iso_at_its_start() -> Vec<u8> {
    let mut image = vec![0u8; 0x8800];
    put(&mut image, 0x8000, b"\x01CD001\x01");
    image
}

#[test]
fn an_archive_that_does_not_start_where_the_executable_ends_is_refused() {
    let blobs: [(&str, Vec<u8>); 9] = [
        ("one byte", vec![0]),
        ("512 bytes", vec![0; 512]),
        ("a tar block", ustar_block()),
        (
            "a cpio header",
            [&b"070701"[..], &[b'0'; 104], b"TRAILER!!!\0"].concat(),
        ),
        ("an LHA header", b"\x20\x00-lh0-\0\0\0\0".to_vec()),
        ("an ARJ header", b"\x60\xEA\x1E\x00".to_vec()),
        ("a UEFI firmware volume", uefi_volume()),
        ("an ISO", iso_at_its_start()),
        ("a xar header", b"xar!\0\x1C\0\x01".to_vec()),
    ];
    for (label, blob) in blobs {
        let prefix = [&stub(1000)[..], &blob].concat();
        refused_before_a_zip_and_a_7z(
            &prefix,
            &["ends at offset 1000"],
            &format!("PE then {label}"),
        );
    }
    for (label, executable) in [
        ("ELF", elf_stub(1000)),
        ("Mach-O", macho_stub(1000)),
        ("fat Mach-O", fat_stub(1000)),
    ] {
        let prefix = [&executable[..], &ustar_block()].concat();
        refused_before_a_zip_and_a_7z(
            &prefix,
            &["ends at offset 1000"],
            &format!("{label} then a tar block"),
        );
    }
}

#[test]
fn the_archive_at_the_overlay_decides_whatever_else_the_stub_holds() {
    let folder = tempfile::tempdir().unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    let mut prefix = stub(1000);
    put(&mut prefix, 500, b"PK\x03\x04");
    let path = written(
        folder.path(),
        "pk-then-7z.exe",
        &[&prefix[..], &seven].concat(),
    );
    assert_eq!(
        extracted_tree(&path),
        extracted_tree(&data("parity/archives/7z.7z"))
    );
}

#[cfg(unix)]
#[test]
fn a_real_executable_stub_works_only_with_the_archive_at_its_end() {
    let stub = Path::new("/usr/bin/true");
    let Ok(stub) = fs::read(stub) else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    for (name, archive, alone) in [
        ("true-zip.exe", &zip, "zip-corpus/infozip-default.zip"),
        ("true-7z.exe", &seven, "parity/archives/7z.7z"),
    ] {
        let path = written(folder.path(), name, &[&stub[..], archive].concat());
        assert_eq!(
            extracted_tree(&path),
            extracted_tree(&data(alone)),
            "{name}"
        );
        let padded = written(
            folder.path(),
            &format!("padded-{name}"),
            &[&stub[..], b"\0", archive].concat(),
        );
        let at = format!("ends at offset {}", stub.len());
        assert_refused(&padded, &[&at], &format!("padded {name}"));
    }
}

fn macho_bomb(commands: u32, sections: u32) -> Vec<u8> {
    let mut command = vec![0x41u8; 72];
    put(&mut command, 0, &0x19u32.to_le_bytes());
    put(&mut command, 4, &72u32.to_le_bytes());
    put(&mut command, 40, &0u64.to_le_bytes());
    put(&mut command, 48, &0u64.to_le_bytes());
    put(&mut command, 64, &sections.to_le_bytes());
    let mut bytes = b"\xcf\xfa\xed\xfe".to_vec();
    for value in [0x0100_000Cu32, 0, 2, commands, 72 * commands, 0, 0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for _ in 0..commands {
        bytes.extend_from_slice(&command);
    }
    bytes.resize(bytes.len() + 72 + 80 * sections as usize, 0x41);
    bytes
}

fn elf_bomb(program_size: u16, count: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    put(&mut bytes, 0, b"\x7fELF\x02\x01\x01");
    put(&mut bytes, 32, &64u64.to_le_bytes());
    put(&mut bytes, 54, &program_size.to_le_bytes());
    put(&mut bytes, 56, &count.to_le_bytes());
    let mut program = vec![0u8; 56];
    put(&mut program, 0, &1u32.to_le_bytes());
    put(&mut program, 32, &64u64.to_le_bytes());
    for _ in 0..count {
        bytes.extend_from_slice(&program[..usize::from(program_size)]);
    }
    bytes.resize(bytes.len().max(64 + 56), 0);
    bytes
}

#[test]
fn header_bombs_are_refused_quickly() {
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    let seven = fs::read(data("parity/archives/7z.7z")).unwrap();
    for (label, prefix, named) in [
        (
            "4096 segments of 4096 sections",
            macho_bomb(4096, 4096),
            "no whole PE, ELF or Mach-O header",
        ),
        (
            "1024 segments of 1024 sections",
            macho_bomb(1024, 1024),
            "no whole PE, ELF or Mach-O header",
        ),
        (
            "65535 program headers of size 0",
            elf_bomb(0, 65535),
            "no whole PE, ELF or Mach-O header",
        ),
        (
            "65535 program headers inside the stub",
            elf_bomb(56, 65535),
            "more than 65536 file ranges",
        ),
    ] {
        for (kind, archive) in [("ZIP", &zip), ("7z", &seven)] {
            let path = written(folder.path(), "bomb.exe", &[&prefix[..], archive].concat());
            let started = cpu_time();
            assert_refused(&path, &[named], &format!("{label} before a {kind}"));
            let spent = cpu_time() - started;
            assert!(
                spent > std::time::Duration::ZERO && spent < std::time::Duration::from_secs(1),
                "{label} before a {kind} took {spent:?} of this thread's CPU time"
            );
        }
    }
}

#[test]
fn a_header_range_that_reaches_the_archive_is_refused_by_name() {
    let mut debug = with_directory(stub(1000), PE_DEBUG, PE_SECTION_ADDRESS + 100, 28);
    put(&mut debug, PE_DATA + 100 + 16, &8u32.to_le_bytes());
    put(&mut debug, PE_DATA + 100 + 24, &1000u32.to_le_bytes());
    for (label, prefix, named) in [
        (
            "a certificate table over the archive",
            with_directory(stub(1000), PE_CERTIFICATES, 1000, 8),
            "PE certificate table at offset 1000",
        ),
        (
            "a certificate table past the archive's start",
            with_directory(stub(1000), PE_CERTIFICATES, 1001, 64),
            "PE certificate table at offset 1001",
        ),
        (
            "a debug entry over the archive",
            debug,
            "PE debug entry's raw data at offset 1000",
        ),
        (
            "an ELF null segment over the archive",
            elf_stub_with_null_segment(1000, 1000, 35),
            "ELF segment at offset 1000",
        ),
        (
            "a PE section table past the stub",
            {
                let mut bytes = stub(1000);
                put(&mut bytes, PE_HEADER + 6, &96u16.to_le_bytes());
                bytes
            },
            "PE section table at offset",
        ),
        (
            "Mach-O symbols over the archive",
            macho_stub_with_symbols(1000, 1000),
            "Mach-O symbol table at offset 1000",
        ),
    ] {
        refused_before_a_zip_and_a_7z(&prefix, &[named], label);
    }
}

#[test]
fn header_ranges_that_end_inside_the_stub_are_accepted() {
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(data("zip-corpus/infozip-default.zip")).unwrap();
    for (label, prefix) in [
        ("a certificate table at the stub's end", {
            let mut bytes = stub(1064);
            put(
                &mut bytes,
                PE_SECTION + 8,
                &((1000 - PE_DATA) as u32).to_le_bytes(),
            );
            put(
                &mut bytes,
                PE_SECTION + 16,
                &((1000 - PE_DATA) as u32).to_le_bytes(),
            );
            with_directory(bytes, PE_CERTIFICATES, 1000, 64)
        }),
        (
            "an ELF null segment inside the stub",
            elf_stub_with_null_segment(1000, 500, 35),
        ),
        (
            "Mach-O symbols inside the stub",
            macho_stub_with_symbols(1000, 500),
        ),
    ] {
        let path = written(folder.path(), "inside.exe", &[&prefix[..], &zip].concat());
        assert_eq!(
            extracted_tree(&path),
            extracted_tree(&data("zip-corpus/infozip-default.zip")),
            "{label}"
        );
    }
}
