//! Every archive in tests/data/zip-corpus comes from a real ZIP writer, and
//! manifest.tsv records the tree that at least three reference readers agree
//! on. It also records the permission bits and exact modification times that
//! Info-ZIP unzip and bsdtar both restore. Both native engines must reproduce
//! the tree exactly, the modes on Unix, and the times wherever they apply.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::path::{Path, PathBuf};
use zipper::{
    Entry,
    native_zip::{read_archive, read_archive_fast},
};

type Reader = fn(&Path, Option<&Path>) -> anyhow::Result<Vec<Entry>>;

#[test]
fn native_engines_match_the_reference_readers() {
    let corpus: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    let archives = corpus::expected(&corpus);
    assert!(
        archives.len() >= 10,
        "corpus has {} archives",
        archives.len()
    );
    for (name, wanted) in &archives {
        for (engine, read) in [
            ("native", read_archive as Reader),
            ("native-fast", read_archive_fast as Reader),
        ] {
            let work = tempfile::tempdir().unwrap();
            let destination = work.path().join("out");
            let result = read(&corpus.join(format!("{name}.zip")), Some(&destination));
            result.unwrap_or_else(|error| panic!("{name} with {engine}: {error:#}"));
            corpus::assert_tree_matches_manifest(
                work.path(),
                wanted,
                &format!("{name} with {engine}"),
            );
        }
    }
}
