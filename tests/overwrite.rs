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
fn a_merge_whose_paths_pass_windows_260_character_limit_replaces_and_adds_files() {
    const WINDOWS_MAX_PATH: usize = 260;
    let root = tempfile::tempdir().unwrap();
    let deep = ["d".repeat(80), "e".repeat(80), "f".repeat(80)].join("/");
    let destination = root.path().join("out");
    fs::create_dir_all(destination.join(&deep)).unwrap();
    let replaced = format!("{deep}/replaced.txt");
    let added = format!("{deep}/{}", "n".repeat(60));
    fs::write(destination.join(&replaced), b"old").unwrap();
    assert!(destination.join(&added).as_os_str().len() > WINDOWS_MAX_PATH);
    let archive = root.path().join("a.zip");
    write_zip(
        &archive,
        &[
            Member::File(&replaced, b"new"),
            Member::File(&added, b"added"),
        ],
    );
    zip_reader(&archive, &destination, Existing::Overwrite).unwrap();
    let tree = snapshot(&destination);
    assert_eq!(tree.get(Path::new(&replaced)), Some(&Some(b"new".to_vec())));
    assert_eq!(tree.get(Path::new(&added)), Some(&Some(b"added".to_vec())));
    assert_eq!(tree.len(), 5, "{:?}", tree.keys());
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

fn link_to_folder(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn names_in_another_case_meet_the_same_checks_where_case_is_ignored() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let destination = existing_destination(root.path());
        if fs::symlink_metadata(destination.join("TOP")).is_err() {
            eprintln!("skipped: this filesystem tells names apart by case");
            return;
        }
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim"), b"outside").unwrap();
        link_to_folder(&outside, &destination.join("linked"));
        let archive = root.path().join(format!("a.{}", format.name));
        for members in [
            [Member::File("LINKED/victim", b"through the link")],
            [Member::File("DOCS", b"file over directory")],
        ] {
            (format.write)(&archive, &members);
            let before = snapshot(root.path());
            let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
            assert!(
                error.to_string().contains("in the destination"),
                "{}: {error}",
                format.name
            );
            assert_eq!(snapshot(root.path()), before, "{}", format.name);
        }
        (format.write)(&archive, &[Member::File("TOP", b"new top")]);
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        assert_eq!(fs::read(destination.join("top")).unwrap(), b"new top");
        let tops = fs::read_dir(&destination)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .eq_ignore_ascii_case("top")
            })
            .count();
        assert_eq!(tops, 1, "{}", format.name);
        assert_eq!(fs::read(outside.join("victim")).unwrap(), b"outside");
    }
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

#[test]
fn a_name_too_long_to_rename_is_refused_before_anything_is_published() {
    let long = "x".repeat(255);
    for existing in [Existing::RenameNew, Existing::RenameExisting] {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join(&long), b"old").unwrap();
        let archive = root.path().join("a.zip");
        let names: Vec<String> = (0..20).map(|index| format!("new{index:02}")).collect();
        let mut members: Vec<Member> = names
            .iter()
            .map(|name| Member::File(name, b"new"))
            .collect();
        members.push(Member::File(&long, b"archive"));
        write_zip(&archive, &members);
        let before = snapshot(&destination);
        let error = zip_reader(&archive, &destination, existing).unwrap_err();
        assert!(
            format!("{error:#}").contains("longer than a file name"),
            "{error:#}"
        );
        assert_eq!(snapshot(&destination), before, "{existing:?}");
    }
}

