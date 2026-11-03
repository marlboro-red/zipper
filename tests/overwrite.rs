//! Extraction into an existing destination with `Existing::Overwrite`.
#[cfg(unix)]
mod common;

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zip::write::SimpleFileOptions;
use zipper::{Existing, native_tar, native_zip};

enum Member<'a> {
    File(&'a str, &'a [u8]),
    Directory(&'a str, u32),
    #[cfg_attr(not(unix), allow(dead_code))]
    Symlink(&'a str, &'a str),
}

fn write_zip(path: &Path, members: &[Member]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for member in members {
        match *member {
            Member::File(name, data) => {
                writer
                    .start_file(
                        name,
                        SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Stored)
                            .unix_permissions(0o644),
                    )
                    .unwrap();
                writer.write_all(data).unwrap();
            }
            Member::Directory(name, mode) => writer
                .add_directory(name, SimpleFileOptions::default().unix_permissions(mode))
                .unwrap(),
            Member::Symlink(name, target) => writer
                .add_symlink(name, target, SimpleFileOptions::default())
                .unwrap(),
        }
    }
    writer.finish().unwrap();
}

fn ustar(name: &str, kind: u8, size: usize, mode: u32) -> Vec<u8> {
    let mut header = vec![0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    header[136..148].copy_from_slice(b"13777777777\0");
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    header
}

fn write_tar(path: &Path, members: &[Member]) {
    let mut bytes = Vec::new();
    for member in members {
        match *member {
            Member::File(name, data) => {
                bytes.extend(ustar(name, b'0', data.len(), 0o644));
                bytes.extend_from_slice(data);
                bytes.resize(bytes.len().div_ceil(512) * 512, 0);
            }
            Member::Directory(name, mode) => bytes.extend(ustar(name, b'5', 0, mode)),
            Member::Symlink(..) => unreachable!("tar fixtures use no links"),
        }
    }
    bytes.extend_from_slice(&[0; 1024]);
    fs::write(path, bytes).unwrap();
}

type Extract = fn(&Path, &Path, Existing) -> anyhow::Result<Vec<zipper::Entry>>;

fn zip_reader(
    archive: &Path,
    destination: &Path,
    existing: Existing,
) -> anyhow::Result<Vec<zipper::Entry>> {
    native_zip::read_archive_with_options(
        archive,
        Some(destination),
        native_zip::ReadOptions {
            existing,
            ..Default::default()
        },
    )
}

fn tar_reader(
    archive: &Path,
    destination: &Path,
    existing: Existing,
) -> anyhow::Result<Vec<zipper::Entry>> {
    native_tar::read_archive(
        archive,
        Some(destination),
        native_tar::ReadOptions {
            existing,
            ..Default::default()
        },
    )
}

struct Format {
    name: &'static str,
    write: fn(&Path, &[Member]),
    extract: Extract,
}

const FORMATS: [Format; 2] = [
    Format {
        name: "zip",
        write: write_zip,
        extract: zip_reader,
    },
    Format {
        name: "tar",
        write: write_tar,
        extract: tar_reader,
    },
];

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let path = entry.path().strip_prefix(root).unwrap().to_owned();
            let content = entry
                .file_type()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            (path, content)
        })
        .collect()
}

fn existing_destination(root: &Path) -> PathBuf {
    let destination = root.join("out");
    fs::create_dir_all(destination.join("docs/old")).unwrap();
    fs::write(destination.join("docs/readme"), b"old readme").unwrap();
    fs::write(destination.join("docs/old/keep"), b"unrelated").unwrap();
    fs::write(destination.join("top"), b"unrelated top").unwrap();
    destination
}

#[test]
fn overwrite_replaces_files_and_keeps_the_rest() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(
            &archive,
            &[
                Member::Directory("docs/", 0o755),
                Member::File("docs/readme", b"new readme"),
                Member::File("docs/new/file", b"new file"),
                Member::File("added", b"added"),
            ],
        );
        let destination = existing_destination(root.path());
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        let tree = snapshot(&destination);
        let file = |path: &str| tree[Path::new(path)].as_deref();
        assert_eq!(
            file("docs/readme"),
            Some(&b"new readme"[..]),
            "{}",
            format.name
        );
        assert_eq!(file("docs/new/file"), Some(&b"new file"[..]));
        assert_eq!(file("added"), Some(&b"added"[..]));
        assert_eq!(file("docs/old/keep"), Some(&b"unrelated"[..]));
        assert_eq!(file("top"), Some(&b"unrelated top"[..]));
        assert_eq!(tree.len(), 8, "{}: {:?}", format.name, tree.keys());
    }
}

