use std::{fs, io::Write, path::Path, process::Command};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

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
        zipper::native_zip::read_archive_fast(&input, None)?;
        let entries = zipper::native_zip::read_archive(&input, None)?;
        assert_eq!(entries.len(), 4);
        let out = temp.path().join(label);
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["--threads", "3", "--zip-engine", "native", "extract"])
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
    // Sparse fixture: advertised directory exceeds the allocation guard. No
    // directory bytes need to be read to reject it.
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
    // Minimal empty ZIP64 with an explicit EOCD64 and locator.
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
    // A forged enormous extension must fail before allocating that sector.
    bytes[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, &bytes)?;
    assert!(zipper::native_zip::list(&path).is_err());
    // Empty standard ZIP.
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
fn fast_zip_checks_crc_and_streams_entries_above_buffer_limit() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("fast.zip");
    for size in [2 * 1024 * 1024 + 7, 64 * 1024 * 1024 + 1] {
        let mut bytes = vec![b'A'; size];
        bytes[size - 1] = b'Z';
        archive(
            &input,
            &[("data", &bytes), ("zero", b"")],
            CompressionMethod::Deflated,
        )?;
        let out = temp.path().join(format!("out-{size}"));
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["--threads", "2", "--zip-engine", "native-fast", "extract"])
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
        assert_eq!(fs::read(out.join("data"))?, bytes);
        assert_eq!(fs::read(out.join("zero"))?, b"");
        // Corrupt the central-directory CRC, leaving a valid Deflate stream.
        let mut damaged = fs::read(&input)?;
        let at = damaged.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        damaged[at + 16] ^= 1;
        fs::write(&input, damaged)?;
        let failed = temp.path().join(format!("failed-{size}"));
        assert!(zipper::native_zip::read_archive_fast(&input, Some(&failed)).is_err());
        assert!(!failed.exists());
    }
    Ok(())
}

#[test]
fn fast_zip_rejects_wrong_sizes_and_malformed_deflate() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("bad-deflate.zip");
    archive(
        &path,
        &[("data", b"a repeated repeated repeated payload")],
        CompressionMethod::Deflated,
    )?;
    zipper::native_zip::read_archive_fast(&path, None)?;
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
        assert!(zipper::native_zip::read_archive_fast(&path, Some(&out)).is_err());
        assert!(!out.exists());
    }
    fs::write(&path, &original)?;
    let mut reader = zip::ZipArchive::new(fs::File::open(&path)?)?;
    let start = reader.by_index_raw(0)?.data_start().unwrap() as usize;
    drop(reader);
    let mut damaged = original.clone();
    damaged[start] = 0xff; // Reserved Deflate block type.
    fs::write(&path, damaged)?;
    assert!(zipper::native_zip::read_archive_fast(&path, None).is_err());
    let out = temp.path().join("malformed");
    assert!(zipper::native_zip::read_archive_fast(&path, Some(&out)).is_err());
    assert!(!out.exists());
    let mut short = original;
    short[central + 20..central + 24].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&path, short)?;
    assert!(zipper::native_zip::read_archive_fast(&path, None).is_err());
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
