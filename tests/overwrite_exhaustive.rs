//! Extraction with `Existing::Overwrite` checked on every pair of small trees.
//! The destination holds any mix of files, folders and symlinks that point
//! outside it, and the archive any mix of files and folders, over two names
//! and two levels: 841 destinations and 121 archives. For each pair the real
//! ZIP reader merges the archive, and the result must match `expected`:
//! nothing outside the destination changes, a conflict changes nothing, and a
//! merge replaces exactly the archive's files. An archive folder needs a real
//! folder or nothing at its path, and an archive file needs no folder there.
//!
//! On a filesystem that ignores case, as macOS and Windows do by default, the
//! pairs run again with the archive names in capitals, and must give the same
//! outcome compared without case.
//!
//! It runs only when `ZIPPER_OVERWRITE_EXHAUSTIVE=1` is set, before a release
//! or when `cargo xtask verify-sync overwrite` asks, because it is too slow
//! for every push. It then checks every 37th pair, and
//! `ZIPPER_MERGE_STRIDE=1` checks all 101,761 pairs.
#![cfg(unix)]
mod common;

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};
use zipper::{Existing, native_zip};

const NAMES: [&str; 2] = ["a", "b"];
const DEPTH: usize = 2;
const STRIDE: &str = "ZIPPER_MERGE_STRIDE";
const OPT_IN: &str = "ZIPPER_OVERWRITE_EXHAUSTIVE";

fn invoked() -> bool {
    if std::env::var_os(OPT_IN).is_some() {
        return true;
    }
    assert!(
        std::env::var_os(STRIDE).is_none(),
        "{STRIDE} asks for a run, so set {OPT_IN}=1 too; nothing was checked"
    );
    eprintln!("skipped: set {OPT_IN}=1 to run the exhaustive overwrite check");
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    LinkToOutsideFolder,
    LinkToOutsideFile,
}

type Tree = BTreeMap<String, Kind>;

fn forest(prefix: &str, depth: usize, leaves: &[Kind]) -> Vec<Tree> {
    let mut trees = vec![Tree::new()];
    for name in NAMES {
        let path = format!("{prefix}{name}");
        let mut options = vec![Tree::new()];
        options.extend(
            leaves
                .iter()
                .map(|&kind| Tree::from([(path.clone(), kind)])),
        );
        let children = if depth > 1 {
            forest(&format!("{path}/"), depth - 1, leaves)
        } else {
            vec![Tree::new()]
        };
        for child in children {
            let mut tree = child;
            tree.insert(path.clone(), Kind::Directory);
            options.push(tree);
        }
        trees = trees
            .iter()
            .flat_map(|tree| {
                options.iter().map(move |option| {
                    let mut merged = tree.clone();
                    merged.extend(option.clone());
                    merged
                })
            })
            .collect();
    }
    trees
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    File(Vec<u8>),
    Directory,
    Link(PathBuf),
}

type Snapshot = BTreeMap<String, Node>;

fn snapshot(root: &Path) -> Snapshot {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let path = entry.path().strip_prefix(root).unwrap();
            let node = if entry.file_type().is_symlink() {
                Node::Link(fs::read_link(entry.path()).unwrap())
            } else if entry.file_type().is_dir() {
                Node::Directory
            } else {
                Node::File(fs::read(entry.path()).unwrap())
            };
            (path.to_str().unwrap().to_owned(), node)
        })
        .collect()
}

fn contents(path: &str) -> Vec<u8> {
    format!("archive {}", path.to_lowercase()).into_bytes()
}

fn without_case(snapshot: Snapshot) -> Snapshot {
    snapshot
        .into_iter()
        .map(|(path, node)| (path.to_lowercase(), node))
        .collect()
}

fn ignores_case(folder: &Path) -> bool {
    fs::write(folder.join("probe"), b"").unwrap();
    fs::symlink_metadata(folder.join("PROBE")).is_ok()
}

