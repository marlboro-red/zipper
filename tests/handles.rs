//! When an extraction returns, after success or failure, the library holds
//! no file handle and no thread of its own, so the caller can rename or delete
//! the destination at once. This file holds one test, so nothing else in its
//! process opens files or threads while it counts them. The shared Rayon pool
//! keeps idle threads between calls, so the counts start after a warm-up.
mod common;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use std::{
    fs,
    path::{Path, PathBuf},
};
use zipper::{ExtractOptions, extract_archive};

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

// EXTERNAL(Linux procfs): a joined thread can stay listed in /proc/self/task for a moment, in state Z or X.
#[cfg(target_os = "linux")]
fn threads() -> usize {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("stat")).ok())
        .filter(|stat| {
            let state = stat.rsplit_once(") ").map(|(_, rest)| rest.as_bytes()[0]);
            !matches!(state, Some(b'Z' | b'X'))
        })
        .count()
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

// EXTERNAL(Linux): a thread pthread_join has returned for can still run its last exit steps for a moment.
fn settled_threads(baseline: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let now = threads();
        if now <= baseline || std::time::Instant::now() > deadline {
            return now;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn segmented_7z(work: &Path) -> Option<PathBuf> {
    let seven_zip = sevenzip::sevenzip()?;
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

fn volumes(source: &Path, work: &Path) -> PathBuf {
    let bytes = fs::read(source).unwrap();
    let name = source.file_name().unwrap().to_str().unwrap();
    for (index, chunk) in bytes.chunks(bytes.len() / 3).enumerate() {
        fs::write(work.join(format!("{name}.{:03}", index + 1)), chunk).unwrap();
    }
    work.join(format!("{name}.001"))
}

fn debian_package(work: &Path) -> PathBuf {
    let mut bytes = b"!<arch>\n".to_vec();
    for (name, data) in [
        ("debian-binary", b"2.0\n".to_vec()),
        (
            "data.tar.gz",
            fs::read(data("parity/archives/tar.tar.gz")).unwrap(),
        ),
    ] {
        bytes.extend_from_slice(
            format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                100644,
                data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&data);
        if data.len() % 2 == 1 {
            bytes.push(b'\n');
        }
    }
    let path = work.join("package.deb");
    fs::write(&path, bytes).unwrap();
    path
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

fn extract(archive: &Path, threads: Option<usize>) -> (tempfile::TempDir, bool) {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("out");
    let result = extract_archive(
        archive,
        &destination,
        ExtractOptions {
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
        ("zip", data("zip-corpus/7zip-deflate.zip")),
        ("tar", data("parity/archives/tar.tar")),
        ("tar.gz", data("parity/archives/tar.tar.gz")),
        ("7z", data("parity/archives/7z.7z")),
        ("gz", data("parity/archives/hello.txt.gz")),
        ("cpio", data("parity/archives/cpio-newc.cpio")),
        ("ar", data("parity/archives/ar.a")),
        ("deb", debian_package(work.path())),
    ];
    if let Some(segments) = segmented_7z(work.path()) {
        cases.push(("7z segment volumes", volumes(&segments, work.path())));
        cases.push(("7z segments", segments));
    }
    for (label, name) in [
        ("zip volumes", "zip-corpus/7zip-deflate.zip"),
        ("tar.gz volumes", "parity/archives/tar.tar.gz"),
        ("7z volumes", "parity/archives/7z.7z"),
    ] {
        cases.push((label, volumes(&data(name), work.path())));
    }
    for (_, archive) in &mut cases {
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
    for (_, archive) in &cases {
        extract(archive, Some(8));
        extract(archive, Some(1));
    }
    let (handles, threads_before) = (open_handles(), threads());
    let mut checked = 0;
    for (label, archive) in &cases {
        let damaged = damaged(archive, work.path());
        for (archive, succeeds, workers) in [
            (archive.clone(), true, 8),
            (archive.clone(), true, 1),
            (damaged.clone(), false, 8),
            (damaged, false, 1),
        ] {
            let label = format!("{label} at {workers} threads");
            let (root, ok) = extract(&archive, Some(workers));
            assert!(ok || !succeeds, "{label} failed");
            assert_eq!(open_handles(), handles, "{label}: a handle stayed open");
            let threads_after = settled_threads(threads_before);
            assert!(
                threads_after <= threads_before,
                "{label}: a thread outlived the call, {threads_after} against {threads_before}"
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
    assert!(checked >= 36, "{checked}");
}
