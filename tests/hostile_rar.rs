//! Damaged RAR archives are refused, and leave nothing behind. Every cut of
//! the corpus archives is refused. Every one-bit flip is refused or, where it
//! lands on bytes no check covers, extracts exactly the original tree, and a
//! stored archive is never extracted where 7-Zip refuses it. Hand-built
//! headers cover the refusals the corpus does not reach.
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