#[cfg(unix)]
#[test]
fn archive_links_never_point_through_a_link_the_destination_keeps() {
    use std::os::unix::fs::symlink;
    for (existing, with_folder, refused) in [
        (Existing::Skip, true, true),
        (Existing::RenameNew, true, true),
        (Existing::RenameExisting, true, false),
        (Existing::Overwrite, false, true),
        (Existing::Skip, false, true),
        (Existing::RenameNew, false, true),
        (Existing::RenameExisting, false, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"outside").unwrap();
        let destination = root.path().join("out");
        fs::create_dir(&destination).unwrap();
        symlink("../outside", destination.join("d")).unwrap();
        let archive = root.path().join("a.zip");
        let mut members = vec![Member::Symlink("l", "d/secret")];
        if with_folder {
            members.push(Member::Directory("d/", 0o755));
            members.push(Member::File("d/secret", b"archive"));
        }
        write_zip(&archive, &members);
        let before = snapshot(root.path());
        let result = zip_reader(&archive, &destination, existing);
        let label = format!("{existing:?} with the folder {with_folder}");
        if refused {
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains("points through d"), "{label}: {error}");
            assert_eq!(snapshot(root.path()), before, "{label}");
        } else {
            result.unwrap();
            let resolved = fs::canonicalize(destination.join("l")).unwrap();
            assert!(
                resolved.starts_with(fs::canonicalize(&destination).unwrap()),
                "{label}"
            );
            assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");
        }
    }
}

#[cfg(unix)]
#[test]
fn archive_links_never_reach_a_kept_link_through_another_archive_link() {
    use std::os::unix::fs::symlink;
    for (existing, layout) in [
        Existing::Overwrite,
        Existing::Skip,
        Existing::RenameNew,
        Existing::RenameExisting,
    ]
    .into_iter()
    .flat_map(|existing| [(existing, "folder"), (existing, "dot")])
    {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"outside").unwrap();
        let destination = root.path().join("out");
        let kept = if layout == "folder" {
            fs::create_dir_all(destination.join("d")).unwrap();
            symlink("../../outside", destination.join("d/kept")).unwrap();
            "d"
        } else {
            fs::create_dir(&destination).unwrap();
            symlink("../outside", destination.join("kept")).unwrap();
            "."
        };
        let archive = root.path().join("a.zip");
        write_zip(
            &archive,
            &[
                Member::Symlink("a", kept),
                Member::Symlink("l", "a/kept/secret"),
            ],
        );
        let before = snapshot(root.path());
        let error = format!(
            "{:#}",
            zip_reader(&archive, &destination, existing).unwrap_err()
        );
        assert!(
            error.contains("points through"),
            "{existing:?} {layout}: {error}"
        );
        assert_eq!(snapshot(root.path()), before, "{existing:?} {layout}");
    }
}

#[cfg(unix)]
type UnixTree = BTreeMap<PathBuf, (u32, Option<Vec<u8>>, Option<PathBuf>)>;

#[cfg(unix)]
fn unix_tree(root: &Path) -> UnixTree {
    use std::os::unix::fs::PermissionsExt;
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let path = entry.path().strip_prefix(root).unwrap().to_owned();
            let metadata = entry.path().symlink_metadata().unwrap();
            let content = entry
                .file_type()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            let target = entry
                .file_type()
                .is_symlink()
                .then(|| fs::read_link(entry.path()).unwrap());
            (path, (metadata.permissions().mode(), content, target))
        })
        .collect()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
const REPLACED: [&str; 10] = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9"];

#[cfg(unix)]
struct FailingRename {
    name: &'static str,
    members: &'static [Member<'static>],
    prepare: fn(&Path),
    at_fault: &'static str,
}