fn write_zip(path: &Path, tree: &Tree, spell: fn(&str) -> String) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, kind) in tree {
        let name = spell(name);
        let options = zip::write::SimpleFileOptions::default();
        if *kind == Kind::Directory {
            writer.add_directory(format!("{name}/"), options).unwrap();
        } else {
            writer.start_file(name.as_str(), options).unwrap();
            writer.write_all(&contents(&name)).unwrap();
        }
    }
    writer.finish().unwrap();
}

fn build(root: &Path, destination: &Tree) {
    fs::create_dir_all(root.join("outside/folder")).unwrap();
    fs::write(root.join("outside/file"), b"outside").unwrap();
    fs::write(root.join("outside/folder/inner"), b"outside inner").unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    for (name, kind) in destination {
        let path = root.join("dest").join(name);
        let depth = name.matches('/').count() + 1;
        let up = "../".repeat(depth);
        match kind {
            Kind::Directory => fs::create_dir(&path).unwrap(),
            Kind::File => fs::write(&path, format!("old {name}")).unwrap(),
            Kind::LinkToOutsideFolder => symlink(format!("{up}outside/folder"), &path).unwrap(),
            Kind::LinkToOutsideFile => symlink(format!("{up}outside/file"), &path).unwrap(),
        }
    }
}

fn renamed(name: &str) -> String {
    format!("{name}_1")
}

fn documented_keep_or_rename(
    before: &Snapshot,
    destination: &Tree,
    archive: &Tree,
    mode: Existing,
) -> Snapshot {
    let mut after = before.clone();
    let mut skipped: Vec<&str> = Vec::new();
    let mut moved: Vec<(&str, String)> = Vec::new();
    let node = |kind: &Kind, name: &str| match kind {
        Kind::Directory => Node::Directory,
        _ => Node::File(contents(name)),
    };
    for (name, kind) in archive {
        if skipped
            .iter()
            .any(|skip| name.starts_with(&format!("{skip}/")))
        {
            continue;
        }
        if let Some((from, to)) = moved
            .iter()
            .find(|(from, _)| name.starts_with(&format!("{from}/")))
        {
            after.insert(
                format!("dest/{to}{}", &name[from.len()..]),
                node(kind, name),
            );
            continue;
        }
        match (kind, destination.get(name)) {
            (_, None) => {
                after.insert(format!("dest/{name}"), node(kind, name));
            }
            (Kind::Directory, Some(Kind::Directory)) => {}
            (_, Some(_)) => match mode {
                Existing::Skip => skipped.push(name),
                Existing::RenameNew => {
                    after.insert(format!("dest/{}", renamed(name)), node(kind, name));
                    moved.push((name, renamed(name)));
                }
                Existing::RenameExisting => {
                    let old = format!("dest/{name}");
                    let aside: Vec<(String, Node)> = before
                        .iter()
                        .filter(|(path, _)| **path == old || path.starts_with(&format!("{old}/")))
                        .map(|(path, node)| (path.clone(), node.clone()))
                        .collect();
                    for (path, node) in aside {
                        after.remove(&path);
                        let rest = &path[old.len()..];
                        after.insert(format!("dest/{}{rest}", renamed(name)), node);
                    }
                    after.insert(old, node(kind, name));
                }
                _ => unreachable!("only the keep and rename modes"),
            },
        }
    }
    after
}

fn documented_outcome(before: &Snapshot, destination: &Tree, archive: &Tree) -> Option<Snapshot> {
    for (name, kind) in archive {
        let parent_is_new = name
            .rfind('/')
            .is_some_and(|slash| !destination.contains_key(&name[..slash]));
        if parent_is_new {
            continue;
        }
        match (kind, destination.get(name)) {
            (_, None) => {}
            (Kind::Directory, Some(Kind::Directory)) => {}
            (Kind::Directory, Some(_)) | (Kind::File, Some(Kind::Directory)) => return None,
            (Kind::File, Some(_)) => {}
            _ => unreachable!("archives hold files and folders"),
        }
    }
    let mut after = before.clone();
    for (name, kind) in archive {
        let node = match kind {
            Kind::Directory => Node::Directory,
            _ => Node::File(contents(name)),
        };
        after.insert(format!("dest/{name}"), node);
    }
    Some(after)
}

