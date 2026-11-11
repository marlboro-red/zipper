//! Every RAR in `tests/data/rar-corpus` that libarchive's test suite reads
//! extracts to the tree `scripts/record_rar_corpus.py` recorded, and to the
//! tree 7-Zip extracts where it is installed and decodes the archive. Every
//! damaged archive from that suite is refused and leaves nothing behind.
//! Without 7-Zip 26.05 or later the live comparison is skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/sevenzip.rs"]
mod sevenzip;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_rar::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rar-corpus");

#[derive(Debug, PartialEq, Eq)]
enum Node {
    File {
        sha256: String,
        mode: Option<u32>,
        mtime: Option<i64>,
    },
    Link(String),
}

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

fn tree(root: &Path) -> BTreeMap<String, Node> {
    let mut found = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry.unwrap();
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(entry.path()).unwrap();
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(entry.path()).unwrap();
            found.insert(name, Node::Link(target.to_string_lossy().into_owned()));
        } else if metadata.is_file() {
            let mtime = metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs() as i64);
            found.insert(
                name,
                Node::File {
                    sha256: sha256(&read_unlocked(entry.path())),
                    mode: mode_of(&metadata),
                    mtime: Some(mtime),
                },
            );
        }
    }
    found
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

fn manifest() -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
        .unwrap()
}

fn recorded(entries: &serde_json::Value, ceiling: Option<u32>) -> BTreeMap<String, Node> {
    entries
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, node)| node.get("dir").is_none())
        .map(|(name, node)| {
            let value = match node.get("link") {
                Some(target) => Node::Link(target.as_str().unwrap().to_owned()),
                None => Node::File {
                    sha256: node["sha256"].as_str().unwrap().to_owned(),
                    mode: node.get("mode").and_then(|mode| {
                        let mode =
                            u32::from_str_radix(mode.as_str()?.trim_start_matches("0o"), 8).ok()?;
                        Some(mode & ceiling?)
                    }),
                    mtime: node.get("mtime").and_then(serde_json::Value::as_i64),
                },
            };
            (name.clone(), value)
        })
        .collect()
}

fn without_unrecorded_fields(
    mut actual: BTreeMap<String, Node>,
    wanted: &BTreeMap<String, Node>,
) -> BTreeMap<String, Node> {
    for (name, node) in &mut actual {
        if let (
            Node::File { mode, mtime, .. },
            Some(Node::File {
                mode: wanted_mode,
                mtime: wanted_mtime,
                ..
            }),
        ) = (node, wanted.get(name))
        {
            if wanted_mode.is_none() {
                *mode = None;
            }
            if wanted_mtime.is_none() {
                *mtime = None;
            }
        }
    }
    actual
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

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, Node>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn corpus(name: &str) -> PathBuf {
    Path::new(CORPUS).join(name)
}

#[test]
fn every_readable_corpus_archive_extracts_the_recorded_tree_and_the_tree_7zip_extracts() {
    let manifest = manifest();
    let sevenzip = sevenzip::sevenzip();
    let mut compared = 0;
    for (name, entries) in manifest["good"].as_object().unwrap() {
        let work = tempfile::tempdir().unwrap();
        let wanted = recorded(entries, umask_ceiling(work.path()));
        let out = work.path().join("out");
        read_archive(&corpus(name), Some(&out), ReadOptions::default())
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let actual = without_unrecorded_fields(tree(&out), &wanted);
        assert_eq!(actual, wanted, "{name}");
        if let Some(binary) = &sevenzip {
            let theirs = work.path().join("7zip");
            if let Some(theirs) = sevenzip_tree(binary, &corpus(name), &theirs) {
                assert_eq!(tree(&out), theirs, "{name} against 7-Zip");
                compared += 1;
            }
        }
        make_deletable(work.path());
    }
    if sevenzip.is_some() {
        assert!(
            compared >= 5,
            "7-Zip extracted only {compared} corpus archives"
        );
    }
}

#[test]
fn every_damaged_or_unsupported_corpus_archive_is_refused_and_leaves_nothing() {
    let manifest = manifest();
    let refused = manifest["bad"]
        .as_array()
        .unwrap()
        .iter()
        .chain(manifest["unsupported"].as_array().unwrap());
    for name in refused {
        let name = name.as_str().unwrap();
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        assert!(
            read_archive(&corpus(name), Some(&out), ReadOptions::default()).is_err(),
            "{name} was extracted"
        );
        assert!(
            read_archive(&corpus(name), None, ReadOptions::default()).is_err(),
            "{name} was verified"
        );
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 0, "{name}");
    }
}
