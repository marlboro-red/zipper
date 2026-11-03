//! `ExtractOptions::flat` puts every file in the destination itself, as
//! `7z e` does. Files that share a name follow `existing` in path order, and
//! an archive with links is refused with nothing published.
use std::{fs, io::Write, path::Path};
use zipper::{Existing, ExtractOptions, extract_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");

fn tree(root: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let kind = if entry.file_type().unwrap().is_dir() {
                "dir".to_owned()
            } else {
                fs::read_to_string(entry.path()).unwrap()
            };
            (entry.file_name().to_string_lossy().into_owned(), kind)
        })
        .collect();
    found.sort();
    found
}

fn flat(existing: Existing) -> ExtractOptions<'static> {
    ExtractOptions {
        flat: true,
        existing,
        ..Default::default()
    }
}

fn zip(path: &Path, members: &[(&str, &str)]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in members {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(data.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn every_format_extracts_its_files_without_folders() {
    for name in ["zip.zip", "tar.tar", "tar.tar.gz", "7z.7z"] {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        extract_archive(
            &Path::new(CORPUS).join(name),
            &destination,
            flat(Existing::Refuse),
        )
        .unwrap();
        let names: Vec<String> = tree(&destination)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["data.txt", "empty", "hello.txt"], "{name}");
    }
}

#[test]
fn files_that_share_a_name_follow_the_existing_mode() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    zip(
        &archive,
        &[("a/x.txt", "from a"), ("b/x.txt", "from b"), ("c/y", "y")],
    );
    let run = |existing| {
        let destination = root.path().join(format!("{existing:?}"));
        extract_archive(&archive, &destination, flat(existing)).map(|()| tree(&destination))
    };
    let error = format!("{:#}", run(Existing::Refuse).unwrap_err());
    assert!(
        error.contains("several archive files are named x.txt"),
        "{error}"
    );
    assert!(!root.path().join("Refuse").exists());
    let pair = |name: &str, data: &str| (name.to_owned(), data.to_owned());
    assert_eq!(
        run(Existing::Overwrite).unwrap(),
        [pair("x.txt", "from b"), pair("y", "y")]
    );
    assert_eq!(
        run(Existing::Skip).unwrap(),
        [pair("x.txt", "from a"), pair("y", "y")]
    );
    assert_eq!(
        run(Existing::RenameNew).unwrap(),
        [
            pair("x.txt", "from a"),
            pair("x_1.txt", "from b"),
            pair("y", "y")
        ]
    );
}

#[test]
fn flat_extraction_merges_into_an_existing_folder() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    zip(&archive, &[("deep/x.txt", "archive")]);
    let destination = root.path().join("out");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("x.txt"), "old").unwrap();
    extract_archive(&archive, &destination, flat(Existing::RenameNew)).unwrap();
    assert_eq!(
        tree(&destination),
        [
            ("x.txt".to_owned(), "old".to_owned()),
            ("x_1.txt".to_owned(), "archive".to_owned())
        ]
    );
}

#[cfg(unix)]
#[test]
fn archives_with_links_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    writer
        .start_file("d/file", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"x").unwrap();
    writer
        .add_symlink("d/link", "file", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();
    let destination = root.path().join("out");
    let error = format!(
        "{:#}",
        extract_archive(&archive, &destination, flat(Existing::Refuse)).unwrap_err()
    );
    assert!(error.contains("refuses archives with links"), "{error}");
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
