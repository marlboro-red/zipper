use std::{fs, io::Write, path::Path, process::Command};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const RESERVED_DEFLATE_BLOCK: u8 = 0xff;

fn archive(
    path: &Path,
    entries: &[(&str, &[u8])],
    method: CompressionMethod,
) -> anyhow::Result<()> {
    let mut writer = ZipWriter::new(fs::File::create(path)?);
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true);
    for (name, data) in entries {
        if name.ends_with('/') {
            writer.add_directory(*name, options)?;
        } else {
            writer.start_file(*name, options)?;
            writer.write_all(data)?;
        }
    }
    writer.finish()?;
    Ok(())
}

#[test]
fn stored_deflated_zip64_and_cli_recover_exact_bytes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let large: Vec<_> = (0..2 * 1024 * 1024 + 19).map(|n| (n % 251) as u8).collect();
    for (label, method) in [
        ("stored", CompressionMethod::Stored),
        ("deflate", CompressionMethod::Deflated),
    ] {
        let input = temp.path().join(format!("{label}.zip"));
        archive(
            &input,
            &[
                ("tree/a", &large),
                ("tree/b", b"tail"),
                ("tree/empty", b""),
                ("emptydir/", b""),
            ],
            method,
        )?;
        let entries = zipper::native_zip::read_archive(&input, None)?;
        assert_eq!(entries.len(), 4);
        let out = temp.path().join(label);
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["--threads", "3", "extract"])
            .arg(&input)
            .arg("-o")
            .arg(&out)
            .env("ZIPPER_7Z", "nonexistent-backend")
            .output()?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(out.join("tree/a"))?, large);
        assert_eq!(fs::read(out.join("tree/b"))?, b"tail");
        assert_eq!(fs::read(out.join("tree/empty"))?, b"");
        assert!(out.join("emptydir").is_dir());
        assert!(zipper::native_zip::read_archive(&input, Some(&out)).is_err());
        assert_eq!(fs::read(out.join("tree/a"))?, large);
    }
    Ok(())
}

#[test]
fn central_comments_cross_preflight_buffer_boundaries() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("comments.zip");
    archive(
        &path,
        &[("first", b"first bytes"), ("second", b"second bytes")],
        CompressionMethod::Stored,
    )?;
    let original = fs::read(&path)?;
    let records: Vec<_> = original
        .windows(4)
        .enumerate()
        .filter_map(|(at, bytes)| (bytes == b"PK\x01\x02").then_some(at))
        .collect();
    assert_eq!(records.len(), 2);
    for length in [65_400usize, 65_500, 65_535] {
        let mut bytes = original.clone();
        bytes[records[0] + 32..records[0] + 34].copy_from_slice(&(length as u16).to_le_bytes());
        bytes.splice(records[1]..records[1], std::iter::repeat_n(b'x', length));
        let end = bytes.len() - 22;
        let directory_size = u32::from_le_bytes(bytes[end + 12..end + 16].try_into()?);
        bytes[end + 12..end + 16].copy_from_slice(&(directory_size + length as u32).to_le_bytes());
        fs::write(&path, &bytes)?;
        let entries = zipper::native_zip::list(&path)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "second");
        let out = temp.path().join(format!("out-{length}"));
        zipper::native_zip::read_archive(&path, Some(&out))?;
        assert_eq!(fs::read(out.join("first"))?, b"first bytes");
        assert_eq!(fs::read(out.join("second"))?, b"second bytes");
        bytes[records[1] + length] ^= 1;
        fs::write(&path, bytes)?;
        assert!(zipper::native_zip::list(&path).is_err());
    }
    Ok(())
}

