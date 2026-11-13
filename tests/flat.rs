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

#[cfg(unix)]
fn everything_under(root: &Path) -> Vec<(String, String)> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let kind = match entry.file_type().is_dir() {
                true => "dir".to_owned(),
                false => fs::read_to_string(entry.path()).unwrap(),
            };
            let relative = entry.path().strip_prefix(root).unwrap();
            (relative.to_string_lossy().into_owned(), kind)
        })
        .collect()
}

#[cfg(unix)]
fn unlock(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if entry.file_type().is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[cfg(unix)]
const NESTED_FOLDERS: [&str; 5] = ["top/", "top/mid/", "top/mid/deep/", "top/empty/", "side/"];

#[cfg(unix)]
const NESTED_FILES: [(&str, &str); 4] = [
    ("top/mid/deep/w.txt", "w"),
    ("top/mid/x.txt", "x"),
    ("top/y.txt", "y"),
    ("side/z.txt", "z"),
];

#[cfg(unix)]
fn zip_with_read_only(path: &Path, read_only: &str) {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for folder in NESTED_FOLDERS {
        let mode = if folder == read_only { 0o555 } else { 0o755 };
        writer
            .add_directory(folder, SimpleFileOptions::default().unix_permissions(mode))
            .unwrap();
    }
    for (name, data) in NESTED_FILES {
        writer
            .start_file(name, SimpleFileOptions::default().unix_permissions(0o644))
            .unwrap();
        writer.write_all(data.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

#[cfg(unix)]
#[test]
fn a_read_only_folder_in_any_place_leaves_the_output_and_its_parent_as_they_were() {
    let archives = tempfile::tempdir().unwrap();
    let mut failures = 0;
    for read_only in NESTED_FOLDERS {
        let archive = archives
            .path()
            .join(format!("{}.zip", read_only.replace('/', "_")));
        zip_with_read_only(&archive, read_only);
        for existing in [Existing::Refuse, Existing::Overwrite] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("out");
            let mut expected = vec![("out".to_owned(), "dir".to_owned())];
            if existing == Existing::Overwrite {
                fs::create_dir(&destination).unwrap();
                fs::write(destination.join("kept.txt"), "kept").unwrap();
                expected.push(("out/kept.txt".to_owned(), "kept".to_owned()));
            }
            let before = everything_under(root.path());
            let result = extract_archive(&archive, &destination, flat(existing));
            let after = everything_under(root.path());
            unlock(root.path());
            let case = format!("{read_only} read-only, {existing:?}: {result:?}");
            if result.is_err() {
                failures += 1;
                assert_eq!(after, before, "{case}");
                continue;
            }
            expected.extend(NESTED_FILES.iter().map(|(path, data)| {
                let name = path.rsplit('/').next().unwrap();
                (format!("out/{name}"), (*data).to_owned())
            }));
            expected.sort();
            assert_eq!(after, expected, "{case}");
        }
    }
    assert!(failures > 0, "no read-only folder made the extraction fail");
}

fn contents(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = tree(root).into_iter().map(|(_, data)| data).collect();
    found.sort();
    found
}

#[test]
fn names_that_differ_only_in_case_or_normalization_are_one_name_where_the_filesystem_ignores_it() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Probe"), "").unwrap();
    if !root.path().join("probe").exists() {
        eprintln!("skipped: this filesystem tells names apart by case");
        return;
    }
    let case = root.path().join("case.zip");
    zip(&case, &[("a/X.txt", "upper"), ("b/x.txt", "lower")]);
    let normalization = root.path().join("normalization.zip");
    zip(
        &normalization,
        &[
            ("a/\u{e9}.txt", "composed"),
            ("b/e\u{301}.txt", "decomposed"),
        ],
    );
    let numbered = root.path().join("numbered.zip");
    zip(
        &numbered,
        &[("a/x.txt", "a"), ("b/x.txt", "b"), ("c/X_1.txt", "c")],
    );
    let run = |archive: &Path, existing| {
        let destination = root.path().join(format!(
            "{}-{existing:?}",
            archive.file_stem().unwrap().to_string_lossy()
        ));
        extract_archive(archive, &destination, flat(existing)).map(|()| contents(&destination))
    };
    for archive in [&case, &normalization] {
        let error = format!("{:#}", run(archive, Existing::Refuse).unwrap_err());
        assert!(error.contains("several archive files are named"), "{error}");
        let parent: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(parent.len(), 4, "{parent:?}");
    }
    assert_eq!(run(&case, Existing::Skip).unwrap(), ["upper"]);
    assert_eq!(run(&case, Existing::Overwrite).unwrap(), ["lower"]);
    assert_eq!(run(&case, Existing::RenameNew).unwrap(), ["lower", "upper"]);
    assert_eq!(
        run(&normalization, Existing::RenameExisting).unwrap(),
        ["composed", "decomposed"]
    );
    assert_eq!(
        run(&numbered, Existing::RenameNew).unwrap(),
        ["a", "b", "c"]
    );
}
