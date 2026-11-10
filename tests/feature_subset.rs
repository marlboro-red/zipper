//! A library built with only some format features reads those formats and
//! refuses a recognized format it leaves out with `NotBuilt`, writing nothing.
#![cfg(all(feature = "zip", feature = "7z", not(feature = "tar")))]

use std::path::Path;
use zipper::{Engine, ExtractOptions, NotBuilt, extract_archive};

fn data(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn refused_as(archive: &str, format: &str, engines: &[Engine]) {
    for &engine in engines {
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        let error = extract_archive(
            &data(archive),
            &destination,
            ExtractOptions {
                engine,
                ..ExtractOptions::default()
            },
        )
        .expect_err(archive);
        let not_built = error
            .downcast_ref::<NotBuilt>()
            .unwrap_or_else(|| panic!("{archive}: {error:#}"));
        assert_eq!(not_built.0, format, "{archive}");
        assert_eq!(
            std::fs::read_dir(work.path()).unwrap().count(),
            0,
            "{archive}"
        );
    }
}

#[test]
fn tar_and_gzip_are_refused_as_not_built() {
    let every = [Engine::Auto, Engine::Native, Engine::NativeFast];
    refused_as("tar-corpus/gnutar-gnu-links.tar", "tar", &every);
    refused_as(
        "tar-corpus/gnutar-gnu-gzip.tar.gz",
        "gzip",
        &[Engine::Native, Engine::NativeFast],
    );
}

#[test]
fn zip_and_7z_extract_in_the_subset_build() {
    for archive in [
        "zip-corpus/7zip-deflate.zip",
        "7z-corpus/7zip-bzip2-long.7z",
    ] {
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        extract_archive(&data(archive), &destination, ExtractOptions::default())
            .unwrap_or_else(|error| panic!("{archive}: {error:#}"));
        assert!(destination.join("src").is_dir(), "{archive}");
    }
}