#[test]
fn many_tiny_entries_recover_and_failure_never_publishes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("many.zip");
    for threads in [0, 65] {
        let error = zipper::native_zip::read_archive_with_options(
            &path,
            None,
            zipper::native_zip::ReadOptions {
                threads: Some(threads),
                max_output_bytes: None,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("ZIP threads must be 1..64"));
    }
    let payloads: Vec<Vec<u8>> = (0..1030).map(|i| vec![(i % 251) as u8; i % 513]).collect();
    let mut writer = ZipWriter::new(fs::File::create(&path)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (i, data) in payloads.iter().enumerate() {
        writer.start_file(format!("group-{}/file-{i}", i % 7), options)?;
        writer.write_all(data)?;
    }
    writer.finish()?;
    for threads in [1, 10] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()?;
        let output = temp.path().join(format!("out-{threads}"));
        let entries = pool.install(|| zipper::native_zip::read_archive(&path, Some(&output)))?;
        assert_eq!(entries.len(), payloads.len());
        for (i, data) in payloads.iter().enumerate() {
            assert_eq!(
                fs::read(output.join(format!("group-{}/file-{i}", i % 7)))?,
                *data
            );
        }
    }
    let mut damaged = fs::read(&path)?;
    let central = damaged
        .windows(4)
        .enumerate()
        .filter_map(|(i, bytes)| (bytes == b"PK\x01\x02").then_some(i))
        .nth(700)
        .unwrap();
    damaged[central + 16] ^= 1;
    fs::write(&path, damaged)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(10).build()?;
    let output = temp.path().join("failed");
    assert!(
        pool.install(|| zipper::native_zip::read_archive(&path, Some(&output)))
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(
        fs::read_dir(temp.path())?.count(),
        3,
        "a failed parallel extraction leaves no staging directory"
    );
    Ok(())
}

#[test]
fn unsafe_paths_and_file_ancestors_never_publish() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("unsafe.zip");
    let out = temp.path().join("out");
    for name in [
        "../escaped",
        "/absolute",
        "a/../b",
        "a\\b",
        "C:drive",
        "a//b",
    ] {
        archive(&input, &[(name, b"payload")], CompressionMethod::Stored)?;
        assert!(
            zipper::native_zip::read_archive(&input, Some(&out)).is_err(),
            "{name}"
        );
        assert!(!out.exists());
    }
    archive(
        &input,
        &[("a", b"file"), ("a/b", b"child")],
        CompressionMethod::Stored,
    )?;
    assert!(zipper::native_zip::read_archive(&input, Some(&out)).is_err());
    assert!(!out.exists());
    assert!(!temp.path().join("escaped").exists());
    Ok(())
}

#[test]
fn corruption_and_truncation_fail_without_partial_publication() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("bad.zip");
    let out = temp.path().join("out");
    archive(
        &input,
        &[("data", b"original data")],
        CompressionMethod::Stored,
    )?;
    let original = fs::read(&input)?;
    let mut reader = zip::ZipArchive::new(fs::File::open(&input)?)?;
    let start = reader.by_index_raw(0)?.data_start().unwrap() as usize;
    drop(reader);
    let mut damaged = original.clone();
    damaged[start] ^= 1;
    fs::write(&input, damaged)?;
    assert!(zipper::native_zip::list(&input).is_ok());
    assert!(zipper::native_zip::read_archive(&input, None).is_err());
    assert!(zipper::native_zip::read_archive(&input, Some(&out)).is_err());
    assert!(!out.exists());
    for end in [0, 1, 30, start, original.len() - 1] {
        fs::write(&input, &original[..end])?;
        assert!(zipper::native_zip::read_archive(&input, Some(&out)).is_err());
        assert!(!out.exists());
    }
    Ok(())
}

#[test]
fn oversized_central_directory_is_rejected_before_parsing() -> anyhow::Result<()> {
    use std::io::{Seek, SeekFrom};
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("oversized.zip");
    let mut file = fs::File::create(&path)?;
    let length = 64u32 * 1024 * 1024 + 1;
    file.seek(SeekFrom::Start(length as u64))?;
    let mut end = [0u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    end[8..10].copy_from_slice(&1u16.to_le_bytes());
    end[10..12].copy_from_slice(&1u16.to_le_bytes());
    end[12..16].copy_from_slice(&length.to_le_bytes());
    file.write_all(&end)?;
    drop(file);
    let error = zipper::native_zip::list(&path).unwrap_err().to_string();
    assert!(error.contains("metadata limit"), "{error}");
    Ok(())
}

#[test]
fn zip64_end_records_and_empty_archives() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("zip64.zip");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"PK\x06\x06");
    bytes.extend_from_slice(&44u64.to_le_bytes());
    bytes.extend_from_slice(&45u16.to_le_bytes());
    bytes.extend_from_slice(&45u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 40]);
    bytes.extend_from_slice(b"PK\x06\x07");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"PK\x05\x06");
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&[255; 12]);
    bytes.extend_from_slice(&[0; 2]);
    fs::write(&path, &bytes)?;
    assert!(zipper::native_zip::read_archive(&path, None)?.is_empty());
    bytes[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, &bytes)?;
    assert!(zipper::native_zip::list(&path).is_err());
    let writer = ZipWriter::new(fs::File::create(&path)?);
    writer.finish()?;
    let out = temp.path().join("out");
    assert!(zipper::native_zip::read_archive(&path, Some(&out))?.is_empty());
    assert!(out.is_dir());
    Ok(())
}

