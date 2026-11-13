//! Damaged RAR archives are refused, and leave nothing behind. Every cut of
//! a RAR 5 corpus archive is refused, and a cut of a RAR 4 one is refused or
//! extracts only entries of the original tree. Every one-bit flip is refused
//! or, where it lands on bytes no check covers, extracts exactly the original
//! tree, and a stored archive is never extracted where 7-Zip refuses it.
//! Hand-built headers cover the refusals the corpus does not reach.
#[path = "common/sevenzip.rs"]
mod sevenzip;

use std::{fs, path::Path, process::Command};
use zipper::native_rar::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rar-corpus");

const STORED: &[&str] = &[
    "test_read_format_rar5_stored.rar",
    "test_read_format_rar5_symlink.rar",
    "test_read_format_rar5_hardlink.rar",
    "test_read_format_rar5_fileattr.rar",
];

const RAR4: &[&str] = &[
    "test_read_format_rar.rar",
    "test_read_format_rar_compress_normal.rar",
    "test_read_format_rar_unicode.rar",
    "test_read_format_rar_windows.rar",
];

const COMPRESSED: &[&str] = &[
    "test_read_format_rar5_compressed.rar",
    "test_read_format_rar5_blake2.rar",
    "test_read_format_rar5_multiple_files_solid.rar",
    "test_read_format_rar5_solid.rar",
    "test_read_format_rar5_win32.rar",
];

fn verifies(bytes: &[u8]) -> bool {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.rar");
    fs::write(&archive, bytes).unwrap();
    read_archive(&archive, None, ReadOptions::default()).is_ok()
}

fn extracted(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.rar");
    fs::write(&archive, bytes).unwrap();
    let out = work.path().join("out");
    let result = read_archive(&archive, Some(&out), ReadOptions::default());
    let found = result.is_ok().then(|| listing(&out));
    if found.is_none() {
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1, "left output");
    }
    make_deletable(work.path());
    found
}

fn listing(root: &Path) -> Vec<(String, Vec<u8>)> {
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
                .replace(std::path::MAIN_SEPARATOR, "/");
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let bytes = if metadata.file_type().is_symlink() {
                fs::read_link(entry.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
                    .as_bytes()
                    .to_vec()
            } else if metadata.is_file() {
                read_unlocked(entry.path())
            } else {
                b"dir".to_vec()
            };
            (name, bytes)
        })
        .collect()
}

fn read_unlocked(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o600)).unwrap();
    }
    fs::read(path).unwrap()
}