#[test]
fn existing_destination_is_refused_without_overwrite() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(&archive, &[Member::File("docs/readme", b"new")]);
        let destination = existing_destination(root.path());
        let before = snapshot(&destination);
        let error = (format.extract)(&archive, &destination, Existing::Refuse).unwrap_err();
        assert!(error.to_string().contains("must not exist"), "{error}");
        assert_eq!(snapshot(&destination), before);
    }
}

#[test]
fn overwrite_into_a_missing_destination_creates_it() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(&archive, &[Member::File("f", b"x")]);
        let destination = root.path().join("fresh");
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        assert_eq!(fs::read(destination.join("f")).unwrap(), b"x");
    }
}

#[test]
fn type_conflicts_fail_before_any_file_changes() {
    for format in &FORMATS {
        for members in [
            [
                Member::File("aaa", b"new"),
                Member::File("docs/old", b"file over directory"),
            ],
            [
                Member::File("aaa", b"new"),
                Member::File("top/inner", b"directory over file"),
            ],
        ] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join(format!("a.{}", format.name));
            (format.write)(&archive, &members);
            let destination = existing_destination(root.path());
            fs::write(destination.join("aaa"), b"old").unwrap();
            let before = snapshot(&destination);
            let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
            assert!(error.to_string().contains("in the destination"), "{error}");
            assert_eq!(snapshot(&destination), before, "{}", format.name);
        }
    }
}

#[test]
fn a_failed_archive_changes_nothing_and_leaves_no_staging() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(
            &archive,
            &[
                Member::File("docs/readme", b"new"),
                Member::File("z", b"last"),
            ],
        );
        let mut bytes = fs::read(&archive).unwrap();
        if format.name == "zip" {
            let at = bytes
                .windows(4)
                .position(|window| window == b"last")
                .unwrap();
            bytes[at] = b'L';
        } else {
            let length = bytes.len();
            bytes.truncate(length - 1024);
        }
        fs::write(&archive, bytes).unwrap();
        let destination = existing_destination(root.path());
        let before = snapshot(&destination);
        assert!((format.extract)(&archive, &destination, Existing::Overwrite).is_err());
        assert_eq!(snapshot(&destination), before, "{}", format.name);
    }
}

#[test]
fn a_destination_file_is_not_a_directory_to_merge_into() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive, &[Member::File("f", b"x")]);
    let destination = root.path().join("file");
    fs::write(&destination, b"keep").unwrap();
    let error = zip_reader(&archive, &destination, Existing::Overwrite).unwrap_err();
    assert!(error.to_string().contains("not a directory"), "{error}");
    assert_eq!(fs::read(&destination).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn symlinks_in_the_destination_are_replaced_never_followed() {
    use std::os::unix::fs::symlink;
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim"), b"outside").unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        let destination = existing_destination(root.path());
        symlink(outside.join("victim"), destination.join("victim")).unwrap();
        (format.write)(&archive, &[Member::File("victim", b"replaced the link")]);
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        assert!(
            fs::symlink_metadata(destination.join("victim"))
                .unwrap()
                .is_file()
        );
        assert_eq!(
            fs::read(destination.join("victim")).unwrap(),
            b"replaced the link"
        );
        assert_eq!(fs::read(outside.join("victim")).unwrap(), b"outside");

        symlink(&outside, destination.join("linked")).unwrap();
        (format.write)(
            &archive,
            &[Member::File("linked/victim", b"through the link")],
        );
        let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
        assert!(error.to_string().contains("in the destination"), "{error}");
        assert_eq!(fs::read(outside.join("victim")).unwrap(), b"outside");
    }
}

#[cfg(unix)]
#[test]
fn archive_symlinks_replace_destination_files() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive, &[Member::Symlink("docs/readme", "old/keep")]);
    let destination = existing_destination(root.path());
    zip_reader(&archive, &destination, Existing::Overwrite).unwrap();
    let link = destination.join("docs/readme");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&link).unwrap(), b"unrelated");
}

#[cfg(unix)]
#[test]
fn new_directories_get_archive_modes_and_existing_ones_keep_theirs() {
    use std::os::unix::fs::PermissionsExt;
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(
            &archive,
            &[
                Member::Directory("docs/", 0o500),
                Member::Directory("locked/", 0o500),
                Member::File("locked/file", b"x"),
            ],
        );
        let destination = existing_destination(root.path());
        let mode = |path: &str| {
            fs::metadata(destination.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        let docs = mode("docs");
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        assert_eq!(mode("docs"), docs, "{}", format.name);
        assert_eq!(mode("locked"), 0o500, "{}", format.name);
        assert_eq!(fs::read(destination.join("locked/file")).unwrap(), b"x");
        common::make_tree_deletable(root.path());
    }
}
