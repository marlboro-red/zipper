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
                .into_owned();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let bytes = if metadata.file_type().is_symlink() {
                fs::read_link(entry.path())
                    .unwrap()
                    .to_string_lossy()
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
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut fields = Vec::new();
        fields.extend((self.data.len() as u32).to_le_bytes());
        fields.extend((self.data.len() as u32).to_le_bytes());
        fields.push(self.host);
        fields.extend(crc32fast::hash(self.data).to_le_bytes());
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

#[cfg(unix)]
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

#[cfg(unix)]
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