fn make_deletable(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if !entry.path_is_symlink() {
            let mode = entry.metadata().unwrap().permissions().mode() | 0o700;
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

fn sevenzip_accepts(binary: &Path, bytes: &[u8]) -> bool {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.rar");
    fs::write(&archive, bytes).unwrap();
    Command::new(binary)
        .args(["t", "-p-"])
        .arg(&archive)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn every_cut_of_every_corpus_archive_is_refused() {
    for name in STORED.iter().chain(COMPRESSED) {
        let bytes = fs::read(Path::new(CORPUS).join(name)).unwrap();
        assert!(verifies(&bytes), "{name}");
        for length in 0..bytes.len() {
            assert!(!verifies(&bytes[..length]), "{name} cut to {length}");
        }
        assert!(extracted(&bytes[..bytes.len() / 2]).is_none(), "{name}");
    }
}

#[test]
fn every_rar4_cut_is_refused_or_extracts_entries_of_the_original_tree() {
    for name in RAR4 {
        let bytes = fs::read(Path::new(CORPUS).join(name)).unwrap();
        let original = extracted(&bytes).unwrap();
        for length in 0..bytes.len() {
            if !verifies(&bytes[..length]) {
                continue;
            }
            let found = extracted(&bytes[..length]).unwrap();
            for entry in &found {
                assert!(
                    entry.1 == b"dir" || original.contains(entry),
                    "{name} cut to {length} extracts {:?}",
                    entry.0
                );
            }
        }
    }
}

#[test]
fn every_rar4_bit_flip_is_refused_or_extracts_the_original_tree() {
    for name in RAR4 {
        let bytes = fs::read(Path::new(CORPUS).join(name)).unwrap();
        let original = extracted(&bytes).unwrap();
        for at in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[at] ^= 0x01;
            if verifies(&flipped) {
                assert_eq!(
                    extracted(&flipped).as_ref(),
                    Some(&original),
                    "{name} with byte {at} flipped"
                );
            }
        }
    }
}

#[test]
fn every_bit_flip_is_refused_or_extracts_the_original_tree() {
    let sevenzip = sevenzip::sevenzip();
    for name in STORED.iter().chain(COMPRESSED) {
        let bytes = fs::read(Path::new(CORPUS).join(name)).unwrap();
        let original = extracted(&bytes).unwrap();
        for at in 0..bytes.len() {
            for bit in [0x01u8, 0x80] {
                let mut flipped = bytes.clone();
                flipped[at] ^= bit;
                if !verifies(&flipped) {
                    continue;
                }
                assert_eq!(
                    extracted(&flipped).as_ref(),
                    Some(&original),
                    "{name} with byte {at} xor {bit:#x}"
                );
                if let (Some(binary), true) = (&sevenzip, STORED.contains(name)) {
                    assert!(
                        sevenzip_accepts(binary, &flipped),
                        "{name} with byte {at} xor {bit:#x} is refused by 7-Zip"
                    );
                }
            }
        }
    }
}

fn vint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn block(kind: u64, flags: u64, fields: &[u8], extra: &[u8], data: &[u8]) -> Vec<u8> {
    let mut body = vint(kind);
    let flags = flags | if extra.is_empty() { 0 } else { 1 } | if data.is_empty() { 0 } else { 2 };
    body.extend(vint(flags));
    if !extra.is_empty() {
        body.extend(vint(extra.len() as u64));
    }
    if !data.is_empty() {
        body.extend(vint(data.len() as u64));
    }
    body.extend_from_slice(fields);
    body.extend_from_slice(extra);
    let mut header = vint(body.len() as u64);
    header.extend(body);
    let mut out = crc32fast::hash(&header).to_le_bytes().to_vec();
    out.extend(header);
    out.extend_from_slice(data);
    out
}

struct File<'a> {
    name: &'a str,
    data: &'a [u8],
    directory: bool,
    compression: u64,
    host: u64,
    attributes: u64,
    extra: Vec<u8>,
    flags: u64,
    unpacked: Option<u64>,
}

impl<'a> File<'a> {
    fn new(name: &'a str, data: &'a [u8]) -> Self {
        File {
            name,
            data,
            directory: false,
            compression: 0,
            host: 1,
            attributes: 0o100644,
            extra: Vec::new(),
            flags: 0,
            unpacked: None,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut fields = vint(if self.directory { 1 } else { 0 } | 4);
        fields.extend(vint(self.unpacked.unwrap_or(self.data.len() as u64)));
        fields.extend(vint(self.attributes));
        fields.extend(crc32fast::hash(self.data).to_le_bytes());
        fields.extend(vint(self.compression));
        fields.extend(vint(self.host));
        fields.extend(vint(self.name.len() as u64));
        fields.extend_from_slice(self.name.as_bytes());
        block(2, self.flags, &fields, &self.extra, self.data)
    }
}

fn record(kind: u64, data: &[u8]) -> Vec<u8> {
    let mut body = vint(kind);
    body.extend_from_slice(data);
    let mut out = vint(body.len() as u64);
    out.extend(body);
    out
}

fn archive(files: &[File]) -> Vec<u8> {
    let mut out = b"Rar!\x1A\x07\x01\x00".to_vec();
    out.extend(block(1, 0, &vint(0), &[], &[]));
    for file in files {
        out.extend(file.bytes());
    }
    out.extend(block(5, 0, &vint(0), &[], &[]));
    out
}

fn symlink(name: &'static str, target: &'static str) -> File<'static> {
    let mut redirect = vec![1, 0];
    redirect.extend(vint(target.len() as u64));
    redirect.extend_from_slice(target.as_bytes());
    File {
        attributes: 0o120777,
        extra: record(5, &redirect),
        ..File::new(name, b"")
    }
}

#[test]
fn the_test_writer_makes_archives_the_reader_and_7zip_extract_alike() {
    let bytes = archive(&[
        File::new("a/b.txt", b"hello"),
        File {
            directory: true,
            ..File::new("c", b"")
        },
    ]);
    let found = extracted(&bytes).unwrap();
    assert_eq!(
        found,
        vec![
            ("a".to_owned(), b"dir".to_vec()),
            ("a/b.txt".to_owned(), b"hello".to_vec()),
            ("c".to_owned(), b"dir".to_vec()),
        ]
    );
    if let Some(binary) = sevenzip::sevenzip() {
        assert!(sevenzip_accepts(&binary, &bytes));
    }
}

#[test]
fn each_hostile_layout_is_refused_and_leaves_nothing() {
    let mut encrypted = vec![0u8, 0, 15];
    encrypted.extend([0u8; 32]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("parent", archive(&[File::new("../x", b"x")])),
        ("absolute", archive(&[File::new("/etc/x", b"x")])),
        ("inner parent", archive(&[File::new("a/../../x", b"x")])),
        ("backslash", archive(&[File::new("a\\..\\x", b"x")])),
        ("empty segment", archive(&[File::new("a//x", b"x")])),
        ("nul", archive(&[File::new("a\0x", b"x")])),
        ("escaping symlink", archive(&[symlink("l", "../../etc")])),
        ("absolute symlink", archive(&[symlink("l", "/etc")])),
        (
            "file under a symlink",
            archive(&[symlink("l", "d"), File::new("l/x", b"x")]),
        ),
        (
            "file and folder of one name",
            archive(&[File::new("a", b"x"), File::new("a/b", b"y")]),
        ),
        (
            "folder with data",
            archive(&[File {
                directory: true,
                ..File::new("d", b"x")
            }]),
        ),
        (
            "stored sizes that differ",
            archive(&[File {
                unpacked: Some(5),
                ..File::new("a", b"xy")
            }]),
        ),
        (
            "unknown method",
            archive(&[File {
                compression: 6 << 7,
                ..File::new("a", b"x")
            }]),
        ),
        (
            "dictionary past 4 GiB",
            archive(&[File {
                compression: (3 << 7) | (16 << 10),
                ..File::new("a", b"x")
            }]),
        ),
        (
            "RAR 7 compression",
            archive(&[File {
                compression: 1 | (3 << 7),
                ..File::new("a", b"x")
            }]),
        ),
        (
            "unknown host",
            archive(&[File {
                host: 7,
                ..File::new("a", b"x")
            }]),
        ),
        (
            "encrypted entry",
            archive(&[File {
                extra: record(1, &encrypted),
                ..File::new("a", b"x")
            }]),
        ),
        (
            "split entry",
            archive(&[File {
                flags: 0x10,
                ..File::new("a", b"x")
            }]),
        ),
        (
            "unknown hash",
            archive(&[File {
                extra: record(2, &[1, 0, 0]),
                ..File::new("a", b"x")
            }]),
        ),
        (
            "wrong BLAKE2sp",
            archive(&[File {
                extra: record(2, &[0; 33]),
                ..File::new("a", b"x")
            }]),
        ),
        ("no end header", {
            let mut bytes = archive(&[File::new("a", b"x")]);
            bytes.truncate(bytes.len() - 7);
            bytes
        }),
        ("second main header", {
            let mut bytes = archive(&[File::new("a", b"x")]);
            let end = bytes.split_off(bytes.len() - 7);
            bytes.extend(block(1, 0, &vint(0), &[], &[]));
            bytes.extend(end);
            bytes
        }),
    ];
    for (label, bytes) in cases {
        assert!(!verifies(&bytes), "{label} was verified");
        assert!(extracted(&bytes).is_none(), "{label} was extracted");
    }
}

fn rar4_block(kind: u8, flags: u16, fields: &[u8]) -> Vec<u8> {
    let size = (7 + fields.len()) as u16;
    let mut header = vec![kind];
    header.extend(flags.to_le_bytes());
    header.extend(size.to_le_bytes());
    header.extend_from_slice(fields);
    let mut out = (crc32fast::hash(&header) as u16).to_le_bytes().to_vec();
    out.extend(header);
    out
}

struct File4<'a> {
    name: &'a [u8],
    data: &'a [u8],
    flags: u16,
    host: u8,
    version: u8,
    method: u8,
    attributes: u32,
    unpacked: Option<(u32, u32)>,
}

