//! Every archive in tests/data/zip-corpus comes from a real ZIP writer, and
//! manifest.tsv records the tree that at least three reference readers agree
//! on. It also records the permission bits and exact modification times that
//! Info-ZIP unzip and bsdtar both restore. The ZIP reader must reproduce
//! the tree exactly, the modes on Unix, and the times wherever they apply.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::path::{Path, PathBuf};
use zipper::native_zip::read_archive;

#[test]
fn the_zip_reader_matches_the_reference_readers() {
    let corpus: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    let archives = corpus::expected(&corpus);
    assert!(
        archives.len() >= 10,
        "corpus has {} archives",
        archives.len()
    );
    for (name, wanted) in &archives {
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        let result = read_archive(&corpus.join(format!("{name}.zip")), Some(&destination));
        result.unwrap_or_else(|error| panic!("{name} with native: {error:#}"));
        corpus::assert_tree_matches_manifest(work.path(), wanted, &format!("{name} with native"));
    }
}
