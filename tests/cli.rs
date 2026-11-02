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
    fs::write(t.path().join("legacy.7z"), b"7z\xBC\xAF\x27\x1C fake")?;
    let result = run(&["test", "legacy.7z"])?;
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
    fs::write(t.path().join("legacy.7z"), b"7z\xBC\xAF\x27\x1C fake")?;
    let status = Command::new(env!("CARGO_BIN_EXE_zipper"))
        .current_dir(t.path())
        .args(["extract", "legacy.7z", "-o", "out"])
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
    assert!(!backend.status.success());
    assert!(!t.path().join("zip").exists());
    let explicit = run(&[
        "--zip-engine",
        "backend",
        "extract",
        "legacy.zip",
        "-o",
        "zip",
        "--max-extracted-bytes",
        "1",
    ])?;
    assert!(
        String::from_utf8_lossy(&explicit.stderr)
            .contains("--max-extracted-bytes needs a .zipper archive")
    );
    Ok(())
}

#[cfg(unix)]
fn days_since_unix_epoch(year: i64, month: i64, day: i64) -> i64 {
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
    let as_utc = days_since_unix_epoch(time.year().into(), time.month().into(), time.day().into())
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

#[cfg(unix)]
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
    let noon_edt_in_utc = days_since_unix_epoch(2024, 7, 1) * 86_400 + 16 * 3_600;
    assert_eq!(
        restored_dos_time(pack(2024, 7, 1, 12), "America/New_York")?,
        noon_edt_in_utc
    );
    assert_eq!(
        restored_dos_time(pack(2020, 13, 1, 0), "UTC")?,
        days_since_unix_epoch(2021, 1, 1) * 86_400
    );
    Ok(())
}

fn stored_zip(entries: &[(&str, u16)]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, _) in entries {
        writer.start_file(*name, options)?;
        writer.write_all(b"payload")?;
    }
    let mut bytes = writer.finish()?.into_inner();
    for (signature, method_at) in [(b"PK\x03\x04", 8), (b"PK\x01\x02", 10)] {
        let starts: Vec<usize> = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, window)| window == signature)
            .map(|(at, _)| at)
            .collect();
        for (start, (_, method)) in starts.into_iter().zip(entries) {
            bytes[start + method_at..start + method_at + 2].copy_from_slice(&method.to_le_bytes());
        }
    }
    Ok(bytes)
}