#[test]
fn overwrite_matches_the_contract_on_every_small_tree_pair() {
    if !invoked() {
        return;
    }
    check_every_pair(str::to_owned, Snapshot::from);
}

#[test]
fn skip_and_rename_modes_match_their_contracts_on_every_small_tree_pair() {
    if !invoked() {
        return;
    }
    for mode in [
        Existing::Skip,
        Existing::RenameNew,
        Existing::RenameExisting,
    ] {
        check_mode(mode, str::to_owned, Snapshot::from);
    }
}

#[test]
fn overwrite_with_names_in_another_case_matches_the_contract() {
    if !invoked() {
        return;
    }
    let probe = tempfile::tempdir().unwrap();
    if !ignores_case(probe.path()) {
        eprintln!("skipped: this filesystem tells names apart by case");
        return;
    }
    check_every_pair(str::to_uppercase, without_case);
    for mode in [
        Existing::Skip,
        Existing::RenameNew,
        Existing::RenameExisting,
    ] {
        check_mode(mode, str::to_uppercase, without_case);
    }
}

fn check_every_pair(spell: fn(&str) -> String, compare: fn(Snapshot) -> Snapshot) {
    check_mode(Existing::Overwrite, spell, compare);
}

fn check_mode(mode: Existing, spell: fn(&str) -> String, compare: fn(Snapshot) -> Snapshot) {
    let destinations = forest(
        "",
        DEPTH,
        &[
            Kind::File,
            Kind::LinkToOutsideFolder,
            Kind::LinkToOutsideFile,
        ],
    );
    let archives = forest("", DEPTH, &[Kind::File]);
    assert_eq!((destinations.len(), archives.len()), (841, 121));
    let stride: usize = std::env::var(STRIDE).map_or(37, |value| value.parse().unwrap());
    let fixtures = tempfile::tempdir().unwrap();
    let zips: Vec<PathBuf> = archives
        .iter()
        .enumerate()
        .map(|(index, tree)| {
            let path = fixtures.path().join(format!("{index}.zip"));
            write_zip(&path, tree, spell);
            path
        })
        .collect();
    const PAIRS_PER_PROGRESS_LINE: usize = 5000;
    let total = (destinations.len() * archives.len()).div_ceil(stride);
    let started = std::time::Instant::now();
    let (mut merged, mut refused, mut checked) = (0, 0, 0_usize);
    for (case, (destination, archive)) in destinations
        .iter()
        .flat_map(|destination| {
            archives
                .iter()
                .zip(&zips)
                .map(move |archive| (destination, archive))
        })
        .enumerate()
        .filter(|(case, _)| case % stride == 0)
    {
        let (archive_tree, zip) = archive;
        let root = tempfile::tempdir().unwrap();
        build(root.path(), destination);
        let before = snapshot(root.path());
        let result = native_zip::read_archive_with_options(
            zip,
            Some(&root.path().join("dest")),
            native_zip::ReadOptions {
                existing: mode,
                ..Default::default()
            },
        );
        let after = snapshot(root.path());
        let label =
            format!("{mode:?} case {case}: destination {destination:?}, archive {archive_tree:?}");
        let wanted = match mode {
            Existing::Overwrite => documented_outcome(&before, destination, archive_tree),
            _ => Some(documented_keep_or_rename(
                &before,
                destination,
                archive_tree,
                mode,
            )),
        };
        match wanted {
            Some(wanted) => {
                result.unwrap_or_else(|error| panic!("{label}: refused with {error:#}"));
                assert_eq!(compare(after), compare(wanted), "{label}");
                merged += 1;
            }
            None => {
                let error = result.expect_err(&label).to_string();
                assert!(error.contains("in the destination"), "{label}: {error}");
                assert_eq!(compare(after), compare(before), "{label}");
                refused += 1;
            }
        }
        checked += 1;
        if checked.is_multiple_of(PAIRS_PER_PROGRESS_LINE) {
            eprintln!(
                "{mode:?}: {checked} of {total} pairs after {:.0?}",
                started.elapsed()
            );
        }
        common::make_tree_deletable(root.path());
    }
    eprintln!("{mode:?}: {checked} pairs, {merged} merged, {refused} refused as conflicts");
}