impl<'a> File4<'a> {
    fn new(name: &'a str, data: &'a [u8]) -> Self {
        File4 {
            name: name.as_bytes(),
            data,
            flags: 0x8000,
            host: 3,
            version: 20,
            method: 0x30,
            attributes: 0o100644,
            unpacked: None,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let (size, crc) = self
            .unpacked
            .unwrap_or((self.data.len() as u32, crc32fast::hash(self.data)));
        let mut fields = Vec::new();
        fields.extend((self.data.len() as u32).to_le_bytes());
        fields.extend(size.to_le_bytes());
        fields.push(self.host);
        fields.extend(crc.to_le_bytes());
        fields.extend(0x5A21_0000u32.to_le_bytes());
        fields.push(self.version);
        fields.push(self.method);
        fields.extend((self.name.len() as u16).to_le_bytes());
        fields.extend(self.attributes.to_le_bytes());
        fields.extend_from_slice(self.name);
        let mut out = rar4_block(0x74, self.flags, &fields);
        out.extend_from_slice(self.data);
        out
    }
}

fn archive4(main_flags: u16, files: &[File4]) -> Vec<u8> {
    let mut out = b"Rar!\x1A\x07\x00".to_vec();
    out.extend(rar4_block(0x73, main_flags, &[0; 6]));
    for file in files {
        out.extend(file.bytes());
    }
    out.extend(rar4_block(0x7B, 0x4000, &[]));
    out
}

#[test]
fn the_rar4_test_writer_makes_archives_the_reader_and_7zip_extract_alike() {
    let bytes = archive4(
        0,
        &[
            File4::new("a\\b.txt", b"hello"),
            File4 {
                flags: 0x80E0,
                ..File4::new("c", b"")
            },
        ],
    );
    assert_eq!(
        extracted(&bytes).unwrap(),
        vec![
            ("a".to_owned(), b"dir".to_vec()),
            ("a/b.txt".to_owned(), b"hello".to_vec()),
            ("c".to_owned(), b"dir".to_vec()),
        ]
    );
    if let Some(binary) = sevenzip::sevenzip() {
        assert!(sevenzip_accepts(&binary, &bytes));
    }
}

#[test]
fn each_hostile_rar4_layout_is_refused_and_leaves_nothing() {
    let link = |target: &'static [u8]| File4 {
        attributes: 0o120777,
        ..File4::new("l", target)
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("parent", archive4(0, &[File4::new("..\\x", b"x")])),
        ("absolute", archive4(0, &[File4::new("\\x", b"x")])),
        ("drive", archive4(0, &[File4::new("c:\\x", b"x")])),
        ("escaping symlink", archive4(0, &[link(b"../../x")])),
        ("absolute symlink", archive4(0, &[link(b"/etc")])),
        (
            "code page name",
            archive4(
                0,
                &[File4 {
                    name: b"\xE9",
                    ..File4::new("", b"x")
                }],
            ),
        ),
        (
            "encrypted entry",
            archive4(
                0,
                &[File4 {
                    flags: 0x8004,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        (
            "solid entry",
            archive4(
                0,
                &[File4 {
                    flags: 0x8010,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        (
            "split entry",
            archive4(
                0,
                &[File4 {
                    flags: 0x8002,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        ("solid archive", archive4(0x0008, &[File4::new("a", b"x")])),
        ("volume", archive4(0x0001, &[File4::new("a", b"x")])),
        (
            "encrypted headers",
            archive4(0x0080, &[File4::new("a", b"x")]),
        ),
        (
            "RAR 2.0 compression",
            archive4(
                0,
                &[File4 {
                    method: 0x33,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        (
            "unknown method",
            archive4(
                0,
                &[File4 {
                    method: 0x36,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        (
            "unknown host",
            archive4(
                0,
                &[File4 {
                    host: 9,
                    ..File4::new("a", b"x")
                }],
            ),
        ),
        (
            "folder with data",
            archive4(
                0,
                &[File4 {
                    flags: 0x80E0,
                    ..File4::new("d", b"x")
                }],
            ),
        ),
        ("second main header", {
            let mut bytes = archive4(0, &[File4::new("a", b"x")]);
            let end = bytes.split_off(bytes.len() - 7);
            bytes.extend(rar4_block(0x73, 0, &[0; 6]));
            bytes.extend(end);
            bytes
        }),
        ("unknown block", {
            let mut bytes = archive4(0, &[File4::new("a", b"x")]);
            let end = bytes.split_off(bytes.len() - 7);
            bytes.extend(rar4_block(0x70, 0, &[]));
            bytes.extend(end);
            bytes
        }),
    ];
    for (label, bytes) in cases {
        assert!(!verifies(&bytes), "{label} was verified");
        assert!(extracted(&bytes).is_none(), "{label} was extracted");
    }
}

const HOSTILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rar-hostile");

#[test]
fn ppmd_models_that_restart_faster_than_their_output_are_refused() {
    let bytes = fs::read(Path::new(HOSTILE).join("ppmd-restarts-256-mib-1000-times.rar")).unwrap();
    let start = std::time::Instant::now();
    assert!(!verifies(&bytes));
    assert!(start.elapsed() < std::time::Duration::from_secs(30));
}

#[test]
fn filters_after_a_program_reset_and_empty_filters_at_the_end_extract() {
    for name in [
        "filter-after-a-program-reset.rar",
        "empty-filter-at-the-end.rar",
    ] {
        let bytes = fs::read(Path::new(HOSTILE).join(name)).unwrap();
        assert!(verifies(&bytes), "{name}");
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, value: u64, count: u32) {
        for bit in (0..count).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value >> bit & 1 == 1 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

#[cfg(unix)]
fn run_of_one_byte_with_fixed_width_tables(repeats: usize) -> (Vec<u8>, u64) {
    let mut writer = BitWriter::default();
    for _ in 0..20 {
        writer.put(5, 4);
    }
    for (count, bits) in [(306, 9), (64, 6), (16, 4), (44, 6)] {
        for _ in 0..count {
            writer.put(bits, 5);
        }
    }
    writer.put(7, 9);
    writer.put(262 + 43, 9);
    writer.put(511, 9);
    writer.put(0, 6);
    for _ in 0..repeats {
        writer.put(257, 9);
    }
    let size = writer.bytes.len();
    let last_bits = match writer.bits % 8 {
        0 => 8,
        bits => bits,
    };
    let size_bytes = size.to_le_bytes();
    let flags = 0x80 | 0x40 | (2 << 3) | (last_bits as u8 - 1);
    let check = 0x5A ^ flags ^ size_bytes[0] ^ size_bytes[1] ^ size_bytes[2];
    let mut packed = vec![flags, check, size_bytes[0], size_bytes[1], size_bytes[2]];
    packed.extend(writer.bytes);
    (packed, 1 + 4097 * (repeats as u64 + 1))
}

#[cfg(unix)]
fn peak_resident_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable rusage for getrusage to fill.
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let peak = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

#[cfg(unix)]
#[test]
fn a_dictionary_past_the_decoder_budget_is_refused_before_its_window_grows() {
    let (packed, size) = run_of_one_byte_with_fixed_width_tables(80_000);
    let power = 15;
    let mut fields = vint(0);
    fields.extend(vint(size));
    fields.extend(vint(0o100644));
    fields.extend(vint((3 << 7) | (power << 10)));
    fields.extend(vint(1));
    fields.extend(vint(1));
    fields.push(b'z');
    let mut bytes = b"Rar!\x1A\x07\x01\x00".to_vec();
    bytes.extend(block(1, 0, &vint(0), &[], &[]));
    bytes.extend(block(2, 0, &fields, &[], &packed));
    bytes.extend(block(5, 0, &vint(0), &[], &[]));
    let before = peak_resident_bytes();
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.rar");
    fs::write(&archive, &bytes).unwrap();
    let error = read_archive(&archive, None, ReadOptions::default()).unwrap_err();
    assert!(format!("{error:#}").contains("decoder budget"), "{error:#}");
    assert!(
        peak_resident_bytes() < before + (64 << 20),
        "the refused archive raised peak memory by {} bytes",
        peak_resident_bytes() - before
    );
}

const RAR4_SHORT_MAIN_SYMBOLS: u64 = 213;
const RAR4_COMPLETE_TABLES: [(usize, usize, u64); 4] =
    [(299, 213, 8), (60, 4, 5), (17, 15, 4), (28, 4, 4)];
const RAR4_FILTER_SYMBOL: u64 = 257;
const RAR4_END_SYMBOL: u64 = 256;
const RAR4_FILTER_MEMORY: usize = 0x4_0000;
const RAR4_FILTER_WORK: usize = 0x3_C000;
const RAR4_MOST_FILTERS: u32 = 8192;

#[derive(Clone, Copy)]
enum Standard {
    Delta,
    E8,
    E8E9,
    Rgb,
    Audio,
}

impl Standard {
    fn program(self) -> Vec<u8> {
        let (length, crc) = match self {
            Standard::Delta => (0x1D, 0x0E06_077D),
            Standard::E8 => (0x35, 0xAD57_6887),
            Standard::E8E9 => (0x39, 0x3CD7_E57E),
            Standard::Rgb => (0x95, 0x1C2C_5DC8),
            Standard::Audio => (0xD8, 0xBC85_E701),
        };
        program_with_fingerprint(length, crc)
    }
}

fn crc_table() -> [u32; 256] {
    std::array::from_fn(|index| {
        (0..8).fold(index as u32, |value, _| {
            if value & 1 == 1 {
                value >> 1 ^ 0xEDB8_8320
            } else {
                value >> 1
            }
        })
    })
}

fn bytes_that_set_the_crc(prefix: &[u8], target: u32) -> [u8; 4] {
    let table = crc_table();
    let mut indexes = [0u8; 4];
    let mut wanted = !target;
    for slot in (0..4).rev() {
        let index = (0..=255u8)
            .find(|&index| table[usize::from(index)] >> 24 == wanted >> 24)
            .unwrap();
        indexes[slot] = index;
        wanted = (wanted ^ table[usize::from(index)]) << 8;
    }
    let mut register = !crc32fast::hash(prefix);
    indexes.map(|index| {
        let byte = register as u8 ^ index;
        register = register >> 8 ^ table[usize::from(index)];
        byte
    })
}

fn program_with_fingerprint(length: usize, crc: u32) -> Vec<u8> {
    for free in 0..=255u8 {
        for check in 0..=255u8 {
            let mut code = vec![0x5A; length];
            code[0] = check;
            code[1] = free;
            let tail = bytes_that_set_the_crc(&code[..length - 4], crc);
            code[length - 4..].copy_from_slice(&tail);
            if code[1..].iter().fold(0, |sum, byte| sum ^ byte) == check {
                assert_eq!(crc32fast::hash(&code), crc);
                return code;
            }
        }
    }
    unreachable!("no program has the check byte and CRC-32 {crc:#x}")
}

#[derive(Clone, Default)]
struct FilterRecord {
    number: Option<u32>,
    start: u32,
    after_258: bool,
    length: Option<u32>,
    registers: Vec<(usize, u32)>,
    program: Option<Vec<u8>>,
    global: Option<u32>,
}

impl FilterRecord {
    fn new(kind: Standard, start: u32, length: u32, registers: &[(usize, u32)]) -> Self {
        FilterRecord {
            number: Some(0),
            start,
            length: Some(length),
            registers: registers.to_vec(),
            program: Some(kind.program()),
            ..FilterRecord::default()
        }
    }

    fn again(number: u32, start: u32, length: u32) -> Self {
        FilterRecord {
            number: Some(number),
            start,
            length: Some(length),
            ..FilterRecord::default()
        }
    }

    fn put(&self, writer: &mut BitWriter) {
        let number = |record: &mut BitWriter, value: u32| {
            record.put(3, 2);
            record.put(u64::from(value), 32);
        };
        let mut record = BitWriter::default();
        let mut flags = 7u64;
        if let Some(value) = self.number {
            flags |= 0x80;
            number(&mut record, value);
        }
        number(&mut record, self.start);
        if self.after_258 {
            flags |= 0x40;
        }
        if let Some(value) = self.length {
            flags |= 0x20;
            number(&mut record, value);
        }
        if !self.registers.is_empty() {
            flags |= 0x10;
            let mask = self
                .registers
                .iter()
                .fold(0, |mask, &(register, _)| mask | 1 << register);
            record.put(mask, 7);
            for &(_, value) in &self.registers {
                number(&mut record, value);
            }
        }
        if let Some(code) = &self.program {
            number(&mut record, code.len() as u32);
            for &byte in code {
                record.put(u64::from(byte), 8);
            }
        }
        if let Some(size) = self.global {
            flags |= 0x08;
            number(&mut record, size);
            for _ in 0..size {
                record.put(0, 8);
            }
        }
        put_main(writer, RAR4_FILTER_SYMBOL);
        writer.put(flags, 8);
        writer.put(record.bytes.len() as u64, 16);
        for byte in record.bytes {
            writer.put(u64::from(byte), 8);
        }
    }
}

enum Lz4<'a> {
    Literals(&'a [u8]),
    Filter(FilterRecord),
}

fn put_main(writer: &mut BitWriter, symbol: u64) {
    if symbol < RAR4_SHORT_MAIN_SYMBOLS {
        writer.put(symbol, 8);
    } else {
        writer.put(
            2 * RAR4_SHORT_MAIN_SYMBOLS + symbol - RAR4_SHORT_MAIN_SYMBOLS,
            9,
        );
    }
}

fn rar4_stream(tokens: &[Lz4]) -> Vec<u8> {
    let mut writer = BitWriter::default();
    writer.put(0, 2);
    for symbol in 0..20 {
        writer.put(if symbol < 12 { 4 } else { 5 }, 4);
    }
    for (count, short, width) in RAR4_COMPLETE_TABLES {
        for symbol in 0..count {
            writer.put(if symbol < short { width } else { width + 1 }, 4);
        }
    }
    for token in tokens {
        match token {
            Lz4::Literals(bytes) => {
                for &byte in *bytes {
                    put_main(&mut writer, u64::from(byte));
                }
            }
            Lz4::Filter(record) => record.put(&mut writer),
        }
    }
    put_main(&mut writer, RAR4_END_SYMBOL);
    writer.put(0, 2);
    writer.bytes
}

fn rar4_compressed(tokens: &[Lz4], size: usize, crc: u32) -> Vec<u8> {
    let packed = rar4_stream(tokens);
    archive4(
        0,
        &[File4 {
            version: 29,
            method: 0x33,
            unpacked: Some((size as u32, crc)),
            ..File4::new("f", &packed)
        }],
    )
}

fn rar4_error(bytes: &[u8]) -> Option<String> {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.rar");
    fs::write(&archive, bytes).unwrap();
    let error = read_archive(&archive, None, ReadOptions::default()).err()?;
    assert!(
        extracted(bytes).is_none(),
        "a refused archive was extracted"
    );
    Some(format!("{error:#}"))
}

fn pattern(length: usize, seed: u32) -> Vec<u8> {
    (0..length as u32)
        .map(|at| (at.wrapping_mul(seed) ^ at >> 3) as u8)
        .collect()
}

fn one_filter(kind: Standard, data: &[u8], registers: &[(usize, u32)]) -> Vec<u8> {
    let record = FilterRecord::new(kind, 0, data.len() as u32, registers);
    rar4_compressed(&[Lz4::Filter(record), Lz4::Literals(data)], data.len(), 0)
}

#[test]
fn a_rar4_program_that_is_not_a_standard_filter_is_refused() {
    let mut itanium_or_other = Standard::E8.program();
    itanium_or_other[2] ^= 1;
    itanium_or_other[3] ^= 1;
    let record = FilterRecord {
        program: Some(itanium_or_other),
        ..FilterRecord::new(Standard::E8, 0, 16, &[])
    };
    let bytes = rar4_compressed(&[Lz4::Filter(record), Lz4::Literals(&[0xE8; 16])], 16, 0);
    let error = rar4_error(&bytes).unwrap();
    assert!(error.contains("not a standard filter"), "{error}");
}

#[test]
fn rar4_filters_outside_their_bounds_are_refused() {
    let data = pattern(600, 11);
    let past_half = pattern(RAR4_FILTER_WORK / 2 + 1, 13);
    let long_e8 = pattern(RAR4_FILTER_WORK + 1, 17);
    let too_long = pattern(RAR4_FILTER_MEMORY + 1, 19);
    let first = |kind, length: usize, registers: &[(usize, u32)]| {
        one_filter(kind, &data[..length], registers)
    };
    let then = |records: Vec<FilterRecord>, length: usize| {
        let mut tokens: Vec<Lz4> = records.into_iter().map(Lz4::Filter).collect();
        tokens.push(Lz4::Literals(&data[..length]));
        rar4_compressed(&tokens, length, 0)
    };
    let e8 = |start, length| FilterRecord::new(Standard::E8, start, length, &[]);
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "delta without channels",
            first(Standard::Delta, 64, &[(0, 0)]),
            "delta filter",
        ),
        (
            "delta with 129 channels",
            first(Standard::Delta, 300, &[(0, 129)]),
            "delta filter",
        ),
        (
            "delta longer than half the work memory",
            one_filter(Standard::Delta, &past_half, &[(0, 1)]),
            "delta filter",
        ),
        (
            "audio longer than half the work memory",
            one_filter(Standard::Audio, &past_half, &[(0, 1)]),
            "audio filter",
        ),
        (
            "RGB longer than half the work memory",
            one_filter(Standard::Rgb, &past_half, &[(0, 3), (1, 0)]),
            "RGB filter",
        ),
        (
            "audio without channels",
            first(Standard::Audio, 64, &[(0, 0)]),
            "audio filter",
        ),
        (
            "audio with 129 channels",
            first(Standard::Audio, 300, &[(0, 129)]),
            "audio filter",
        ),
        ("E8 of 4 bytes", first(Standard::E8, 4, &[]), "E8 filter"),
        ("E8E9 of 1 byte", first(Standard::E8E9, 1, &[]), "E8 filter"),
        (
            "E8 longer than the work memory",
            one_filter(Standard::E8, &long_e8, &[]),
            "E8 filter",
        ),
        (
            "E8E9 longer than the filter memory",
            one_filter(Standard::E8E9, &too_long, &[]),
            "filter memory",
        ),
        (
            "RGB of 2 bytes",
            first(Standard::Rgb, 2, &[(0, 1), (1, 0)]),
            "RGB filter",
        ),
        (
            "RGB stride past its block",
            first(Standard::Rgb, 30, &[(0, 31), (1, 0)]),
            "RGB filter",
        ),
        (
            "RGB byte offset 3",
            first(Standard::Rgb, 30, &[(0, 6), (1, 3)]),
            "RGB filter",
        ),
        (
            "RGB byte offset that wraps",
            first(Standard::Rgb, 30, &[(0, 6), (1, u32::MAX)]),
            "RGB filter",
        ),
        (
            "filter past the declared size",
            then(vec![e8(10, 64)], 73),
            "outside its file",
        ),
        (
            "filter 258 bytes on, past the declared size",
            then(
                vec![FilterRecord {
                    after_258: true,
                    ..e8(0, 64)
                }],
                300,
            ),
            "outside its file",
        ),
        (
            "filter start at the top of 32 bits",
            then(vec![e8(u32::MAX, 64)], 80),
            "outside its file",
        ),
        (
            "length register that differs from the block",
            first(Standard::E8, 64, &[(4, 65)]),
            "outside its file",
        ),
        (
            "filter that overlaps the one before",
            then(vec![e8(0, 64), FilterRecord::again(1, 63, 64)], 200),
            "outside its file",
        ),
        (
            "program number that was never defined",
            then(vec![e8(0, 64), FilterRecord::again(3, 100, 64)], 200),
            "does not exist",
        ),
        (
            "global data past the user area",
            then(
                vec![FilterRecord {
                    global: Some(0x2000 - 0x40 + 1),
                    ..e8(0, 64)
                }],
                64,
            ),
            "global data",
        ),
        (
            "empty program",
            then(
                vec![FilterRecord {
                    program: Some(Vec::new()),
                    ..e8(0, 64)
                }],
                64,
            ),
            "empty or too long",
        ),
    ];
    for (label, bytes, reason) in cases {
        let error = rar4_error(&bytes).unwrap_or_else(|| panic!("{label} was verified"));
        assert!(error.contains(reason), "{label}: {error}");
    }
}

#[test]
fn more_rar4_filters_than_the_queue_holds_are_refused() {
    let data = pattern((RAR4_MOST_FILTERS as usize + 1) * 5, 23);
    let mut tokens = vec![Lz4::Filter(FilterRecord::new(Standard::E8, 0, 5, &[]))];
    for index in 1..=RAR4_MOST_FILTERS {
        tokens.push(Lz4::Filter(FilterRecord::again(1, index * 5, 5)));
    }
    tokens.push(Lz4::Literals(&data));
    let error = rar4_error(&rar4_compressed(&tokens, data.len(), 0)).unwrap();
    assert!(error.contains("outside its file"), "{error}");
}

fn calls(length: usize, opcodes: &[u8]) -> Vec<u8> {
    let addresses = [
        0x10u32,
        0x00FF_FFFF,
        0x0100_0000,
        0xFFFF_FFF0,
        0x8000_0000,
        0x7FFF_FFFF,
        5,
    ];
    let mut data = pattern(length, 29);
    for (index, at) in (3..length).step_by(9).enumerate() {
        data[at] = opcodes[index % opcodes.len()];
        let address = addresses[index % addresses.len()].to_le_bytes();
        let end = (at + 5).min(length);
        data[at + 1..end].copy_from_slice(&address[..end - at - 1]);
    }
    data
}

fn accepted_rar4_filters(crcs: &[u32; 13]) -> Vec<(&'static str, Vec<u8>)> {
    let lead = pattern(100, 5);
    let body = pattern(1000, 41);
    let slow: Vec<u8> = (0..20_000u32).map(|at| (at / 50) as u8).collect();
    let steady = vec![3u8; 20_000];
    let alternating: Vec<u8> = (0..20_000u32)
        .map(|at| if at % 2 == 0 { 0x40 } else { 0xC0 })
        .collect();
    let noise = pattern(20_000, 97);
    let e8 = calls(1000, &[0xE8]);
    let e8e9 = calls(1000, &[0xE8, 0xE9, 0x90]);
    let built: Vec<(&str, Vec<Lz4>, usize)> = vec![
        (
            "delta of 3 channels after 100 bytes",
            vec![
                Lz4::Literals(&lead),
                Lz4::Filter(FilterRecord::new(Standard::Delta, 0, 300, &[(0, 3)])),
                Lz4::Literals(&body[..400]),
            ],
            500,
        ),
        (
            "E8 after 100 bytes",
            vec![
                Lz4::Literals(&lead),
                Lz4::Filter(FilterRecord::new(Standard::E8, 0, 1000, &[])),
                Lz4::Literals(&e8),
            ],
            1100,
        ),
        (
            "E8E9 258 bytes on",
            vec![
                Lz4::Filter(FilterRecord {
                    after_258: true,
                    ..FilterRecord::new(Standard::E8E9, 0, 700, &[])
                }),
                Lz4::Literals(&lead),
                Lz4::Literals(&e8e9),
            ],
            1100,
        ),
        (
            "RGB of 10 pixels a row",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Rgb, 0, 300, &[(0, 30), (1, 1)])),
                Lz4::Literals(&body[..300]),
            ],
            300,
        ),
        (
            "RGB with a stride of 3 and byte offset 2",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Rgb, 0, 301, &[(0, 3), (1, 2)])),
                Lz4::Literals(&body[..301]),
            ],
            301,
        ),
        (
            "audio of 2 channels",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 999, &[(0, 2)])),
                Lz4::Literals(&body[..999]),
            ],
            999,
        ),
        (
            "delta, then a second program, then a reset to audio",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Delta, 0, 100, &[(0, 1)])),
                Lz4::Filter(FilterRecord {
                    number: Some(2),
                    ..FilterRecord::new(Standard::E8, 200, 300, &[])
                }),
                Lz4::Literals(&e8[..600]),
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 300, &[(0, 1)])),
                Lz4::Literals(&body[..400]),
            ],
            1000,
        ),
        (
            "an empty filter",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Delta, 0, 0, &[(0, 1)])),
                Lz4::Literals(&body[..10]),
            ],
            10,
        ),
        (
            "delta of 2 channels 258 bytes on",
            vec![
                Lz4::Filter(FilterRecord {
                    after_258: true,
                    ..FilterRecord::new(Standard::Delta, 0, 301, &[(0, 2)])
                }),
                Lz4::Literals(&body[..600]),
            ],
            600,
        ),
        (
            "audio of 1 channel over a slow ramp",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 20_000, &[(0, 1)])),
                Lz4::Literals(&slow),
            ],
            20_000,
        ),
        (
            "audio of 1 channel over a steady value",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 20_000, &[(0, 1)])),
                Lz4::Literals(&steady),
            ],
            20_000,
        ),
        (
            "audio of 1 channel over alternating values",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 20_000, &[(0, 1)])),
                Lz4::Literals(&alternating),
            ],
            20_000,
        ),
        (
            "audio of 1 channel over noise",
            vec![
                Lz4::Filter(FilterRecord::new(Standard::Audio, 0, 20_000, &[(0, 1)])),
                Lz4::Literals(&noise),
            ],
            20_000,
        ),
    ];
    built
        .into_iter()
        .enumerate()
        .map(|(index, (label, tokens, size))| (label, rar4_compressed(&tokens, size, crcs[index])))
        .collect()
}