#[test]
fn default_engine_reads_zip_in_process_and_never_hands_it_to_7zip() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(args)
            .env("ZIPPER_7Z", t.path().join("missing-backend"))
            .output()
    };
    fs::write(t.path().join("plain.zip"), stored_zip(&[("f", 0)])?)?;
    for args in [
        &["extract", "plain.zip", "-o", "out"][..],
        &["list", "plain.zip"],
        &["test", "plain.zip"],
    ] {
        let result = run(args)?;
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(fs::read(t.path().join("out/f"))?, b"payload");

    for (name, bytes, expected) in [
        (
            "bzip2.zip",
            stored_zip(&[("f", 12)])?,
            "--zip-engine backend",
        ),
        (
            "bzip2-then-escape.zip",
            stored_zip(&[("f", 12), ("../escape", 0)])?,
            "--zip-engine backend",
        ),
        (
            "escape.zip",
            stored_zip(&[("../escape", 0)])?,
            "unsafe archive path",
        ),
    ] {
        fs::write(t.path().join(name), bytes)?;
        for command in ["extract", "list", "test"] {
            let mut args = vec![command, name];
            if command == "extract" {
                args.extend(["-o", "rejected"]);
            }
            let result = run(&args)?;
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(!result.status.success(), "{name} {command}");
            assert!(
                !stderr.contains("7-Zip backend") && !stderr.contains("failed to launch"),
                "{name} {command}: {stderr}"
            );
            if command == "extract" {
                assert!(stderr.contains(expected), "{name}: {stderr}");
            }
        }
        assert!(!t.path().join("rejected").exists(), "{name}");
        assert!(
            !t.path().parent().unwrap().join("escape").exists(),
            "{name}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn backend_engine_publishes_what_7zip_extracts() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir()?;
    let backend = t.path().join("backend");
    fs::write(
        &backend,
        "#!/bin/sh\nfor a; do case $a in -o*) d=${a#-o};; esac; done\necho backend > \"$d/marker\"\n",
    )?;
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700))?;
    fs::write(t.path().join("bzip2.zip"), stored_zip(&[("f", 12)])?)?;
    let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
        .current_dir(t.path())
        .args([
            "--zip-engine",
            "backend",
            "extract",
            "bzip2.zip",
            "-o",
            "out",
        ])
        .env("ZIPPER_7Z", &backend)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(t.path().join("out/marker"))?, b"backend\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn default_engine_pins_known_formats_for_7zip_and_reads_everything_else_in_process()
-> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir()?;
    let backend = t.path().join("backend");
    fs::write(
        &backend,
        "#!/bin/sh\necho \"$*\" >> \"$0.log\"\n\
         for a; do case $a in -o*) d=${a#-o};; esac; done\necho 7zip > \"$d/marker\"\n",
    )?;
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700))?;
    let zip = stored_zip(&[("f", 0)])?;
    let mut tar_with_zip = vec![0u8; 512];
    tar_with_zip[257..262].copy_from_slice(b"ustar");
    tar_with_zip.extend(&zip);
    let mut sevenzip_with_zip = b"7z\xBC\xAF\x27\x1C".to_vec();
    sevenzip_with_zip.extend(&zip);
    let mut shebang_with_zip = b"#!/bin/sh\nexit 0\n".to_vec();
    shebang_with_zip.extend(&zip);
    for (name, bytes, switch) in [
        ("a.tar", tar_with_zip, Some("-ttar ")),
        ("b.bin", sevenzip_with_zip.clone(), Some("-t7z ")),
        ("c.7z.001", sevenzip_with_zip, Some("-t7z.split ")),
        ("d.bin", shebang_with_zip, None),
        ("e.bin", b"Type = zip\n".to_vec(), None),
    ] {
        fs::write(t.path().join(name), bytes)?;
        let _ = fs::remove_file(t.path().join("backend.log"));
        let output = t.path().join(format!("out-{name}"));
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(["extract", name, "-o"])
            .arg(&output)
            .env("ZIPPER_7Z", &backend)
            .output()?;
        let log = fs::read_to_string(t.path().join("backend.log")).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&result.stderr);
        match switch {
            Some(switch) => {
                assert!(result.status.success(), "{name}: {stderr}");
                assert!(log.contains(switch), "{name}: {log}");
            }
            None => {
                assert!(!result.status.success(), "{name}");
                assert!(log.is_empty(), "{name} reached 7-Zip: {log}");
                assert!(
                    stderr.contains("--zip-engine backend") || stderr.contains("ZIP"),
                    "{name}: {stderr}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn real_7zip_routes_split_prefixed_and_wrapped_zips_correctly() -> anyhow::Result<()> {
    let Some(sevenzip) = ["7zz", "7z"].into_iter().find(|name| {
        Command::new(name)
            .arg("i")
            .output()
            .is_ok_and(|o| o.status.success())
    }) else {
        return Ok(());
    };
    let t = tempfile::tempdir()?;
    let zip = stored_zip(&[("Readme.txt", 0), ("../escape", 0)])?;
    fs::write(t.path().join("c.zip.001"), &zip[..3])?;
    fs::write(t.path().join("c.zip.002"), &zip[3..])?;
    let mut prefixed = b"#!/bin/sh\nexit 0\n".to_vec();
    prefixed.extend(&zip);
    fs::write(t.path().join("prefixed.bin"), prefixed)?;
    fs::create_dir(t.path().join("tree"))?;
    fs::write(t.path().join("tree/inner.zip"), stored_zip(&[("f", 0)])?)?;
    let tarred = Command::new(sevenzip)
        .current_dir(t.path())
        .args(["a", "-ttar", "wrapped.tar", "tree"])
        .output()?;
    assert!(tarred.status.success());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(args)
            .env("ZIPPER_7Z", sevenzip)
            .output()
    };
    for archive in ["c.zip.001", "prefixed.bin"] {
        let result = run(&["extract", archive, "-o", "rejected"])?;
        assert!(!result.status.success(), "{archive} reached 7-Zip");
        assert!(!t.path().join("rejected").exists(), "{archive}");
    }
    let hidden = Command::new(sevenzip)
        .current_dir(t.path())
        .args(["a", "-t7z", "-mhe=on", "-psecret", "hidden.7z", "tree"])
        .output()?;
    assert!(hidden.status.success());
    let mut prefixed = std::io::Cursor::new(fs::read(t.path().join("hidden.7z"))?);
    std::io::Seek::seek(&mut prefixed, std::io::SeekFrom::End(0))?;
    let mut writer = zip::ZipWriter::new(prefixed);
    for name in ["Readme.txt", "../escape"] {
        writer.start_file(name, zip::write::SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut writer, b"payload")?;
    }
    fs::write(t.path().join("polyglot.zip"), writer.finish()?.into_inner())?;
    let mut guessed = Command::new(env!("CARGO_BIN_EXE_zipper"))
        .current_dir(t.path())
        .args(["extract", "polyglot.zip", "-o", "polyglot"])
        .env("ZIPPER_7Z", sevenzip)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        let _ = guessed.stdin.take().unwrap().write_all(b"wrong\nwrong\n");
    }
    let guessed = guessed.wait_with_output()?;
    assert!(!guessed.status.success(), "the appended ZIP reached 7-Zip");
    assert!(!t.path().join("polyglot").exists());
    let wrapped = run(&["extract", "wrapped.tar", "-o", "wrapped"])?;
    assert!(
        wrapped.status.success(),
        "{}",
        String::from_utf8_lossy(&wrapped.stderr)
    );
    assert!(t.path().join("wrapped/tree/inner.zip").is_file());
    let split = Command::new(sevenzip)
        .current_dir(t.path())
        .args(["a", "-t7z", "-v200", "volumes.7z", "tree"])
        .output()?;
    assert!(split.status.success());
    let joined = run(&["extract", "volumes.7z.001", "-o", "volumes"])?;
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert!(t.path().join("volumes/tree/inner.zip").is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn wildcards_in_7zip_paths_are_refused_before_7zip_runs() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir()?;
    let backend = t.path().join("backend");
    fs::write(&backend, "#!/bin/sh\necho called >> \"$0.log\"\n")?;
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700))?;
    let sevenzip = b"7z\xBC\xAF\x27\x1C fake";
    fs::write(t.path().join("a?.7z"), sevenzip)?;
    fs::write(t.path().join("plain.7z"), sevenzip)?;
    fs::create_dir(t.path().join("p*"))?;
    for (archive, output) in [("a?.7z", "out"), ("plain.7z", "p*/out")] {
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .current_dir(t.path())
            .args(["extract", archive, "-o", output])
            .env("ZIPPER_7Z", &backend)
            .output()?;
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{archive} -> {output}");
        assert!(stderr.contains("wildcards"), "{stderr}");
        assert!(
            !t.path().join("backend.log").exists(),
            "{archive} reached 7-Zip"
        );
        assert!(!t.path().join(output).exists());
    }
    Ok(())
}