#[cfg(unix)]
const FAILING_RENAMES: [FailingRename; 3] = [
    FailingRename {
        name: "a new file in a read-only folder",
        members: &[
            Member::Directory("d/", 0o555),
            Member::File("d/f", b"f"),
            Member::Directory("e/", 0o555),
            Member::File("e/g", b"g"),
        ],
        prepare: |destination| {
            fs::create_dir(destination.join("e")).unwrap();
            set_mode(&destination.join("e"), 0o555);
        },
        at_fault: "e/g",
    },
    FailingRename {
        name: "a new file in a read-only folder deep in the tree",
        members: &[
            Member::File("a/x", b"new x"),
            Member::File("a/b/c/new", b"new"),
            Member::File("z/new", b"new"),
        ],
        prepare: |destination| {
            fs::create_dir_all(destination.join("a/b/c")).unwrap();
            fs::write(destination.join("a/x"), b"old x").unwrap();
            set_mode(&destination.join("a/b/c"), 0o555);
        },
        at_fault: "a/b/c/new",
    },
    FailingRename {
        name: "a read-only file in a read-only folder",
        members: &[
            Member::File("a/x", b"new x"),
            Member::File("a/locked/file", b"new"),
        ],
        prepare: |destination| {
            fs::create_dir_all(destination.join("a/locked")).unwrap();
            fs::write(destination.join("a/x"), b"old x").unwrap();
            fs::write(destination.join("a/locked/file"), b"old").unwrap();
            set_mode(&destination.join("a/locked/file"), 0o444);
            set_mode(&destination.join("a/locked"), 0o555);
        },
        at_fault: "a/locked/file",
    },
];

#[cfg(unix)]
#[test]
fn a_rename_that_fails_partway_leaves_the_destination_as_it_was() {
    for format in &FORMATS {
        for case in &FAILING_RENAMES {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join(format!("a.{}", format.name));
            let mut members: Vec<Member> = REPLACED
                .iter()
                .map(|name| Member::File(name, b"new"))
                .collect();
            members.extend(case.members.iter().map(|member| match *member {
                Member::File(name, data) => Member::File(name, data),
                Member::Directory(name, mode) => Member::Directory(name, mode),
                Member::Symlink(name, target) => Member::Symlink(name, target),
            }));
            (format.write)(&archive, &members);
            let destination = root.path().join("out");
            fs::create_dir(&destination).unwrap();
            for name in REPLACED {
                fs::write(destination.join(name), format!("old {name}")).unwrap();
            }
            (case.prepare)(&destination);
            let before = unix_tree(&destination);
            let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
            let at_fault = destination.join(case.at_fault);
            assert!(
                format!("{error:#}").contains(&at_fault.display().to_string()),
                "{} {}: {error:#}",
                format.name,
                case.name
            );
            assert_eq!(
                unix_tree(&destination),
                before,
                "{} {}: {error:#}",
                format.name,
                case.name
            );
            common::make_tree_deletable(root.path());
        }
    }
}

#[test]
fn a_read_only_file_in_a_writable_folder_is_replaced() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(&archive, &[Member::File("locked", b"new")]);
        let destination = root.path().join("out");
        fs::create_dir(&destination).unwrap();
        let locked = destination.join("locked");
        fs::write(&locked, b"old").unwrap();
        let mut permissions = fs::metadata(&locked).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&locked, permissions).unwrap();
        (format.extract)(&archive, &destination, Existing::Overwrite).unwrap();
        let tree = snapshot(&destination);
        assert_eq!(tree.len(), 1, "{}: {:?}", format.name, tree.keys());
        assert_eq!(tree[Path::new("locked")].as_deref(), Some(&b"new"[..]));
        let metadata = fs::metadata(&locked).unwrap();
        assert!(!metadata.permissions().readonly(), "{}", format.name);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o777,
            0o644,
            "{}",
            format.name
        );
    }
}

#[cfg(unix)]
#[test]
fn a_failed_rename_existing_keeps_the_spelling_the_destination_had() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    let upper: Vec<String> = REPLACED.iter().map(|name| name.to_uppercase()).collect();
    let mut members: Vec<Member> = upper
        .iter()
        .map(|name| Member::File(name, b"new"))
        .collect();
    members.push(Member::File("cafe\u{301}.txt", b"new"));
    members.push(Member::File("HL", b"new"));
    members.push(Member::File("q/new", b"new"));
    write_zip(&archive, &members);
    let destination = root.path().join("out");
    fs::create_dir_all(destination.join("q")).unwrap();
    for name in REPLACED {
        fs::write(destination.join(name), b"old").unwrap();
    }
    fs::write(destination.join("caf\u{e9}.txt"), b"old").unwrap();
    fs::write(destination.join("hl"), b"old").unwrap();
    fs::hard_link(destination.join("hl"), destination.join("other")).unwrap();
    set_mode(&destination.join("q"), 0o555);
    let before = unix_tree(&destination);
    let error = zip_reader(&archive, &destination, Existing::RenameExisting).unwrap_err();
    let at_fault = destination.join("q/new").display().to_string();
    assert!(format!("{error:#}").contains(&at_fault), "{error:#}");
    assert_eq!(unix_tree(&destination), before, "{error:#}");
    common::make_tree_deletable(root.path());
}

