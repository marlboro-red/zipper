//! Existing archive formats are handled by a separately installed 7-Zip binary.
use anyhow::{Context, Result, bail, ensure};
use std::{fs::File, io::Read, path::Path, process::Command};

const ANY_NATIVE_VERSION_PREFIX: &[u8; 6] = b"ZIPPER";

pub fn is_native(path: &Path) -> Result<bool> {
    let mut f = File::open(path)?;
    let mut prefix = [0; 6];
    let n = f.read(&mut prefix)?;
    Ok(n == prefix.len() && &prefix == ANY_NATIVE_VERSION_PREFIX)
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

/// Formats the default engine hands to 7-Zip, by signature and offset, with
/// the 7-Zip `-t` name that pins each one. ZIP is absent on purpose.
const PINNED_FORMATS: &[(&[u8], usize, &str)] = &[
    (b"7z\xBC\xAF\x27\x1C", 0, "7z"),
    (b"Rar!\x1A\x07\x01\x00", 0, "rar5"),
    (b"Rar!\x1A\x07\x00", 0, "rar"),
    (b"\x1F\x8B", 0, "gzip"),
    (b"BZh", 0, "bzip2"),
    (b"\xFD7zXZ\x00", 0, "xz"),
    (b"MSCF", 0, "cab"),
];
const SIGNATURE_WINDOW: usize = 8;

/// The `-t` switch for a format the default engine sends to 7-Zip, or `None`
/// when the file starts with none of them. The first volume of a split
/// archive, named `*.001`, gets the split chain, such as `-t7z.split`.
pub fn pinned_format(archive: &Path) -> Result<Option<String>> {
    let mut head = Vec::with_capacity(SIGNATURE_WINDOW);
    File::open(archive)?
        .take(SIGNATURE_WINDOW as u64)
        .read_to_end(&mut head)?;
    let split = archive
        .canonicalize()?
        .extension()
        .is_some_and(|extension| extension == "001");
    Ok(PINNED_FORMATS
        .iter()
        .find(|(signature, at, _)| head.get(*at..at + signature.len()) == Some(*signature))
        .map(|(_, _, name)| format!("-t{name}{}", if split { ".split" } else { "" })))
}

// EXTERNAL(7-Zip): it expands `*` and `?` in archive paths even after `--`,
// and replaces `*` in the -o path with the archive name.
fn ensure_no_wildcards(path: &Path) -> Result<()> {
    let named = path.components().any(|component| {
        matches!(component, std::path::Component::Normal(name) if name.to_string_lossy().contains(['*', '?']))
    });
    ensure!(
        !named,
        "7-Zip treats `*` and `?` in {} as wildcards; rename the path",
        path.display()
    );
    Ok(())
}

/// Run 7-Zip `action` on `archive`. `format` pins the format, as from
/// [`pinned_format`], so 7-Zip cannot open the file as another one.
pub fn run(
    action: &str,
    archive: &Path,
    destination: Option<&Path>,
    format: Option<&str>,
    existing: crate::Existing,
) -> Result<()> {
    ensure!(
        ["x", "l", "t"].contains(&action),
        "unsupported backend action"
    );
    let archive = archive.canonicalize()?;
    ensure_no_wildcards(&archive)?;
    let mut command = Command::new(backend()?);
    command.arg(action).arg("-y").args(format);
    let staging = if let Some(dst) = destination {
        let temp = crate::staging::stage(dst, existing)?;
        let staging = temp.path().canonicalize()?;
        ensure_no_wildcards(&staging)?;
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(staging);
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
    if let Some(staging) = staging {
        staging.publish(&[])?;
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_not_a_wildcard() {
        assert!(ensure_no_wildcards(Path::new(r"\\?\C:\work\a.7z")).is_ok());
        assert!(ensure_no_wildcards(Path::new(r"\\?\C:\work\a?.7z")).is_err());
        assert!(ensure_no_wildcards(Path::new(r"\\?\C:\p*\out")).is_err());
    }
}
