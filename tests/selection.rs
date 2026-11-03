//! Extraction of selected paths from a ZIP archive.
use std::{fs, io::Write, path::Path};
use zip::write::SimpleFileOptions;
use zipper::{Existing, native_zip};

fn write_zip(path: &Path) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    writer.add_directory("docs/", options).unwrap();
    for (name, data) in [
        ("docs/readme", "readme"),
        ("docs/guide/intro", "intro"),
        ("docsextra/file", "extra"),
        ("implied/deep/file", "implied"),
        ("top", "top"),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(data.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn extract(
    archive: &Path,
    destination: &Path,
    paths: &[&str],
    existing: Existing,
) -> anyhow::Result<Vec<String>> {
    let paths: Vec<String> = paths.iter().map(|path| path.to_string()).collect();
    let entries = native_zip::read_archive_with_options(
        archive,
        Some(destination),
        native_zip::ReadOptions {
            paths: &paths,
            existing,
            ..Default::default()
        },
    )?;
    Ok(entries.into_iter().map(|entry| entry.name).collect())
}

fn files(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let relative = entry.path().strip_prefix(root).unwrap();
            relative.to_string_lossy().replace('\\', "/")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn folders_and_files_select_their_subtrees_only() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive);
    for (case, paths, expected) in [
        (
            "everything",
            &[][..],
            &[
                "docs/guide/intro",
                "docs/readme",
                "docsextra/file",
                "implied/deep/file",
                "top",
            ][..],
        ),
        ("folder", &["docs"], &["docs/guide/intro", "docs/readme"]),
        (
            "trailing slash",
            &["docs/"],
            &["docs/guide/intro", "docs/readme"],
        ),
        ("file", &["docs/readme"], &["docs/readme"]),
        ("implied folder", &["implied"], &["implied/deep/file"]),
        (
            "two paths",
            &["top", "docs/guide"],
            &["docs/guide/intro", "top"],
        ),
    ] {
        let destination = root.path().join(case);
        let entries = extract(&archive, &destination, paths, Existing::Refuse).unwrap();
        assert_eq!(files(&destination), expected, "{case}");
        assert!(
            entries
                .iter()
                .all(|name| expected.iter().any(|file| file.starts_with(name.as_str()))),
            "{case}: {entries:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(root.path().join("file/docs/readme")).unwrap(),
        "readme"
    );
}

#[test]
fn paths_that_match_nothing_or_escape_are_refused_before_output() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive);
    for paths in [
        &["doc"][..],
        &["missing"],
        &["docs", "missing"],
        &["../top"],
        &["/top"],
        &[""],
    ] {
        let destination = root.path().join("out");
        assert!(
            extract(&archive, &destination, paths, Existing::Refuse).is_err(),
            "{paths:?}"
        );
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            1,
            "{paths:?} left output"
        );
    }
}

#[test]
fn selection_merges_into_an_existing_folder() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive);
    let destination = root.path().join("out");
    fs::create_dir_all(destination.join("docs")).unwrap();
    fs::write(destination.join("docs/readme"), "old").unwrap();
    fs::write(destination.join("top"), "old top").unwrap();
    extract(&archive, &destination, &["docs"], Existing::Overwrite).unwrap();
    assert_eq!(
        fs::read_to_string(destination.join("docs/readme")).unwrap(),
        "readme"
    );
    assert_eq!(
        fs::read_to_string(destination.join("top")).unwrap(),
        "old top"
    );
    assert_eq!(
        files(&destination),
        ["docs/guide/intro", "docs/readme", "top"]
    );
}

