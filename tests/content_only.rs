//! `Metadata::ContentOnly` on every archive of the ZIP, tar and 7z corpora.
//! Contents and links must match the manifest. No file or folder may keep a
//! time from the archive, a file's mode is 0o666 or 0o777 less the umask by
//! the archive's execute bit, and folders get the default mode. On Windows no
//! read-only, hidden or system attribute and no `Zone.Identifier` stream may
//! appear, even when the archive carries a mark.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use zipper::{ExtractOptions, Metadata, extract_archive};

fn data(folder: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(folder)
}

fn earlier_volume(rest: &str) -> bool {
    rest.strip_prefix(".z")
        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn archive_files(corpus: &Path, name: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let file = path.file_name().unwrap().to_str().unwrap();
            file.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('.') && rest != ".tsv" && rest != ".json")
        })
        .collect();
    files.sort_by_key(|path| {
        let file = path.file_name().unwrap().to_str().unwrap();
        !earlier_volume(&file[name.len()..])
    });
    assert!(!files.is_empty(), "{name} has no archive file");
    files
}

#[cfg(unix)]
fn check_mode(path: &Path, wanted: Option<u32>, directory: bool, default: u32, label: &str) {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    if directory {
        assert_eq!(mode, default, "{label}: folder {path:?}");
    } else if let Some(wanted) = wanted {
        let plain = if wanted & 0o111 == 0 { 0o666 } else { 0o777 };
        assert_eq!(
            mode,
            plain & default,
            "{label}: file {path:?} from mode {wanted:o}"
        );
    } else {
        assert!(
            [0o666 & default, 0o777 & default].contains(&mode),
            "{label}: file {path:?} has {mode:o}"
        );
    }
}

#[cfg(windows)]
fn check_windows(path: &Path, label: &str) {
    use std::os::windows::fs::MetadataExt;
    const READ_ONLY_HIDDEN_SYSTEM: u32 = 0x1 | 0x2 | 0x4;
    let attributes = fs::symlink_metadata(path).unwrap().file_attributes();
    assert_eq!(attributes & READ_ONLY_HIDDEN_SYSTEM, 0, "{label}: {path:?}");
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    assert!(fs::read(stream).is_err(), "{label}: {path:?} is marked");
}

#[test]
fn content_only_extraction_writes_contents_and_the_execute_bit_alone() {
    let started = SystemTime::now() - Duration::from_secs(5);
    let mut checked = 0;
    for folder in ["zip-corpus", "tar-corpus", "7z-corpus"] {
        let corpus = data(folder);
        for (name, tree) in corpus::expected(&corpus) {
            let work = tempfile::tempdir().unwrap();
            let mut archive = PathBuf::new();
            for file in archive_files(&corpus, &name) {
                archive = work.path().join(file.file_name().unwrap());
                fs::copy(&file, &archive).unwrap();
            }
            #[cfg(windows)]
            {
                let mut stream = archive.as_os_str().to_owned();
                stream.push(":Zone.Identifier");
                fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
            }
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("out");
            extract_archive(
                &archive,
                &destination,
                ExtractOptions {
                    metadata: Metadata::ContentOnly,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
            #[cfg(unix)]
            let default = {
                use std::os::unix::fs::PermissionsExt;
                let probe = root.path().join("umask-probe");
                fs::create_dir(&probe).unwrap();
                fs::metadata(&probe).unwrap().permissions().mode() & 0o777
            };
            for (path, expected) in &tree {
                if expected.link_target.is_some() {
                    continue;
                }
                let found = destination.join(path);
                let metadata =
                    fs::metadata(&found).unwrap_or_else(|error| panic!("{name} {path}: {error}"));
                assert!(
                    metadata.modified().unwrap() >= started,
                    "{name}: {path} kept its time"
                );
                #[cfg(unix)]
                check_mode(
                    &found,
                    expected.mode(),
                    expected.is_directory(),
                    default,
                    &name,
                );
                #[cfg(windows)]
                check_windows(&found, &name);
            }
            let contents: corpus::Tree = tree
                .iter()
                .map(|(path, expected)| (path.clone(), expected.without_metadata()))
                .collect();
            corpus::assert_tree_matches_manifest(root.path(), &contents, &name);
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}