#[test]
fn embedded_end_record_cannot_hide_real_entries() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ambiguous.zip");
    let mut writer = ZipWriter::new(fs::File::create(&path)?);
    writer.start_file(
        "must-not-disappear",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )?;
    writer.write_all(b"expected bytes")?;
    let mut comment = vec![0u8; 23];
    comment[..4].copy_from_slice(b"PK\x05\x06");
    comment[22] = b'x';
    writer.set_raw_comment(comment.into_boxed_slice())?;
    writer.finish()?;
    let out = temp.path().join("out");
    assert!(zipper::native_zip::read_archive(&path, Some(&out)).is_err());
    assert!(!out.exists());
    Ok(())
}

#[test]
fn zip_rejects_wrong_sizes_and_malformed_deflate() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("bad-deflate.zip");
    archive(
        &path,
        &[("data", b"a repeated repeated repeated payload")],
        CompressionMethod::Deflated,
    )?;
    zipper::native_zip::read_archive(&path, None)?;
    let original = fs::read(&path)?;
    let central = original
        .windows(4)
        .position(|b| b == b"PK\x01\x02")
        .unwrap();
    for size in [0u32, 1, 100] {
        let mut damaged = original.clone();
        damaged[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
        fs::write(&path, damaged)?;
        let out = temp.path().join(format!("size-{size}"));
        assert!(zipper::native_zip::read_archive(&path, Some(&out)).is_err());
        assert!(!out.exists());
    }
    fs::write(&path, &original)?;
    let mut reader = zip::ZipArchive::new(fs::File::open(&path)?)?;
    let start = reader.by_index_raw(0)?.data_start().unwrap() as usize;
    drop(reader);
    let mut damaged = original.clone();
    damaged[start] = RESERVED_DEFLATE_BLOCK;
    fs::write(&path, damaged)?;
    assert!(zipper::native_zip::read_archive(&path, None).is_err());
    let out = temp.path().join("malformed");
    assert!(zipper::native_zip::read_archive(&path, Some(&out)).is_err());
    assert!(!out.exists());
    let mut short = original;
    short[central + 20..central + 24].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&path, short)?;
    assert!(zipper::native_zip::read_archive(&path, None).is_err());
    Ok(())
}

#[test]
fn parallel_zip_failure_cleans_up_all_workers_outputs() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("parallel.zip");
    let payload = vec![b'x'; 1024 * 1024];
    archive(
        &path,
        &[
            ("a", &payload),
            ("b", &payload),
            ("c", &payload),
            ("d", &payload),
            ("e", &payload),
            ("f", &payload),
        ],
        CompressionMethod::Deflated,
    )?;
    let mut damaged = fs::read(&path)?;
    let at = damaged
        .windows(4)
        .enumerate()
        .filter(|(_, bytes)| *bytes == b"PK\x01\x02")
        .nth(2)
        .unwrap()
        .0;
    damaged[at + 16] ^= 1;
    fs::write(&path, damaged)?;
    let out = temp.path().join("out");
    assert!(zipper::native_zip::read_archive(&path, Some(&out)).is_err());
    assert!(!out.exists());
    assert_eq!(fs::read_dir(temp.path())?.count(), 1);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_devices_and_aliases_are_rejected() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("devices.zip");
    for name in ["NUL", "CON.txt", "COM1", "LPT¹.log", "name.", "name "] {
        archive(&path, &[(name, b"unsafe")], CompressionMethod::Stored)?;
        assert!(zipper::native_zip::list(&path).is_err(), "{name}");
    }
    Ok(())
}