// EXTERNAL(libarchive 3.7.4): the CRC-32 of what `bsdtar -xOf` printed for
// each archive of `accepted_rar4_filters` on macOS, in order.
const BSDTAR_FILTER_OUTPUT_CRCS: [u32; 13] = [
    0x2056_D2A5,
    0xFD9C_C6E8,
    0xB640_C806,
    0x2DB6_1985,
    0x45D7_2328,
    0xCC65_F1CE,
    0x7F8D_6793,
    0x6275_7B30,
    0x6BEA_D2CB,
    0xD2F9_40CE,
    0x4A7B_DF64,
    0xDAE8_C973,
    0xCDE3_7020,
];

#[test]
fn standard_rar4_filters_decode_as_libarchive_decodes_them() {
    for (label, bytes) in accepted_rar4_filters(&BSDTAR_FILTER_OUTPUT_CRCS) {
        let tree = extracted(&bytes).unwrap_or_else(|| panic!("{label} was refused"));
        assert_eq!(tree.len(), 1, "{label}");
    }
}

const PPMD_256_MIB_RESTART: [u8; 8] = [0xA1, 0xFF, 0x01, 0xFE, 0x01, 0xFE, 0x00, 0x00];

fn files_restarting_ppmd(files: &[(usize, &[u8])]) -> Vec<u8> {
    let streams: Vec<(String, Vec<u8>, &[u8])> = files
        .iter()
        .enumerate()
        .map(|(index, &(restarts, output))| {
            let mut packed = PPMD_256_MIB_RESTART.repeat(restarts);
            packed.extend(rar4_stream(&[Lz4::Literals(output)]));
            (format!("f{index}"), packed, output)
        })
        .collect();
    let entries: Vec<File4> = streams
        .iter()
        .map(|(name, packed, output)| File4 {
            version: 29,
            method: 0x33,
            unpacked: Some((output.len() as u32, crc32fast::hash(output))),
            ..File4::new(name, packed)
        })
        .collect();
    archive4(0, &entries)
}

#[test]
fn ppmd_restarts_are_charged_to_the_archive_not_to_each_file() {
    let byte: &[u8] = b"A";
    assert!(verifies(&files_restarting_ppmd(&[(4, byte)])));
    assert!(verifies(&files_restarting_ppmd(&[(2, byte), (2, byte)])));
    let error = rar4_error(&files_restarting_ppmd(&[(4, byte), (4, byte)])).unwrap();
    assert!(error.contains("restart more often"), "{error}");
    let four_mib = pattern(4 << 20, 31);
    assert!(verifies(&files_restarting_ppmd(&[
        (4, &four_mib),
        (1, byte)
    ])));
    let error = rar4_error(&files_restarting_ppmd(&[(4, &four_mib[1..]), (1, byte)])).unwrap();
    assert!(error.contains("restart more often"), "{error}");
}
