//! With the `counters` feature, one extraction with each native engine
//! reports exactly the files, directories and bytes the archive holds, and on
//! Unix exactly its metadata calls, and a SquashFS extraction reads each
//! fragment block once, whatever order its files take turns between them.
#![cfg(feature = "counters")]
#[path = "common/squashfs_writer.rs"]
mod squashfs_writer;

use std::{collections::HashMap, io::Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
use zipper::counters::{Counter, take};

type Reader = fn(&std::path::Path, Option<&std::path::Path>) -> anyhow::Result<Vec<zipper::Entry>>;

#[test]
fn one_extraction_counts_what_the_archive_holds() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("input.zip");
    let mut writer = ZipWriter::new(std::fs::File::create(&archive).unwrap());
    let deflated = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    writer
        .add_directory("top/", deflated.unix_permissions(0o700))
        .unwrap();
    let mut payload = 0;
    for index in 0..5 {
        let data = vec![b'a' + index as u8; 1000 * (index + 1)];
        writer
            .start_file(format!("top/group/file-{index}"), deflated)
            .unwrap();
        writer.write_all(&data).unwrap();
        payload += data.len() as u64;
    }
    writer
        .start_file(
            "top/stored",
            deflated.compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"stored").unwrap();
    payload += 6;
    writer.finish().unwrap();
    let size = std::fs::metadata(&archive).unwrap().len();

    for (engine, read) in [
        ("native", zipper::native_zip::read_archive as Reader),
        ("native-fast", zipper::native_zip::read_archive_fast),
    ] {
        take();
        let destination = work.path().join(engine);
        read(&archive, Some(&destination)).unwrap();
        let counts: HashMap<Counter, u64> = take().into_iter().collect();
        assert_eq!(counts[&Counter::FilesCreated], 6, "{engine}");
        assert_eq!(counts[&Counter::DirectoryCreateCalls], 2, "{engine}");
        assert_eq!(counts[&Counter::SymlinksCreated], 0, "{engine}");
        assert_eq!(counts[&Counter::OutputBytesWritten], payload, "{engine}");
        assert!(counts[&Counter::OutputWrites] >= 6, "{engine}");
        assert!(counts[&Counter::ArchiveBytesRead] >= size / 2, "{engine}");
        assert!(counts[&Counter::DecodeNanos] > 0, "{engine}");
        #[cfg(unix)]
        assert_eq!(counts[&Counter::MetadataCalls], 6 + 1 + 1, "{engine}");
    }
}

#[test]
fn squashfs_reads_each_fragment_block_once_when_files_take_turns() {
    let blocks = 9u8;
    let files = 2000;
    let fragments: Vec<Vec<u8>> = (0..blocks)
        .map(|fragment| {
            (0..squashfs_writer::BLOCK)
                .map(|at| (at as u8).wrapping_mul(7) ^ fragment)
                .collect()
        })
        .collect();
    let entries: Vec<_> = (0..files)
        .map(|index| {
            let offset = (index % squashfs_writer::BLOCK) as u32;
            let fragment = index as u32 % u32::from(blocks);
            squashfs_writer::tail(
                squashfs_writer::ROOT,
                &format!("f{index:05}"),
                fragment,
                offset,
                1,
            )
        })
        .collect();
    let built = squashfs_writer::build_with_fragments(
        &entries,
        squashfs_writer::Options { compress: true },
        &fragments,
    );
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("turns.sqfs");
    std::fs::write(&image, &built.bytes).unwrap();
    for destination in [None, Some(work.path().join("out"))] {
        take();
        let listed = zipper::native_squashfs::read_archive(
            &image,
            destination.as_deref(),
            zipper::native_squashfs::ReadOptions::default(),
        )
        .unwrap();
        let counts: HashMap<Counter, u64> = take().into_iter().collect();
        assert_eq!(counts[&Counter::FragmentBlocksRead], u64::from(blocks));
        let names: Vec<String> = listed.into_iter().map(|entry| entry.name).collect();
        let listing: Vec<String> = (0..files).map(|index| format!("f{index:05}")).collect();
        assert_eq!(names, listing);
    }
    let out = work.path().join("out");
    for index in 0..files {
        let fragment = index % usize::from(blocks);
        let offset = index % squashfs_writer::BLOCK;
        let written = std::fs::read(out.join(format!("f{index:05}"))).unwrap();
        assert_eq!(written, [fragments[fragment][offset]], "f{index:05}");
    }
}

#[test]
fn a_7z_extraction_creates_each_folder_once_for_the_files_inside_it() {
    let archive = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/7z-corpus/7zip-solid-per-2-files-short.7z");
    let work = tempfile::tempdir().unwrap();
    take();
    let listed = zipper::native_7z::read_archive(
        &archive,
        Some(&work.path().join("out")),
        zipper::native_7z::ReadOptions::default(),
    )
    .unwrap();
    let calls = take()
        .into_iter()
        .find(|(counter, _)| *counter == Counter::DirectoryCreateCalls)
        .unwrap()
        .1;
    let folders: std::collections::HashSet<&str> = listed
        .iter()
        .map(|entry| match entry.directory {
            true => entry.name.as_str(),
            false => entry.name.rsplit_once('/').map_or("", |(parent, _)| parent),
        })
        .collect();
    let files = listed.iter().filter(|entry| !entry.directory).count();
    assert!(
        files > folders.len(),
        "{files} files in {} folders",
        folders.len()
    );
    assert!(
        calls > 0 && calls <= folders.len() as u64,
        "{calls} calls for {} folders",
        folders.len()
    );
}
