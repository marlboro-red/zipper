//! A build with the `zstd` feature reads Zstandard ZIP entries, `.tar.zst`
//! and zstd SquashFS images. A build without it refuses each with
//! `NotBuilt("zstd")` and writes nothing.

use std::path::{Path, PathBuf};
use zipper::{Engine, ExtractOptions, NotBuilt, extract_archive};

#[cfg(feature = "zip-deflate")]
const TEXT: &[u8] = b"hello\n";
#[cfg(feature = "zip-deflate")]
const ZSTANDARD_METHOD: u16 = 93;
#[cfg(feature = "zip-deflate")]
const VERSION_NEEDED_6_3: u16 = 63;

#[cfg(any(feature = "tar", feature = "squashfs"))]
fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn extract(archive: &Path, work: &Path) -> anyhow::Result<()> {
    extract_archive(
        archive,
        &work.join("out"),
        ExtractOptions {
            engine: Engine::Native,
            ..ExtractOptions::default()
        },
    )
}

fn assert_refused(archive: &Path) {
    let work = tempfile::tempdir().unwrap();
    let error = extract(archive, work.path()).expect_err("read without zstd");
    let not_built = error
        .downcast_ref::<NotBuilt>()
        .unwrap_or_else(|| panic!("{}: {error:#}", archive.display()));
    assert_eq!(not_built.0, "zstd");
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
}

#[cfg(feature = "zip-deflate")]
// SPEC(RFC 8878 3.1.1): one raw block in a single-segment frame with a one-byte content size.
fn zstd_frame(content: &[u8]) -> Vec<u8> {
    let length = u8::try_from(content.len()).unwrap();
    let block_header = 1 | u32::from(length) << 3;
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x20, length];
    frame.extend_from_slice(&block_header.to_le_bytes()[..3]);
    frame.extend_from_slice(content);
    frame
}

#[cfg(feature = "zip-deflate")]
// SPEC(APPNOTE 4.3.7, 4.3.12 and 4.3.16): one local header, its central header and the end record.
fn zstandard_zip(name: &str, content: &[u8]) -> Vec<u8> {
    let packed = zstd_frame(content);
    let crc = crc32fast::hash(content);
    let fields = |zip: &mut Vec<u8>| {
        zip.extend_from_slice(&VERSION_NEEDED_6_3.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&ZSTANDARD_METHOD.to_le_bytes());
        zip.extend_from_slice(&[0, 0, 0x21, 0]);
        zip.extend_from_slice(&crc.to_le_bytes());
        zip.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(content.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
    };
    let mut zip = b"PK\x03\x04".to_vec();
    fields(&mut zip);
    zip.extend_from_slice(name.as_bytes());
    zip.extend_from_slice(&packed);
    let central = zip.len() as u32;
    zip.extend_from_slice(b"PK\x01\x02");
    zip.extend_from_slice(&(3u16 << 8 | VERSION_NEEDED_6_3).to_le_bytes());
    fields(&mut zip);
    zip.extend_from_slice(&[0; 6]);
    zip.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(name.as_bytes());
    let central_size = zip.len() as u32 - central;
    zip.extend_from_slice(b"PK\x05\x06");
    zip.extend_from_slice(&[0; 4]);
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&central_size.to_le_bytes());
    zip.extend_from_slice(&central.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip
}

#[cfg(feature = "zip-deflate")]
#[test]
fn a_zstandard_zip_entry_reads_only_with_zstd() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("zstandard.zip");
    std::fs::write(&archive, zstandard_zip("hello.txt", TEXT)).unwrap();
    if cfg!(feature = "zstd") {
        let into = tempfile::tempdir().unwrap();
        extract(&archive, into.path()).unwrap();
        assert_eq!(
            std::fs::read(into.path().join("out/hello.txt")).unwrap(),
            TEXT
        );
    } else {
        assert_refused(&archive);
    }
}

#[cfg(feature = "tar")]
#[test]
fn a_zstd_tar_reads_only_with_zstd() {
    let archive = data("tar-corpus/gnutar-pax-zstd.tar.zst");
    if cfg!(feature = "zstd") {
        let work = tempfile::tempdir().unwrap();
        extract(&archive, work.path()).unwrap();
    } else {
        assert_refused(&archive);
    }
}

#[cfg(feature = "squashfs")]
#[test]
fn a_zstd_squashfs_image_reads_only_with_zstd() {
    let archive = data("squashfs-corpus/zstd.sqfs");
    if cfg!(feature = "zstd") {
        let work = tempfile::tempdir().unwrap();
        extract(&archive, work.path()).unwrap();
    } else {
        assert_refused(&archive);
    }
}
