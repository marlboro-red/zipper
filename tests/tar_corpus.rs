//! Every archive in tests/data/tar-corpus comes from a real tar writer, and
//! manifest.tsv records the tree that at least three reference readers agree
//! on, with the modes and exact times that two of GNU tar, bsdtar and Python
//! restore. The native tar reader must reproduce the tree exactly, the modes
//! on Unix, and the times wherever they apply.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::path::Path;
use zipper::native_tar::{ReadOptions, read_archive};

#[test]
fn native_reader_matches_the_reference_readers() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tar-corpus");
    let archives = corpus::expected(&corpus);
    assert!(
        archives.len() >= 40,
        "corpus has {} archives",
        archives.len()
    );
    let files: Vec<_> = std::fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    for (name, wanted) in &archives {
        let archive = files
            .iter()
            .find(|path| {
                let file = path.file_name().unwrap().to_str().unwrap();
                file.strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
            })
            .unwrap_or_else(|| panic!("{name} has no archive file"));
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        let result = read_archive(archive, Some(&destination), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
        corpus::assert_tree_matches_manifest(work.path(), wanted, name);
    }
}