#[test]
fn streaming_empty_entries_still_validate_crc_and_cleanup() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("empty.zip");
    for method in [CompressionMethod::Stored, CompressionMethod::Deflated] {
        archive(&path, &[("empty", b"")], method)?;
        let original = fs::read(&path)?;
        let central = original
            .windows(4)
            .position(|b| b == b"PK\x01\x02")
            .unwrap();
        let options = zipper::native_zip::ReadOptions {
            threads: Some(1),
            max_output_bytes: None,
            ..Default::default()
        };
        fs::write(&path, &original)?;
        let out = temp.path().join("out");
        zipper::native_zip::read_archive_with_options(&path, Some(&out), options)?;
        assert_eq!(fs::read(out.join("empty"))?, b"");
        fs::remove_dir_all(&out)?;
        let mut damaged = original.clone();
        damaged[central + 16] ^= 1;
        fs::write(&path, damaged)?;
        assert!(zipper::native_zip::read_archive_with_options(&path, None, options).is_err());
        assert!(zipper::native_zip::read_archive_with_options(&path, Some(&out), options).is_err());
        assert!(!out.exists());
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
    }
    Ok(())
}

#[test]
fn large_entries_written_behind_decoding_match_and_fail_closed() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut data = Vec::with_capacity(20 << 20);
    let mut state = 0x9e37_79b9_u32;
    while data.len() < 20 << 20 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        data.extend_from_slice(&state.to_le_bytes());
        data.extend_from_slice(b"text that compresses ");
    }
    for method in [CompressionMethod::Stored, CompressionMethod::Deflated] {
        let input = temp.path().join(format!("{method:?}.zip"));
        archive(&input, &[("big", &data)], method)?;
        let out = temp.path().join(format!("{method:?}-native"));
        zipper::native_zip::read_archive(&input, Some(&out))?;
        assert!(fs::read(out.join("big"))? == data, "{method:?}");
        let original = fs::read(&input)?;
        let mut reader = zip::ZipArchive::new(fs::File::open(&input)?)?;
        let entry = reader.by_index_raw(0)?;
        let (start, length) = (
            entry.data_start().unwrap() as usize,
            entry.compressed_size() as usize,
        );
        drop(entry);
        drop(reader);
        let mut damaged = original.clone();
        damaged[start + length - 1000] ^= 0x55;
        let truncated = original[..start + length - 1000].to_vec();
        for (case, bytes) in [("damaged", damaged), ("truncated", truncated)] {
            fs::write(&input, bytes)?;
            let out = temp.path().join(format!("{method:?}-{case}"));
            assert!(
                zipper::native_zip::read_archive(&input, Some(&out)).is_err(),
                "{method:?} {case}"
            );
            assert!(!out.exists(), "{method:?} {case}");
        }
    }
    let leftovers: Vec<_> = fs::read_dir(temp.path())?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
    Ok(())
}

fn modified_seconds(path: &Path) -> i64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn ntfs_field(seconds: i64) -> Vec<u8> {
    let ticks = ((seconds + 11_644_473_600) * 10_000_000) as u64;
    let mut field = vec![0u8; 4];
    field.extend_from_slice(&1u16.to_le_bytes());
    field.extend_from_slice(&24u16.to_le_bytes());
    for _ in 0..3 {
        field.extend_from_slice(&ticks.to_le_bytes());
    }
    field
}

