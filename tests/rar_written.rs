//! Every archive RARLAB's `rar` wrote into `tests/data/rar-written` extracts
//! to the tree `scripts/make_rar_corpus.py` recorded from its source, with
//! the same bytes, links, folders, modes and RAR 5 times. Every archive that
//! uses a feature Zipper refuses is refused with the reason the manifest
//! names, and leaves nothing behind. Where `unrar` is installed, each archive
//! Zipper reads extracts to the tree `unrar` extracts, and `unrar` extracts
//! each refused one. Where 7-Zip is installed, each archive extracts to the
//! tree 7-Zip extracts wherever it decodes the archive, which must include
//! the stored ones. Without them those comparisons are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_UNRAR` or `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/unrar.rs"]
mod unrar;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_rar::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rar-written");
const PASSWORD: &str = "zipper";
const STORED: [&str; 2] = ["rar5-m0.rar", "rar4-m0.rar"];

#[derive(Debug, PartialEq, Eq, Clone)]
enum Node {
    Folder,
    File {
        sha256: String,
        mode: Option<u32>,
        mtime_ns: Option<i128>,
    },
    Link(String),
}

type Tree = BTreeMap<String, Node>;

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn mode_of(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode_of(_: &fs::Metadata) -> Option<u32> {
    None
}

fn mtime_ns(metadata: &fs::Metadata) -> i128 {
    let since = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128;
    if cfg!(windows) {
        since - since % 100
    } else {
        since
    }
}

fn read_unlocked(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o600)).unwrap();
        let bytes = fs::read(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        bytes
    }
    #[cfg(not(unix))]
    fs::read(path).unwrap()
}

fn tree(root: &Path) -> Tree {
    let mut found = Tree::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry.unwrap();
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(entry.path()).unwrap();
        let node = if metadata.file_type().is_symlink() {
            let target = fs::read_link(entry.path()).unwrap();
            Node::Link(
                target
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            )
        } else if metadata.is_dir() {
            Node::Folder
        } else {
            Node::File {
                sha256: sha256(&read_unlocked(entry.path())),
                mode: mode_of(&metadata),
                mtime_ns: Some(mtime_ns(&metadata)),
            }
        };
        found.insert(name, node);
    }
    found
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

#[cfg(unix)]
fn umask_ceiling(work: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let probe = work.join("probe");
    fs::create_dir(&probe).unwrap();
    let mode = fs::metadata(&probe).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&probe).unwrap();
    Some(mode)
}

#[cfg(not(unix))]
fn umask_ceiling(_: &Path) -> Option<u32> {
    None
}

struct Archive {
    name: String,
    tree: Option<Tree>,
    refused: Option<String>,
    files: Vec<String>,
}

fn manifest(ceiling: Option<u32>) -> Vec<Archive> {
    let text = fs::read_to_string(Path::new(CORPUS).join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    manifest["archives"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, entry)| Archive {
            name: name.clone(),
            tree: entry.get("tree").map(|tree| recorded(tree, ceiling)),
            refused: entry
                .get("refused")
                .map(|reason| reason.as_str().unwrap().to_owned()),
            files: entry["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| file.as_str().unwrap().to_owned())
                .collect(),
        })
        .collect()
}

fn recorded(entries: &serde_json::Value, ceiling: Option<u32>) -> Tree {
    entries
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, node)| {
            let value = if node.get("dir").is_some() {
                Node::Folder
            } else if let Some(target) = node.get("link") {
                Node::Link(target.as_str().unwrap().to_owned())
            } else {
                Node::File {
                    sha256: node["sha256"].as_str().unwrap().to_owned(),
                    mode: node.get("mode").and_then(|mode| {
                        let mode =
                            u32::from_str_radix(mode.as_str()?.trim_start_matches("0o"), 8).ok()?;
                        Some(mode & ceiling?)
                    }),
                    mtime_ns: node
                        .get("mtime_ns")
                        .and_then(serde_json::Value::as_i64)
                        .map(|ns| {
                            let ns = i128::from(ns);
                            if cfg!(windows) { ns - ns % 100 } else { ns }
                        }),
                }
            };
            (name.clone(), value)
        })
        .collect()
}

fn without_unrecorded_fields(mut actual: Tree, wanted: &Tree) -> Tree {
    for (name, node) in &mut actual {
        if let (
            Node::File { mode, mtime_ns, .. },
            Some(Node::File {
                mode: wanted_mode,
                mtime_ns: wanted_mtime,
                ..
            }),
        ) = (node, wanted.get(name))
        {
            if wanted_mode.is_none() {
                *mode = None;
            }
            if wanted_mtime.is_none() {
                *mtime_ns = None;
            }
        }
    }
    actual
}

fn without_times(mut found: Tree) -> Tree {
    for node in found.values_mut() {
        if let Node::File { mtime_ns, .. } = node {
            *mtime_ns = None;
        }
    }
    found
}

fn corpus(name: &str) -> PathBuf {
    Path::new(CORPUS).join(name)
}

fn zipper_tree(archive: &Archive, work: &Path) -> Tree {
    let out = work.join("zipper");
    read_archive(&corpus(&archive.name), Some(&out), ReadOptions::default())
        .unwrap_or_else(|error| panic!("{}: {error:#}", archive.name));
    tree(&out)
}

fn unrar_tree(binary: &Path, archive: &Path, into: &Path) -> Tree {
    fs::create_dir(into).unwrap();
    let run = Command::new(binary)
        .args(["x", "-idq", "-y", "-ver"])
        .arg(format!("-p{PASSWORD}"))
        .arg(archive)
        .arg(into)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "unrar failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&run.stderr)
    );
    with_versions_named_as_7zip_names_them(tree(into))
}

