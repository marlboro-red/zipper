//! NTFS alternate data streams on Windows. 7-Zip stores them with `-sns` as
//! `file:stream` entries. With `alternate_streams`, Zipper writes each one onto
//! its file and keeps the file's time and read-only attribute. 7-Zip stores no
//! stream on a folder.
//! Without it, no stream is written.
#![cfg(windows)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{ExtractOptions, extract_archive};

fn stream(path: &Path, name: &str) -> PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":");
    stream.push(name);
    stream.into()
}

fn run(command: &mut Command) {
    let output = command.stdin(Stdio::null()).output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
}

#[test]
fn stored_streams_are_written_only_when_asked() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join("folder")).unwrap();
    fs::write(source.join("plain.txt"), "plain").unwrap();
    fs::write(stream(&source.join("plain.txt"), "extra"), "plain stream").unwrap();
    fs::write(source.join("read-only.txt"), "read-only").unwrap();
    fs::write(
        stream(&source.join("read-only.txt"), "extra"),
        "read-only stream",
    )
    .unwrap();
    run(Command::new("attrib")
        .arg("+r")
        .arg(source.join("read-only.txt")));
    let archive = root.path().join("streams.7z");
    let sevenzip = std::env::var_os("ZIPPER_7Z").unwrap_or_else(|| "7z".into());
    run(Command::new(sevenzip)
        .args(["a", "-t7z", "-sns"])
        .arg(&archive)
        .arg(source.join("*")));

    let plain = root.path().join("plain");
    extract_archive(&archive, &plain, ExtractOptions::default()).unwrap();
    for name in ["plain.txt", "read-only.txt"] {
        assert!(
            fs::read(stream(&plain.join(name), "extra")).is_err(),
            "{name}"
        );
    }

    let with_streams = root.path().join("with-streams");
    extract_archive(
        &archive,
        &with_streams,
        ExtractOptions {
            alternate_streams: true,
            ..Default::default()
        },
    )
    .unwrap();
    for (name, data) in [
        ("plain.txt", "plain stream"),
        ("read-only.txt", "read-only stream"),
    ] {
        assert_eq!(
            fs::read_to_string(stream(&with_streams.join(name), "extra")).unwrap(),
            data,
            "{name}"
        );
    }
    for name in ["plain.txt", "read-only.txt"] {
        let (written, reference) = (
            fs::metadata(with_streams.join(name)).unwrap(),
            fs::metadata(plain.join(name)).unwrap(),
        );
        assert_eq!(
            written.modified().unwrap(),
            reference.modified().unwrap(),
            "{name}"
        );
        assert_eq!(
            written.permissions().readonly(),
            reference.permissions().readonly(),
            "{name}"
        );
    }
    assert!(
        fs::metadata(with_streams.join("read-only.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );
}