#[test]
fn ntfs_and_dos_only_times_are_restored_on_every_platform() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("windows-made.zip");
    let ntfs_seconds = 1_262_347_384;
    let dos = zip::DateTime::from_date_and_time(2015, 6, 15, 12, 0, 0).unwrap();
    let dos_as_utc = 1_434_369_600;
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive)?);
    for (name, ntfs) in [
        ("ntfs/", true),
        ("ntfs/file", true),
        ("dos/", false),
        ("dos/file", false),
    ] {
        let mut options = zip::write::FullFileOptions::default().last_modified_time(dos);
        if ntfs {
            options.add_extra_field(0x000A, ntfs_field(ntfs_seconds), false)?;
        }
        if name.ends_with('/') {
            writer.add_directory(name, options)?;
        } else {
            writer.start_file(name, options)?;
            writer.write_all(b"data")?;
        }
    }
    writer.finish()?;
    let bytes = fs::read(&archive)?;
    assert!(
        !bytes.windows(2).any(|window| window == b"UT"),
        "the fixture must carry no extended timestamp"
    );
    let out = temp.path().join("out");
    zipper::native_zip::read_archive(&archive, Some(&out))?;
    for path in ["ntfs", "ntfs/file"] {
        assert_eq!(modified_seconds(&out.join(path)), ntfs_seconds, "{path}");
    }
    for path in ["dos", "dos/file"] {
        let offset = modified_seconds(&out.join(path)) - dos_as_utc;
        assert!(
            offset.abs() <= 14 * 3600,
            "{path} is {offset} s from its DOS time, past any time zone"
        );
    }
    Ok(())
}

fn many_entries_of_every_size() -> Vec<(String, Vec<u8>)> {
    let mut state = 0x2545_f491_u32;
    (0..240)
        .map(|index| {
            let length = match index % 8 {
                0 => 3 * 1024 * 1024 + index * 7,
                1 | 2 => 70_000 + index * 13,
                _ => index * 37 % 9_000,
            };
            let bytes = (0..length)
                .map(|at| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    if index % 3 == 0 {
                        state as u8
                    } else {
                        (at % 61) as u8
                    }
                })
                .collect();
            (format!("folder-{}/entry-{index}", index % 9), bytes)
        })
        .collect()
}

fn zip_crate_reads(archive: &Path) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut reader = zip::ZipArchive::new(fs::File::open(archive)?)?;
    (0..reader.len())
        .map(|index| {
            let mut entry = reader.by_index(index)?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)?;
            Ok((entry.name()?.into_owned(), bytes))
        })
        .collect()
}

fn assert_tree_holds(
    out: &Path,
    expected: &[(String, Vec<u8>)],
    label: &str,
) -> anyhow::Result<()> {
    for (name, bytes) in expected {
        assert!(fs::read(out.join(name))? == *bytes, "{label}: {name}");
    }
    let files = walkdir::WalkDir::new(out)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .count();
    assert_eq!(files, expected.len(), "{label}");
    Ok(())
}

#[test]
fn workers_sharing_one_archive_recover_the_zip_crates_bytes_at_every_width() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let entries = many_entries_of_every_size();
    let archive = temp.path().join("shared.zip");
    let mut writer = ZipWriter::new(fs::File::create(&archive)?);
    for (index, (name, bytes)) in entries.iter().enumerate() {
        let method = if index % 2 == 0 {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        writer.start_file(
            name,
            SimpleFileOptions::default().compression_method(method),
        )?;
        writer.write_all(bytes)?;
    }
    writer.finish()?;
    let expected = zip_crate_reads(&archive)?;
    assert!(
        expected == entries,
        "the zip crate reads back what was written"
    );
    let set = temp.path().join("set");
    fs::create_dir(&set)?;
    let whole = fs::read(&archive)?;
    for (index, volume) in whole.chunks(1_000_003).enumerate() {
        fs::write(set.join(format!("shared.zip.{:03}", index + 1)), volume)?;
    }
    assert!(whole.len() / 1_000_003 >= 8, "the set spans many volumes");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(64).build()?;
    for threads in [1, 4, 16, 64] {
        let out = temp.path().join(format!("plain-{threads}"));
        pool.install(|| {
            zipper::native_zip::read_archive_with_options(
                &archive,
                Some(&out),
                zipper::native_zip::ReadOptions {
                    threads: Some(threads),
                    ..Default::default()
                },
            )
        })?;
        assert_tree_holds(&out, &expected, &format!("plain at {threads} threads"))?;
        let out = temp.path().join(format!("split-{threads}"));
        pool.install(|| {
            zipper::extract_archive(
                &set.join("shared.zip.001"),
                &out,
                zipper::ExtractOptions {
                    threads: Some(threads),
                    ..Default::default()
                },
            )
        })?;
        assert_tree_holds(&out, &expected, &format!("split at {threads} threads"))?;
    }
    Ok(())
}
