//! Existing archive formats are handled by a separately installed 7-Zip binary.
use anyhow::{Context, Result, bail, ensure};
use std::{fs::File, io::Read, path::Path, process::Command};

pub fn is_native(path: &Path) -> Result<bool> {
    let mut f = File::open(path)?;
    let mut prefix = [0; 6];
    let n = f.read(&mut prefix)?;
    // Route every native version to the native parser, including damaged headers.
    Ok(n == prefix.len() && &prefix == b"ZIPPER")
}

fn backend() -> Result<std::ffi::OsString> {
    if let Some(binary) = std::env::var_os("ZIPPER_7Z") {
        return Ok(binary);
    }
    for binary in ["7zz", "7z"] {
        if Command::new(binary)
            .arg("i")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Ok(binary.into());
        }
    }
    bail!(
        "existing archive formats require 7zz or 7z on PATH; alternatively set ZIPPER_7Z to a 7-Zip executable"
    )
}

pub fn run(action: &str, archive: &Path, destination: Option<&Path>) -> Result<()> {
    ensure!(
        ["x", "l", "t"].contains(&action),
        "unsupported backend action"
    );
    let archive = archive.canonicalize()?;
    let mut command = Command::new(backend()?);
    command.arg(action).arg("-y");
    let staging = if let Some(dst) = destination {
        ensure!(
            std::fs::symlink_metadata(dst).is_err(),
            "extraction destination must not exist"
        );
        let temp = tempfile::tempdir_in(super::parent(dst))?;
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(temp.path().canonicalize()?);
        command.arg(flag);
        Some(temp)
    } else {
        None
    };
    let status = command
        .arg("--")
        .arg(archive)
        .status()
        .context("failed to launch 7-Zip backend")?;
    ensure!(status.success(), "7-Zip backend failed: {status}");
    if let (Some(temp), Some(dst)) = (staging, destination) {
        ensure!(
            std::fs::symlink_metadata(dst).is_err(),
            "destination appeared during extraction"
        );
        std::fs::rename(temp.path(), dst)?;
    }
    Ok(())
}
