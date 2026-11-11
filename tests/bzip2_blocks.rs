//! BZip2 blocks decoded on several threads give the bytes one thread gives, on every BZip2
//! fixture in the corpora and on generated tar, 7z and ZIP archives of many blocks.
mod common;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

const THREADS: [Option<usize>; 4] = [Some(1), Some(2), Some(4), None];

fn extracted(
    archive: &Path,
    threads: Option<usize>,
    password: Option<&str>,
) -> BTreeMap<String, Vec<u8>> {
    let scratch = tempfile::tempdir().unwrap();
    let destination = scratch.path().join("out");
    let mut command = Command::new(env!("CARGO_BIN_EXE_zipper"));
    command
        .arg("extract")
        .arg("-o")
        .arg(&destination)
        .arg(archive);
    if let Some(threads) = threads {
        command.args(["--threads", &threads.to_string()]);
    }
    if let Some(password) = password {
        command.args(["--password", password]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{} at {threads:?} threads: {}",
        archive.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(&destination) {
        let entry = entry.unwrap();
        let name = entry.path().strip_prefix(&destination).unwrap();
        let name = name.to_string_lossy().into_owned();
        if entry.path_is_symlink() {
            let target = std::fs::read_link(entry.path()).unwrap();
            files.insert(name, target.to_string_lossy().into_owned().into_bytes());
        } else if entry.file_type().is_file() {
            files.insert(name, std::fs::read(entry.path()).unwrap());
        }
    }
    common::make_tree_deletable(scratch.path());
    files
}

fn same_at_every_thread_count(archive: &Path, password: Option<&str>) -> BTreeMap<String, Vec<u8>> {
    let one_thread = extracted(archive, Some(1), password);
    assert!(!one_thread.is_empty(), "{}", archive.display());
    for threads in THREADS {
        assert!(
            extracted(archive, threads, password) == one_thread,
            "{} differs at {threads:?} threads",
            archive.display()
        );
    }
    one_thread
}

#[test]
fn every_bzip2_fixture_extracts_the_same_at_one_two_four_and_default_threads() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    for (fixture, password) in [
        ("7z-corpus/7zip-bzip2-short.7z", None),
        ("7z-corpus/7zip-bzip2-long.7z", None),
        ("7z-corpus/7zip-bzip2-links.7z", None),
        ("encrypted-7z/bzip2.7z", Some("zipper")),
        ("encrypted-zip/zipcrypto-bzip2.zip", Some("zipper")),
        ("encrypted-zip/aes256-bzip2.zip", Some("zipper")),
        ("parity/archives/tar.tar.bz2", None),
        ("parity/archives/hello.txt.bz2", None),
        ("parity/archives/7z-bzip2.7z", None),
        ("parity/archives/rpm-bzip2.rpm", None),
        ("parity/archives/zip-bzip2.zip", None),
        ("xar-corpus/bzip2.xar", None),
    ] {
        same_at_every_thread_count(&data.join(fixture), password);
    }
}

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn source_files() -> BTreeMap<String, Vec<u8>> {
    let mut noise = Noise(0x5EED);
    let words: [&[u8]; 6] = [
        b"block ",
        b"magic ",
        b"stream ",
        b"\n",
        b"aaaaaaaa",
        b"\0\0\0\0\0",
    ];
    let mut text = Vec::new();
    while text.len() < 700_000 {
        text.extend_from_slice(words[(noise.next() % 6) as usize]);
    }
    let random: Vec<u8> = (0..450_000).map(|_| noise.next() as u8).collect();
    BTreeMap::from([
        ("text.txt".to_owned(), text),
        ("folder/random.bin".to_owned(), random),
        ("folder/zeros.bin".to_owned(), vec![0; 300_000]),
        ("empty".to_owned(), Vec::new()),
    ])
}

fn ustar(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, bytes) in files {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", bytes.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..265].copy_from_slice(b"ustar\x0000");
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        tar.extend_from_slice(&header);
        tar.extend_from_slice(bytes);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    tar
}

fn bzip2(bytes: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn a_tar_of_many_blocks_and_streams_extracts_its_source_at_every_thread_count() {
    let files = source_files();
    let tar = ustar(&files);
    let scratch = tempfile::tempdir().unwrap();
    let cut = tar.len() / 3;
    for (name, packed) in [
        ("one-stream.tar.bz2", bzip2(&tar, 1)),
        ("largest-blocks.tar.bz2", bzip2(&tar, 9)),
        (
            "two-streams.tar.bz2",
            [bzip2(&tar[..cut], 1), bzip2(&tar[cut..], 2)].concat(),
        ),
    ] {
        let archive = scratch.path().join(name);
        std::fs::write(&archive, packed).unwrap();
        assert!(
            same_at_every_thread_count(&archive, None) == files,
            "{name}"
        );
    }
}

#[test]
fn a_7z_and_a_zip_of_many_blocks_extract_their_source_at_every_thread_count() {
    let Some(sevenzip) = sevenzip::sevenzip() else {
        return;
    };
    let files = source_files();
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("source");
    for (name, bytes) in &files {
        let path = source.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    for (name, switches) in [
        ("solid.7z", &["-t7z", "-m0=BZip2", "-mx=1", "-ms=on"][..]),
        (
            "swapped.7z",
            &["-t7z", "-m0=Swap2", "-m1=BZip2", "-mx=1"][..],
        ),
        ("largest-blocks.7z", &["-t7z", "-m0=BZip2", "-mx=9"][..]),
        ("entries.zip", &["-tzip", "-mm=BZip2", "-mx=1"][..]),
        (
            "one-entry.zip",
            &["-tzip", "-mm=BZip2", "-mx=1", "-x!folder", "-x!empty"][..],
        ),
    ] {
        let archive = scratch.path().join(name);
        let made = Command::new(&sevenzip)
            .current_dir(&source)
            .arg("a")
            .args(switches)
            .arg(&archive)
            .arg(".")
            .output()
            .unwrap();
        assert!(made.status.success(), "{name}: {made:?}");
        let extracted = same_at_every_thread_count(&archive, None);
        let expected: BTreeMap<_, _> = files
            .iter()
            .filter(|(file, _)| name != "one-entry.zip" || file.as_str() == "text.txt")
            .map(|(file, bytes)| (file.clone(), bytes.clone()))
            .collect();
        assert!(extracted == expected, "{name}");
    }
}