#[test]
fn cli_selects_from_zip_and_tar() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive);
    let binary = env!("CARGO_BIN_EXE_zipper");
    let destination = root.path().join("cli");
    let status = std::process::Command::new(binary)
        .args(["extract", "--path", "docs/guide", "-o"])
        .arg(&destination)
        .arg(&archive)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(files(&destination), ["docs/guide/intro"]);

    let tar = root.path().join("a.tar");
    let mut bytes = vec![0u8; 512];
    bytes[..1].copy_from_slice(b"f");
    bytes[100..108].copy_from_slice(b"0000644\0");
    bytes[124..136].copy_from_slice(b"00000000000\0");
    bytes[136..148].copy_from_slice(b"00000000000\0");
    bytes[156] = b'0';
    bytes[257..263].copy_from_slice(b"ustar\0");
    bytes[263..265].copy_from_slice(b"00");
    bytes[148..156].fill(b' ');
    let sum: u32 = bytes.iter().map(|&byte| u32::from(byte)).sum();
    bytes[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    bytes.extend_from_slice(&[0; 1024]);
    fs::write(&tar, bytes).unwrap();
    let status = std::process::Command::new(binary)
        .args(["extract", "--path", "f", "-o"])
        .arg(root.path().join("tar-out"))
        .arg(&tar)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(files(&root.path().join("tar-out")), ["f"]);
    let output = std::process::Command::new(binary)
        .args(["extract", "--path", "missing", "-o"])
        .arg(root.path().join("tar-missing"))
        .arg(&tar)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("selected path not found"));
    assert!(!root.path().join("tar-missing").exists());
}

fn sevenzip() -> Option<std::path::PathBuf> {
    std::env::var_os("ZIPPER_7Z")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            ["7zz", "7z"]
                .into_iter()
                .find(|binary| {
                    std::process::Command::new(binary)
                        .arg("i")
                        .output()
                        .is_ok_and(|output| output.status.success())
                })
                .map(std::path::PathBuf::from)
        })
}

#[test]
fn include_and_exclude_patterns_select_the_same_files_in_every_format() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/parity/archives");
    let list = |values: &[&str]| {
        values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
    };
    type Case<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str], &'a str);
    let cases: [Case; 5] = [
        (
            &["*.txt"],
            &[],
            &["folder/data.txt", "hello.txt"],
            "-ir!*.txt",
        ),
        (&[], &["*.txt"], &["folder/empty"], "-xr!*.txt"),
        (&[], &["folder"], &["hello.txt"], "-xr!folder"),
        (&["folder/*"], &["empty"], &["folder/data.txt"], ""),
        (&["**/data.txt"], &[], &["folder/data.txt"], ""),
    ];
    for archive in ["zip.zip", "tar.tar", "tar.tar.gz", "7z.7z"] {
        for (include, exclude, expected, switch) in cases {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("out");
            let (include, exclude) = (list(include), list(exclude));
            zipper::extract_archive(
                &corpus.join(archive),
                &destination,
                zipper::ExtractOptions {
                    include: &include,
                    exclude: &exclude,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("{archive} {include:?} {exclude:?}: {error:#}"));
            assert_eq!(
                files(&destination),
                expected,
                "{archive} {include:?} {exclude:?}"
            );
            if let (Some(binary), false) =
                (sevenzip(), switch.is_empty() || archive.ends_with(".gz"))
            {
                let by_7zip = root.path().join("7zip");
                let mut output = std::ffi::OsString::from("-o");
                output.push(&by_7zip);
                let status = std::process::Command::new(binary)
                    .args(["x", "-y", switch])
                    .arg(output)
                    .arg(corpus.join(archive))
                    .stdout(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success(), "7-Zip {switch} on {archive}");
                assert_eq!(files(&by_7zip), expected, "7-Zip {switch} on {archive}");
            }
        }
    }
    let partly = list(&["*.txt", "*.bin"]);
    let error = zipper::extract_archive(
        &corpus.join("tar.tar"),
        &tempfile::tempdir().unwrap().path().join("out"),
        zipper::ExtractOptions {
            include: &partly,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("--include *.bin"));
    let nothing = list(&["*.bin"]);
    let error = zipper::extract_archive(
        &corpus.join("zip.zip"),
        &tempfile::tempdir().unwrap().path().join("out"),
        zipper::ExtractOptions {
            include: &nothing,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("no archive entry matches"));
}

#[test]
fn unselected_entries_still_get_their_local_headers_checked() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for name in ["unsel", "keep"] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(name.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    let local_name = bytes
        .windows(5)
        .position(|window| window == b"unsel")
        .unwrap();
    bytes[local_name + 4] = b'x';
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    let error = extract(&archive, &destination, &["keep"], Existing::Refuse).unwrap_err();
    assert!(error.to_string().contains("local header"), "{error}");
    assert!(!destination.exists());
}
