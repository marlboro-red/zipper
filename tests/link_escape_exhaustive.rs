//! Archive links never resolve outside the destination, checked on every
//! pair of small destination and archive trees in every `Existing` mode that
//! merges. The destination's names `a` and `b` each hold nothing, a file, a
//! folder, a link to a folder outside, or a folder holding such a link. The
//! archive's `a` and `b` each hold nothing, a file, a folder or a link, and a
//! link `l` points through them. After each run, every link the archive
//! placed is followed on the real filesystem and must land inside the
//! destination. A refused run must leave the destination unchanged and no
//! staging behind, and nothing outside may ever change.
//!
//! It runs only when `ZIPPER_LINK_ESCAPE=1` is set, before a release or when
//! `cargo xtask verify-sync link-escape` asks, because it is too slow for
//! every push. It then checks every 13th run, and `ZIPPER_LINK_STRIDE=1`
//! checks all 25,200.
mod common;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use zipper::{Existing, native_zip};

const STRIDE: &str = "ZIPPER_LINK_STRIDE";
const OPT_IN: &str = "ZIPPER_LINK_ESCAPE";

fn link_to_folder(target: &str, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target.replace('/', "\\"), path).unwrap();
}
const OUTSIDE_FROM_TOP: &str = "../outside";
const OUTSIDE_FROM_FOLDER: &str = "../../outside";
const LINK_TARGETS: [&str; 6] = ["a/k/s", "b/k/s", "a/s", "k/s", "a", "."];

#[derive(Clone, Copy, Debug)]
enum Kept {
    Nothing,
    File,
    Folder,
    LinkOut,
    FolderWithLinkOut,
}

#[derive(Clone, Copy, Debug)]
enum Packed {
    Nothing,
    File,
    Folder,
    Link(&'static str),
}

const KEPT: [Kept; 5] = [
    Kept::Nothing,
    Kept::File,
    Kept::Folder,
    Kept::LinkOut,
    Kept::FolderWithLinkOut,
];
const PACKED: [Packed; 6] = [
    Packed::Nothing,
    Packed::File,
    Packed::Folder,
    Packed::Link("."),
    Packed::Link("a"),
    Packed::Link("b"),
];

fn build_destination(destination: &Path, layout: [(&str, Kept); 2]) {
    fs::create_dir(destination).unwrap();
    for (name, kept) in layout {
        let path = destination.join(name);
        match kept {
            Kept::Nothing => {}
            Kept::File => fs::write(&path, b"kept").unwrap(),
            Kept::Folder => fs::create_dir(&path).unwrap(),
            Kept::LinkOut => link_to_folder(OUTSIDE_FROM_TOP, &path),
            Kept::FolderWithLinkOut => {
                fs::create_dir(&path).unwrap();
                link_to_folder(OUTSIDE_FROM_FOLDER, &path.join("k"));
            }
        }
    }
}

fn write_archive(path: &Path, members: &[(&str, Packed)]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, packed) in members {
        match packed {
            Packed::Nothing => {}
            Packed::File => {
                writer.start_file(*name, options).unwrap();
                std::io::Write::write_all(&mut writer, b"archive").unwrap();
            }
            Packed::Folder => writer.add_directory(*name, options).unwrap(),
            Packed::Link(target) => writer.add_symlink(*name, *target, options).unwrap(),
        }
    }
    writer.finish().unwrap();
}

type Snapshot = BTreeMap<PathBuf, String>;

fn snapshot(root: &Path) -> Snapshot {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let kind = if entry.file_type().is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                format!("link {}", target.display().to_string().replace('\\', "/"))
            } else if entry.file_type().is_dir() {
                "folder".to_owned()
            } else {
                format!("file {:?}", fs::read(entry.path()).unwrap())
            };
            (entry.path().strip_prefix(root).unwrap().to_owned(), kind)
        })
        .collect()
}

fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn lands(path: &Path, hops: usize) -> PathBuf {
    assert!(hops < 64, "{} goes through too many links", path.display());
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    if let Ok(target) = fs::read_link(path) {
        return lands(&normalized(&path.parent().unwrap().join(target)), hops + 1);
    }
    let (mut ancestor, mut rest) = (path.to_path_buf(), Vec::new());
    while fs::symlink_metadata(&ancestor).is_err() {
        rest.push(ancestor.file_name().unwrap().to_owned());
        assert!(
            ancestor.pop(),
            "{} has no existing ancestor",
            path.display()
        );
    }
    let mut landed = lands(&ancestor, hops + 1);
    landed.extend(rest.iter().rev());
    normalized(&landed)
}

#[test]
fn archive_links_land_inside_the_destination_on_every_small_tree_pair() {
    if std::env::var_os(OPT_IN).is_none() {
        assert!(
            std::env::var_os(STRIDE).is_none(),
            "{STRIDE} asks for a run, so set {OPT_IN}=1 too; nothing was checked"
        );
        eprintln!("skipped: set {OPT_IN}=1 to run the link escape check");
        return;
    }
    let stride: usize = std::env::var(STRIDE).map_or(13, |value| value.parse().unwrap());
    let fixtures = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for a in PACKED {
        for b in PACKED {
            for l in std::iter::once(None).chain(LINK_TARGETS.iter().copied().map(Some)) {
                let path = fixtures.path().join(format!("{}.zip", archives.len()));
                let mut members = vec![("a", a), ("b", b)];
                if let Some(target) = l {
                    members.push(("l", Packed::Link(target)));
                }
                write_archive(&path, &members);
                archives.push((path, format!("a {a:?}, b {b:?}, l {l:?}")));
            }
        }
    }
    let modes = [
        Existing::Overwrite,
        Existing::Skip,
        Existing::RenameNew,
        Existing::RenameExisting,
    ];
    let (mut extracted, mut refused, mut checked) = (0, 0, 0);
    let cases = KEPT
        .iter()
        .flat_map(|&a| KEPT.iter().map(move |&b| [("a", a), ("b", b)]))
        .flat_map(|layout| archives.iter().map(move |archive| (layout, archive)))
        .flat_map(|case| modes.iter().map(move |&mode| (case, mode)));
    for (case, ((layout, (archive, description)), mode)) in cases.enumerate() {
        if case % stride != 0 {
            continue;
        }
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("s"), b"outside").unwrap();
        let destination = root.path().join("dest");
        build_destination(&destination, layout);
        let before = snapshot(root.path());
        let result = native_zip::read_archive_with_options(
            archive,
            Some(&destination),
            native_zip::ReadOptions {
                existing: mode,
                ..Default::default()
            },
        );
        let label = format!("case {case}, {mode:?}: destination {layout:?}, archive {description}");
        let after = snapshot(root.path());
        let outside_part = |snapshot: &Snapshot| -> Snapshot {
            snapshot
                .iter()
                .filter(|(path, _)| path.starts_with("outside"))
                .map(|(path, kind)| (path.clone(), kind.clone()))
                .collect()
        };
        assert_eq!(
            outside_part(&after),
            outside_part(&before),
            "{label}: outside changed"
        );
        match result {
            Err(_) => {
                assert_eq!(
                    after, before,
                    "{label}: a refused run changed the destination"
                );
                refused += 1;
            }
            Ok(_) => {
                let inside = fs::canonicalize(&destination).unwrap();
                for (path, kind) in &after {
                    let kept = kind == &format!("link {OUTSIDE_FROM_TOP}")
                        || kind == &format!("link {OUTSIDE_FROM_FOLDER}");
                    if !kind.starts_with("link ") || kept || !path.starts_with("dest") {
                        continue;
                    }
                    let landing = lands(&root.path().join(path), 0);
                    assert!(
                        landing.starts_with(&inside),
                        "{label}: {} lands at {}",
                        path.display(),
                        landing.display()
                    );
                }
                extracted += 1;
            }
        }
        checked += 1;
        common::make_tree_deletable(root.path());
    }
    eprintln!("{checked} runs: {extracted} extracted, {refused} refused");
}