fn with_versions_named_as_7zip_names_them(found: Tree) -> Tree {
    let mut renamed = Tree::new();
    for (name, node) in found {
        let name = match name.rsplit_once(';') {
            Some((base, number)) if number.bytes().all(|b| b.is_ascii_digit()) => {
                format!("[VER]/{number}/{base}")
            }
            _ => name,
        };
        let parts: Vec<&str> = name.split('/').collect();
        if parts[0] == "[VER]" {
            for depth in 1..parts.len() {
                renamed.insert(parts[..depth].join("/"), Node::Folder);
            }
        }
        renamed.insert(name, node);
    }
    renamed
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Result<Tree, String> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y", "-snld"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    if run.status.success() {
        Ok(tree(into))
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        ))
    }
}

fn is_rar4(name: &str) -> bool {
    name.starts_with("rar4-")
}

#[test]
fn every_archive_rar_wrote_extracts_its_source_tree() {
    let work = tempfile::tempdir().unwrap();
    let archives = manifest(umask_ceiling(work.path()));
    let mut extracted = 0;
    for archive in &archives {
        let Some(wanted) = &archive.tree else {
            continue;
        };
        let work = tempfile::tempdir().unwrap();
        let actual = without_unrecorded_fields(zipper_tree(archive, work.path()), wanted);
        assert_eq!(&actual, wanted, "{}", archive.name);
        read_archive(&corpus(&archive.name), None, ReadOptions::default())
            .unwrap_or_else(|error| panic!("{} fails its test: {error:#}", archive.name));
        make_deletable(work.path());
        extracted += 1;
    }
    assert!(extracted >= 40, "only {extracted} archives extracted");
}

#[test]
fn every_archive_with_a_feature_zipper_refuses_is_refused_by_name_and_leaves_nothing() {
    let mut refused = 0;
    for archive in manifest(None) {
        let Some(reason) = &archive.refused else {
            continue;
        };
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        for into in [Some(out.as_path()), None] {
            let error = read_archive(&corpus(&archive.name), into, ReadOptions::default())
                .err()
                .unwrap_or_else(|| panic!("{} was read", archive.name));
            assert!(
                format!("{error:#}").contains(reason.as_str()),
                "{}: {error:#} does not say {reason:?}",
                archive.name
            );
        }
        assert_eq!(
            fs::read_dir(work.path()).unwrap().count(),
            0,
            "{}",
            archive.name
        );
        refused += 1;
    }
    assert!(refused >= 8, "only {refused} archives refused");
}

#[test]
fn every_archive_extracts_the_tree_unrar_and_7zip_extract() {
    let unrar = unrar::unrar();
    let sevenzip = sevenzip::sevenzip();
    let mut against_7zip = Vec::new();
    for archive in manifest(None) {
        let work = tempfile::tempdir().unwrap();
        let ours = archive
            .tree
            .as_ref()
            .map(|_| zipper_tree(&archive, work.path()));
        if let Some(binary) = &unrar {
            let theirs = unrar_tree(binary, &corpus(&archive.name), &work.path().join("theirs"));
            assert!(
                !theirs.is_empty(),
                "unrar extracted nothing from {}",
                archive.name
            );
            if let Some(ours) = &ours {
                assert_eq!(ours, &theirs, "{} against unrar", archive.name);
            }
        }
        if let (Some(binary), Some(ours)) = (&sevenzip, ours) {
            let into = work.path().join("7zip");
            match sevenzip_tree(binary, &corpus(&archive.name), &into) {
                Ok(theirs) => {
                    let (ours, theirs) = if is_rar4(&archive.name) {
                        (without_times(ours), without_times(theirs))
                    } else {
                        (ours, theirs)
                    };
                    assert_eq!(ours, theirs, "{} against 7-Zip", archive.name);
                    against_7zip.push(archive.name.clone());
                }
                Err(output) => assert!(
                    !STORED.contains(&archive.name.as_str()),
                    "7-Zip did not extract {}: {output}",
                    archive.name
                ),
            }
        }
        make_deletable(work.path());
    }
    if sevenzip.is_some() {
        for stored in STORED {
            assert!(
                against_7zip.iter().any(|name| name == stored),
                "7-Zip was not compared on {stored}"
            );
        }
    }
}

#[test]
fn every_volume_the_manifest_names_is_in_the_corpus() {
    for archive in manifest(None) {
        for file in &archive.files {
            assert!(corpus(file).is_file(), "{} names {file}", archive.name);
        }
    }
}

#[test]
fn a_selected_path_late_in_a_solid_stream_extracts_only_its_entries() {
    let work = tempfile::tempdir().unwrap();
    let ceiling = umask_ceiling(work.path());
    for (name, path) in [
        ("rar5-solid-m5.rar", "src/text"),
        ("rar5-solid-small-texts.rar", "src/notes/note-119.txt"),
    ] {
        let archive = manifest(ceiling)
            .into_iter()
            .find(|archive| archive.name == name)
            .unwrap();
        let wanted: Tree = archive
            .tree
            .unwrap()
            .into_iter()
            .filter(|(entry, _)| {
                entry == path
                    || entry.starts_with(&format!("{path}/"))
                    || path.starts_with(&format!("{entry}/"))
            })
            .collect();
        assert!(wanted.len() >= 2, "{name} has no {path}");
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        let paths = [path.to_owned()];
        read_archive(
            &corpus(name),
            Some(&out),
            ReadOptions {
                paths: &paths,
                ..ReadOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(
            without_unrecorded_fields(tree(&out), &wanted),
            wanted,
            "{name}"
        );
        make_deletable(work.path());
    }
}
