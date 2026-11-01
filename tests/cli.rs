use std::{fs, process::Command};

#[test]
fn native_cli_and_missing_backend() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    fs::write(t.path().join("data"), b"zipper")?;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(args)
            .env("ZIPPER_7Z", t.path().join("missing-backend"))
            .output()
    };
    assert!(run(&["create", "a.zipper", "data"])?.status.success());
    assert!(run(&["test", "a.zipper"])?.status.success());
    assert!(run(&["list", "a.zipper"])?.status.success());
    assert!(run(&["extract", "a.zipper", "-o", "out"])?.status.success());
    assert_eq!(fs::read(t.path().join("out/data"))?, b"zipper");
    assert!(
        run(&["extract", "a.zipper", "-o", "selected", "--path", "data"])?
            .status
            .success()
    );
    assert_eq!(fs::read(t.path().join("selected/data"))?, b"zipper");
    fs::write(t.path().join("legacy.zip"), b"PK fake")?;
    let result = run(&["test", "legacy.zip"])?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("failed to launch 7-Zip"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn compatibility_backend_failure_does_not_publish() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir()?;
    let backend = t.path().join("backend");
    fs::write(&backend, "#!/bin/sh\nexit 2\n")?;
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700))?;
    fs::write(t.path().join("legacy.zip"), b"PK fake")?;
    let status = Command::new(env!("CARGO_BIN_EXE_zipper"))
        .current_dir(t.path())
        .args(["extract", "legacy.zip", "-o", "out"])
        .env("ZIPPER_7Z", backend)
        .output()?;
    assert!(!status.status.success());
    assert!(!t.path().join("out").exists());
    Ok(())
}

#[test]
fn max_extracted_bytes_caps_native_archives_before_writing() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    fs::create_dir(t.path().join("data"))?;
    fs::write(t.path().join("data/file"), vec![7u8; 4096])?;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(args)
            .env("ZIPPER_7Z", t.path().join("missing-backend"))
            .output()
    };
    assert!(run(&["create", "a.zipper", "data"])?.status.success());
    let refused = run(&[
        "extract",
        "a.zipper",
        "-o",
        "small",
        "--max-extracted-bytes",
        "4095",
    ])?;
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--max-extracted-bytes"));
    assert!(!t.path().join("small").exists());
    let selected = run(&[
        "extract",
        "a.zipper",
        "-o",
        "part",
        "--path",
        "data",
        "--max-extracted-bytes",
        "4095",
    ])?;
    assert!(!selected.status.success());
    assert!(
        run(&[
            "extract",
            "a.zipper",
            "-o",
            "fits",
            "--max-extracted-bytes",
            "4096"
        ])?
        .status
        .success()
    );
    assert_eq!(fs::read(t.path().join("fits/data/file"))?, vec![7u8; 4096]);
    fs::write(t.path().join("legacy.zip"), b"PK fake")?;
    let backend = run(&[
        "extract",
        "legacy.zip",
        "-o",
        "zip",
        "--max-extracted-bytes",
        "1",
    ])?;
    assert!(String::from_utf8_lossy(&backend.stderr).contains("requires a native archive"));
    Ok(())
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(unix)]
#[test]
fn dos_times_are_read_in_the_local_time_zone() -> anyhow::Result<()> {
    let archive = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/zip-corpus/python-deflate.zip");
    let path = "src/text/readme.txt";
    let mut zip = zip::ZipArchive::new(fs::File::open(&archive)?)?;
    let time = zip.by_name(path)?.last_modified().unwrap();
    let as_utc = days_from_civil(time.year().into(), time.month().into(), time.day().into())
        * 86_400
        + i64::from(time.hour()) * 3_600
        + i64::from(time.minute()) * 60
        + i64::from(time.second());
    let t = tempfile::tempdir()?;
    let mut restored = Vec::new();
    for zone in ["UTC", "Asia/Tokyo"] {
        let output = t.path().join(zone.replace('/', "-"));
        let status = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["--zip-engine", "native", "extract"])
            .arg(&archive)
            .arg("-o")
            .arg(&output)
            .env("TZ", zone)
            .status()?;
        assert!(status.success());
        let modified = fs::metadata(output.join(path))?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        restored.push(modified);
    }
    assert_eq!(restored, [as_utc, as_utc - 9 * 3_600]);
    Ok(())
}

/// One stored entry, `a.txt`, with the given DOS time (low 16 bits) and date.
fn dos_only_zip(dos: u32) -> Vec<u8> {
    let (name, data) = (b"a.txt", b"x");
    let crc = crc32fast::hash(data).to_le_bytes();
    let mut out = b"PK\x03\x04\x14\0\0\0\0\0".to_vec();
    out.extend_from_slice(&dos.to_le_bytes());
    out.extend_from_slice(&crc);
    out.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0]);
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    let directory = out.len() as u32;
    out.extend_from_slice(b"PK\x01\x02\x14\0\x14\0\0\0\0\0");
    out.extend_from_slice(&dos.to_le_bytes());
    out.extend_from_slice(&crc);
    out.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 5, 0]);
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(name);
    let size = out.len() as u32 - directory;
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(unix)]
fn restored_dos_time(dos: u32, zone: &str) -> anyhow::Result<i64> {
    let t = tempfile::tempdir()?;
    fs::write(t.path().join("a.zip"), dos_only_zip(dos))?;
    let status = Command::new(env!("CARGO_BIN_EXE_zipper"))
        .current_dir(t.path())
        .args(["--zip-engine", "native", "extract", "a.zip", "-o", "out"])
        .env("TZ", zone)
        .status()?;
    assert!(status.success());
    Ok(fs::metadata(t.path().join("out/a.txt"))?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64)
}

#[cfg(unix)]
#[test]
fn dos_times_follow_daylight_saving_and_normalize_like_bsdtar() -> anyhow::Result<()> {
    let pack = |year: u32, month: u32, day: u32, hour: u32| {
        (year - 1980) << 25 | month << 21 | day << 16 | hour << 11
    };
    // Noon on 2024-07-01 in New York is daylight time, four hours behind UTC.
    let summer = days_from_civil(2024, 7, 1) * 86_400 + 16 * 3_600;
    assert_eq!(
        restored_dos_time(pack(2024, 7, 1, 12), "America/New_York")?,
        summer
    );
    // Month 13 of 2020 normalizes to January 2021.
    assert_eq!(
        restored_dos_time(pack(2020, 13, 1, 0), "UTC")?,
        1_609_459_200
    );
    Ok(())
}
