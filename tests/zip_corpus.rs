//! Every archive in tests/data/zip-corpus comes from a real ZIP writer, and
//! manifest.tsv records the tree that at least three reference readers agree
//! on. Both native engines must reproduce that tree exactly.
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

type Tree = BTreeMap<String, Option<(u64, u32)>>;

fn expected() -> BTreeMap<String, Tree> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    let manifest = fs::read_to_string(corpus.join("manifest.tsv")).unwrap();
    let mut archives: BTreeMap<String, Tree> = BTreeMap::new();
    for line in manifest.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let value = match fields[2..] {
            ["dir"] => None,
            [size, crc] => Some((size.parse().unwrap(), u32::from_str_radix(crc, 16).unwrap())),
            _ => panic!("bad manifest line {line:?}"),
        };
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
            let value = entry.file_type().is_file().then(|| {
                let data = fs::read(entry.path()).unwrap();
                (data.len() as u64, crc32fast::hash(&data))
            });
            (name.join("/"), value)
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
            assert_eq!(&tree(&destination), expected, "{name} with {engine}");
        }
    }
}
