//! Mark of the Web on Windows. Every file extracted from an archive that
//! carries a `Zone.Identifier` stream gets the same stream, with its
//! modification time unchanged. Folders get none, and an unmarked archive
//! marks nothing.
#![cfg(windows)]
use std::{
    fs,
    path::{Path, PathBuf},
};
use zipper::{Existing, ExtractOptions, extract_archive};

const ARCHIVES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");
const ZONE: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.invalid/a.zip\r\n";

fn stream(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(":Zone.Identifier");
    name.into()
}

fn extract(archive: &Path, destination: &Path, existing: Existing) {
    extract_archive(
        archive,
        destination,
        ExtractOptions {
            existing,
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn extracted_files_carry_the_archives_mark_of_the_web() {
    for name in ["zip.zip", "tar.tar", "tar.tar.gz", "7z.7z"] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(name);
        fs::copy(Path::new(ARCHIVES).join(name), &archive).unwrap();
        let plain = root.path().join("plain");
        extract(&archive, &plain, Existing::Refuse);
        fs::write(stream(&archive), ZONE).unwrap();
        let marked = root.path().join("marked");
        extract(&archive, &marked, Existing::Refuse);
        let mut files = 0;
        for entry in walkdir::WalkDir::new(&marked).min_depth(1) {
            let entry = entry.unwrap();
            let unmarked = plain.join(entry.path().strip_prefix(&marked).unwrap());
            assert!(
                fs::read(stream(&unmarked)).is_err(),
                "{name}: {unmarked:?} is marked"
            );
            if entry.file_type().is_dir() {
                assert!(
                    fs::read(stream(entry.path())).is_err(),
                    "{name}: folder marked"
                );
                continue;
            }
            assert_eq!(fs::read(stream(entry.path())).unwrap(), ZONE, "{name}");
            assert_eq!(
                entry.metadata().unwrap().modified().unwrap(),
                fs::metadata(&unmarked).unwrap().modified().unwrap(),
                "{name}: {:?}",
                entry.path()
            );
            files += 1;
        }
        assert_eq!(files, 3, "{name}");
        extract(&archive, &plain, Existing::Overwrite);
        assert_eq!(
            fs::read(stream(&plain.join("hello.txt"))).unwrap(),
            ZONE,
            "{name}"
        );
    }
}
