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
        for threads in [Some(1), Some(4), None] {
            let options = ReadOptions {
                threads,
                ..ReadOptions::default()
            };
            let case = format!("{name} at {threads:?} threads");
            let work = tempfile::tempdir().unwrap();
            let destination = work.path().join("out");
            let result = read_archive(&archive, Some(&destination), options);
            result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
            corpus::assert_tree_matches_manifest(work.path(), wanted, &case);
            read_archive(&archive, None, options)
                .unwrap_or_else(|error| panic!("{case} verification: {error:#}"));
        }
    }
}

#[test]
fn refused_archives_fail_by_name_and_only_encrypted_ones_stay_in_process() {
    let refused = std::fs::read_to_string(corpus().join("refused.tsv")).unwrap();
    assert!(refused.lines().count() >= 1);
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
        let reader = zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap();
        if error == "encrypted" {
            assert!(matches!(reader, zipper::Reader::SevenZip), "{name}");
            read_archive(
                &archive,
                Some(&destination),
                ReadOptions {
                    password: Some(b"zipper"),
                    ..ReadOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        } else {
            assert!(
                matches!(reader, zipper::Reader::Backend { format: Some(_) }),
                "{name}"
            );
        }
    }
}

#[test]
fn an_lzma_setting_liblzma_refuses_reads_natively_as_7zip_reads_it() {
    let archives = corpus::expected(&corpus());
    let archive = corpus().join("7zip-lzma-lc8-short.7z");
    assert!(matches!(
        zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
        zipper::Reader::SevenZip
    ));
    let work = tempfile::tempdir().unwrap();
    read_archive(
        &archive,
        Some(&work.path().join("out")),
        ReadOptions::default(),
    )
    .unwrap();
    corpus::assert_tree_matches_manifest(work.path(), &archives["7zip-lzma-short"], "lzma-lc8");
}
