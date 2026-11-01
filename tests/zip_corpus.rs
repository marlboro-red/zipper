//! Every archive in tests/data/zip-corpus comes from a real ZIP writer, and
//! manifest.tsv records the tree that at least three reference readers agree
//! on, with the permission bits Info-ZIP unzip and bsdtar both restore. Both
//! native engines must reproduce that tree exactly, and on Unix those modes.
mod common;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use zipper::{
    Entry,
    native_zip::{read_archive, read_archive_fast},
};

type Reader = fn(&Path, Option<&Path>) -> anyhow::Result<Vec<Entry>>;

/// Path to file size and CRC-32 (`None` for a directory) and expected mode.
type Tree = BTreeMap<String, (Option<(u64, u32)>, Option<u32>)>;

fn expected() -> BTreeMap<String, Tree> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    let manifest = fs::read_to_string(corpus.join("manifest.tsv")).unwrap();
    let mut archives: BTreeMap<String, Tree> = BTreeMap::new();
    for line in manifest.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [_, _, size, crc, mode] = fields[..] else {
            panic!("bad manifest line {line:?}");
        };
        let content =
            (size != "dir").then(|| (size.parse().unwrap(), u32::from_str_radix(crc, 16).unwrap()));
        let mode = (mode != "-").then(|| u32::from_str_radix(mode, 8).unwrap());
        let value = (content, mode);
        archives
            .entry(fields[0].to_string())
            .or_default()
            .insert(fields[1].to_string(), value);
    }
    archives
}

fn tree(root: &Path) -> Tree {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.path().strip_prefix(root).unwrap();
            let name: Vec<_> = name.iter().map(|part| part.to_str().unwrap()).collect();
            let content = entry.file_type().is_file().then(|| {
                let data = fs::read(entry.path()).unwrap();
                (data.len() as u64, crc32fast::hash(&data))
            });
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(entry.metadata().unwrap().permissions().mode() & 0o777)
            };
            #[cfg(not(unix))]
            let mode = None;
            (name.join("/"), (content, mode))
        })
        .collect()
}

#[test]
fn native_engines_match_the_reference_readers() {
    let archives = expected();
    assert!(
        archives.len() >= 10,
        "corpus has {} archives",
        archives.len()
    );
    let corpus: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    for (name, expected) in &archives {
        for (engine, read) in [
            ("native", read_archive as Reader),
            ("native-fast", read_archive_fast as Reader),
        ] {
            let work = tempfile::tempdir().unwrap();
            let destination = work.path().join("out");
            read(&corpus.join(format!("{name}.zip")), Some(&destination))
                .unwrap_or_else(|error| panic!("{name} with {engine}: {error:#}"));
            let mut actual = tree(&destination);
            // Modes were recorded under umask 022; a stricter umask removes bits.
            #[cfg(unix)]
            let default_mode = {
                // The extracted root gets the mode of any new directory.
                use std::os::unix::fs::PermissionsExt;
                let probe = work.path().join("probe");
                fs::create_dir(&probe).unwrap();
                let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(
                    mode(&destination),
                    mode(&probe),
                    "{name} root with {engine}"
                );
                mode(&probe)
            };
            #[cfg(not(unix))]
            let default_mode = 0o777;
            common::unlock(work.path());
            // Compare modes only on Unix, where the reference readers agreed.
            let mut expected = expected.clone();
            for (path, (_, mode)) in actual.iter_mut() {
                let unknown = expected
                    .get(path)
                    .is_none_or(|(_, wanted)| wanted.is_none());
                if cfg!(not(unix)) || unknown {
                    *mode = None;
                }
            }
            for (_, mode) in expected.values_mut() {
                *mode = mode.filter(|_| cfg!(unix)).map(|mode| mode & default_mode);
            }
            assert_eq!(actual, expected, "{name} with {engine}");
        }
    }
}