#[cfg(unix)]
#[test]
fn a_destination_folder_that_cannot_be_read_is_named() {
    for format in &FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{}", format.name));
        (format.write)(&archive, &[Member::File("closed/f", b"new")]);
        let destination = root.path().join("out");
        fs::create_dir_all(destination.join("closed")).unwrap();
        set_mode(&destination.join("closed"), 0o600);
        let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
        let at_fault = destination.join("closed/f").display().to_string();
        assert!(
            format!("{error:#}").contains(&at_fault),
            "{}: {error:#}",
            format.name
        );
        common::make_tree_deletable(root.path());
    }
}

#[cfg(target_os = "macos")]
struct DiskImage {
    mount: PathBuf,
    _folder: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl DiskImage {
    fn new(filesystem: &str) -> DiskImage {
        let folder = tempfile::tempdir().unwrap();
        let image = folder.path().join("disk.dmg");
        let mount = folder.path().join("mount");
        fs::create_dir(&mount).unwrap();
        let run = |arguments: &[&std::ffi::OsStr]| {
            let status = std::process::Command::new("hdiutil")
                .args(arguments)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "hdiutil {arguments:?}");
        };
        run(&[
            "create".as_ref(),
            "-size".as_ref(),
            "40m".as_ref(),
            "-fs".as_ref(),
            filesystem.as_ref(),
            "-volname".as_ref(),
            "ZIPPER".as_ref(),
            image.as_os_str(),
        ]);
        run(&[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            image.as_os_str(),
        ]);
        DiskImage {
            mount,
            _folder: folder,
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for DiskImage {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .args(["detach".as_ref(), "-force".as_ref(), self.mount.as_os_str()])
            .stdout(std::process::Stdio::null())
            .status();
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches FAT32 and exFAT disk images with hdiutil"]
fn a_merge_into_fat_or_exfat_is_refused_before_any_change() {
    for (filesystem, named) in [("MS-DOS FAT32", "msdos"), ("ExFAT", "exfat")] {
        let disk = DiskImage::new(filesystem);
        for format in &FORMATS {
            let archive = disk.mount.join(format!("a.{}", format.name));
            (format.write)(
                &archive,
                &[
                    Member::File("old", b"new"),
                    Member::File("added", b"added"),
                    Member::File("sub/x", b"x"),
                ],
            );
            let destination = disk.mount.join(format!("out-{}", format.name));
            fs::create_dir_all(destination.join("sub")).unwrap();
            fs::write(destination.join("old"), b"old").unwrap();
            fs::write(destination.join("kept"), b"kept").unwrap();
            let before = snapshot(&destination);
            let error = (format.extract)(&archive, &destination, Existing::Overwrite).unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains(named) && message.contains("new folder"),
                "{filesystem} {}: {message}",
                format.name
            );
            assert_eq!(
                snapshot(&destination),
                before,
                "{filesystem} {}: {message}",
                format.name
            );
            let fresh = disk.mount.join(format!("fresh-{}", format.name));
            (format.extract)(&archive, &fresh, Existing::Overwrite).unwrap();
            assert_eq!(fs::read(fresh.join("old")).unwrap(), b"new");
            assert_eq!(fs::read(fresh.join("sub/x")).unwrap(), b"x");
        }
    }
}
