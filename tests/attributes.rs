//! Windows file attributes. Source files get read-only, hidden and system
//! attributes, 7-Zip archives them as 7z, ZIP and tar, and Zipper must leave
//! each extracted file with the same three attributes 7-Zip's own extraction
//! gives it. An overwrite must also replace a read-only destination file.
#![cfg(windows)]
use std::{
    collections::BTreeMap,
    fs,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{Existing, ExtractOptions, extract_archive};

const READ_ONLY: u32 = 0x1;
const HIDDEN: u32 = 0x2;
const SYSTEM: u32 = 0x4;
const COMPARED: u32 = READ_ONLY | HIDDEN | SYSTEM;

fn sevenzip() -> PathBuf {
    std::env::var_os("ZIPPER_7Z")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("7z"))
}

fn run(command: &mut Command) {
    let output = command.stdin(Stdio::null()).output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
}

fn attributes(root: &Path) -> BTreeMap<String, u32> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string();
            (name, entry.metadata().unwrap().file_attributes() & COMPARED)
        })
        .collect()
}

#[test]
fn extracted_attributes_match_7zip() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    for (name, flags) in [
        ("plain.txt", &[][..]),
        ("read-only.txt", &["+r"][..]),
        ("hidden.txt", &["+h"][..]),
        ("system.txt", &["+s"][..]),
        ("all.txt", &["+r", "+h", "+s"][..]),
    ] {
        let path = source.join(name);
        fs::write(&path, name).unwrap();
        if !flags.is_empty() {
            run(Command::new("attrib").args(flags).arg(&path));
        }
    }
    for kind in ["7z", "zip", "tar"] {
        let archive = root.path().join(format!("a.{kind}"));
        run(Command::new(sevenzip())
            .args(["a", &format!("-t{kind}")])
            .arg(&archive)
            .arg(source.join("*")));
        let by_7zip = root.path().join(format!("7zip-{kind}"));
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(&by_7zip);
        run(Command::new(sevenzip())
            .args(["x", "-y"])
            .arg(flag)
            .arg(&archive));
        let by_zipper = root.path().join(format!("zipper-{kind}"));
        extract_archive(&archive, &by_zipper, ExtractOptions::default()).unwrap();
        let expected = attributes(&by_7zip);
        assert_eq!(expected.len(), 5, "{kind}");
        assert_eq!(attributes(&by_zipper), expected, "{kind}");
        extract_archive(
            &archive,
            &by_zipper,
            ExtractOptions {
                existing: Existing::Overwrite,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(attributes(&by_zipper), expected, "{kind} after overwrite");
    }
}
