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
