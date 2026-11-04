//! When an extraction returns, after success or failure, the library holds
//! no file handle and no thread of its own, so the caller can rename or delete
//! the destination at once. This file holds one test, so nothing else in its
//! process opens files or threads while it counts them. The shared Rayon pool
//! keeps idle threads between calls, so the counts start after a warm-up.
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
};
use zipper::{Engine, ExtractOptions, extract_archive};

fn data(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(path)
}

#[cfg(unix)]
fn open_handles() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

#[cfg(not(unix))]
fn open_handles() -> usize {
    0
}

#[cfg(target_os = "linux")]
fn threads() -> usize {
    fs::read_dir("/proc/self/task").unwrap().count()
}

#[cfg(target_os = "macos")]
fn threads() -> usize {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, a plain C struct.
    let written = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    assert_eq!(written, size);
    info.pti_threadnum as usize
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn threads() -> usize {
    0
}

fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|folder| folder.join(tool))
            .find(|path| path.is_file())
    })
}

fn segmented_7z(work: &Path) -> Option<PathBuf> {
    let seven_zip = which("7zz")?;
    let source = work.join("big.bin");
    let words: Vec<String> = (0..4096u32)
        .map(|word| format!("{:x} ", word.wrapping_mul(2_654_435_761)))
        .collect();
    let mut state = 1u32;
    let mut data = Vec::with_capacity(64 << 20);
    while data.len() < 64 << 20 {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        data.extend_from_slice(words[(state >> 20) as usize].as_bytes());
    }
    fs::write(&source, data).unwrap();
    let archive = work.join("segments.7z");
    let status = std::process::Command::new(seven_zip)
        .current_dir(work)
        .args(["a", "-bd", "-mx1", "-ms=on", "-mmt=4", "-m0=LZMA2:d=1m"])
        .arg(&archive)
        .arg("big.bin")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    fs::remove_file(source).unwrap();
    Some(archive)
}

fn damaged(source: &Path, work: &Path) -> PathBuf {
    let mut bytes = fs::read(source).unwrap();
    let early = if bytes.starts_with(b"7z\xBC\xAF") && bytes.len() > 4096 {
        32 + 64
    } else {
        bytes.len() / 2
    };
    bytes[early] ^= 0x55;
    let path = work.join(format!(
        "damaged-{}",
        source.file_name().unwrap().to_str().unwrap()
    ));
    fs::write(&path, bytes).unwrap();
    path
}

fn extract(archive: &Path, engine: Engine, threads: Option<usize>) -> (tempfile::TempDir, bool) {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("out");
    let result = extract_archive(
        archive,
        &destination,
        ExtractOptions {
            engine,
            threads,
            ..Default::default()
        },
    );
    (root, result.is_ok())
}

#[test]
fn nothing_stays_open_after_any_extraction_returns() {
    let work = tempfile::tempdir().unwrap();
    let mut cases = vec![
        ("zip", data("zip-corpus/7zip-deflate.zip"), Engine::Native),
        (
            "zip fast",
            data("zip-corpus/7zip-deflate.zip"),
            Engine::NativeFast,
        ),
        ("tar", data("parity/archives/tar.tar"), Engine::Native),
        ("tar.gz", data("parity/archives/tar.tar.gz"), Engine::Native),
        ("7z", data("parity/archives/7z.7z"), Engine::Native),
        ("gz", data("parity/archives/hello.txt.gz"), Engine::Native),
    ];
    if let Some(segments) = segmented_7z(work.path()) {
        cases.push(("7z segments", segments, Engine::Native));
    }
    for (_, archive, _) in &mut cases {
        let copy = work.path().join(archive.file_name().unwrap());
        if !copy.exists() {
            fs::copy(&*archive, &copy).unwrap();
        }
        #[cfg(windows)]
        {
            let mut stream = copy.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        }
        *archive = copy;
    }
    for (_, archive, engine) in &cases {
        extract(archive, *engine, Some(8));
    }
    let (handles, threads_before) = (open_handles(), threads());
    let mut checked = 0;
    for (label, archive, engine) in &cases {
        let damaged = damaged(archive, work.path());
        for (archive, succeeds) in [(archive.clone(), true), (damaged, false)] {
            let (root, ok) = extract(&archive, *engine, Some(8));
            assert!(ok || !succeeds, "{label} failed");
            assert_eq!(open_handles(), handles, "{label}: a handle stayed open");
            assert_eq!(
                threads(),
                threads_before,
                "{label}: a thread outlived the call"
            );
            let destination = root.path().join("out");
            if ok {
                let moved = root.path().join("moved");
                fs::rename(&destination, &moved).unwrap_or_else(|error| panic!("{label}: {error}"));
                common::make_tree_deletable(&moved);
                fs::remove_dir_all(&moved).unwrap_or_else(|error| panic!("{label}: {error}"));
            }
            checked += 1;
        }
    }
    assert!(checked >= 12, "{checked}");
}
