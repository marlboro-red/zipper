//! Every archive in tests/data/7z-corpus comes from 7-Zip or libarchive, and
//! manifest.tsv records the tree that 7-Zip and bsdtar both extract, with the
//! modes and times bsdtar restores from the source. The native 7z reader must
//! reproduce the tree exactly, the modes on Unix, and the times wherever they
//! apply. Each archive in refused.tsv must fail with the error it names.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::path::Path;
use zipper::native_7z::{ReadOptions, read_archive};

fn corpus() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/7z-corpus")
}

#[test]
fn native_reader_matches_the_reference_readers() {
    let archives = corpus::expected(&corpus());
    assert!(
        archives.len() >= 40,
        "corpus has {} archives",
        archives.len()
    );
    for (name, wanted) in &archives {
        let archive = corpus().join(format!("{name}.7z"));
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        let result = read_archive(&archive, Some(&destination), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
        corpus::assert_tree_matches_manifest(work.path(), wanted, name);
        read_archive(&archive, None, ReadOptions::default())
            .unwrap_or_else(|error| panic!("{name} verification: {error:#}"));
    }
}

#[test]
fn unsupported_methods_are_refused_by_name_and_routed_to_7zip() {
    let refused = std::fs::read_to_string(corpus().join("refused.tsv")).unwrap();
    assert!(refused.lines().count() >= 3);
    for line in refused.lines() {
        let (name, error) = line.split_once('\t').unwrap();
        let archive = corpus().join(name);
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        let failure = read_archive(&archive, Some(&destination), ReadOptions::default())
            .expect_err(name)
            .to_string();
        assert!(failure.contains(error), "{name}: {failure}");
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0, "{name}");
        assert!(matches!(
            zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
            zipper::Reader::Backend { format: Some(_) }
        ));
    }
}

#[test]
fn bzip2_and_ppmd_archives_extract_the_same_files_as_lzma2() {
    let files = |name: &str| {
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        read_archive(&corpus().join(name), Some(&out), ReadOptions::default())
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let mut found: Vec<(String, Option<Vec<u8>>)> = walkdir::WalkDir::new(&out)
            .min_depth(1)
            .into_iter()
            .map(|entry| entry.unwrap())
            .map(|entry| {
                let name = entry
                    .path()
                    .strip_prefix(&out)
                    .unwrap()
                    .display()
                    .to_string();
                let data = entry
                    .file_type()
                    .is_file()
                    .then(|| std::fs::read(entry.path()).unwrap());
                (name, data)
            })
            .collect();
        found.sort();
        found
    };
    assert_eq!(files("refused-bzip2.7z"), files("7zip-lzma2-short.7z"));
    assert_eq!(files("refused-ppmd.7z"), files("7zip-lzma2-short.7z"));
}
