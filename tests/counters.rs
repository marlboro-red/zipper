//! With the `counters` feature, one extraction with each native engine
//! reports exactly the files, directories and bytes the archive holds, and on
//! Unix exactly its metadata calls.
#![cfg(feature = "counters")]

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
