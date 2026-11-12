//! Existing archive formats are handled by a separately installed 7-Zip binary.
use anyhow::{Context, Result, bail, ensure};
use std::{io::Read, path::Path, process::Command};

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

/// Formats `--zip-engine backend` hands to 7-Zip, by signature and offset,
/// with the 7-Zip `-t` name that pins each one. Every run of 7-Zip names one
/// of these, or one of [`ROUTED_TYPES`], so 7-Zip never searches a file for
/// a format Zipper refuses.
const IDENTIFIED_FORMATS: &[(&[u8], usize, &str)] = &[
    (b"7z\xBC\xAF\x27\x1C", 0, "7z"),
    (b"Rar!\x1A\x07\x01\x00", 0, "rar5"),
    (b"Rar!\x1A\x07\x00", 0, "rar"),
    (b"PK\x03\x04", 0, "zip"),
    (b"PK\x05\x06", 0, "zip"),
    (b"\x1F\x8B", 0, "gzip"),
    (b"BZh", 0, "bzip2"),
    (b"\xFD7zXZ\x00", 0, "xz"),
    (b"\x28\xB5\x2F\xFD", 0, "zstd"),
    (b"\x1F\x9D", 0, "Z"),
    (b"ustar", 257, "tar"),
    (b"xar!", 0, "xar"),
    (b"\xED\xAB\xEE\xDB", 0, "rpm"),
    (b"!<arch>\n", 0, "ar"),
    (b"070707", 0, "cpio"),
    (b"070701", 0, "cpio"),
    (b"070702", 0, "cpio"),
    (b"\xC7\x71", 0, "cpio"),
    (b"\x71\xC7", 0, "cpio"),
    (b"-lh", 2, "lzh"),
    (b"\x60\xEA", 0, "arj"),
    (b"hsqs", 0, "squashfs"),
    (b"sqsh", 0, "squashfs"),
    (b"\x45\x3D\xCD\x28", 0, "cramfs"),
    (b"\x28\xCD\x3D\x45", 0, "cramfs"),
    (b"CD001", 0x8001, "iso"),
    (b"BEA01", 0x8001, "udf"),
    (b"\x53\xEF", 1080, "ext"),
    (b"\x7FELF", 0, "elf"),
    (b"\xCE\xFA\xED\xFE", 0, "macho"),
    (b"\xCF\xFA\xED\xFE", 0, "macho"),
    (b"\xFE\xED\xFA\xCE", 0, "macho"),
    (b"\xFE\xED\xFA\xCF", 0, "macho"),
    (b"\xCA\xFE\xBA\xBE", 0, "mub"),
    (b"\xB9\xFA\xF1\x0E", 0, "mub"),
    (b"MZ", 0, "pe"),
];
const IDENTIFY_WINDOW: usize = 0x8001 + 5;

/// The 7-Zip types the default engine names after its own reader has checked
/// the file, beside those of [`IDENTIFIED_FORMATS`].
const ROUTED_TYPES: &[&str] = &["ihex", "lp", "uefic", "uefif"];

/// The `-t` switch for a format the default engine sends to 7-Zip, or `None`
/// when the file starts with none of them. The first volume of a split
/// archive, named `*.001`, gets the split chain, such as `-t7z.split`.
pub fn pinned_format(archive: &Path) -> Result<Option<String>> {
    switch_for(archive, PINNED_FORMATS, SIGNATURE_WINDOW)
}

/// The `-t` switch `--zip-engine backend` passes to 7-Zip for `archive`, or
/// `None` when it starts with no signature of a format Zipper hands to 7-Zip.
/// A `*.001` file gets the split chain, as in [`pinned_format`].
pub fn identified_format(archive: &Path) -> Result<Option<String>> {
    switch_for(archive, IDENTIFIED_FORMATS, IDENTIFY_WINDOW)
}

fn switch_for(
    archive: &Path,
    formats: &[(&[u8], usize, &str)],
    window: usize,
) -> Result<Option<String>> {
    let mut head = Vec::with_capacity(window);
    crate::volumes::open(archive)?
        .take(window as u64)
        .read_to_end(&mut head)?;
    let split = archive
        .canonicalize()?
        .extension()
        .is_some_and(|extension| extension == "001")
        && !crate::volumes::read_alone_beside_its_next_volume(archive)?;
    Ok(formats
        .iter()
        .find(|(signature, at, _)| head.get(*at..at + signature.len()) == Some(*signature))
        .map(|(_, _, name)| format!("-t{name}{}", if split { ".split" } else { "" })))
}

fn ensure_named_for_7zip(format: Option<&str>) -> Result<()> {
    let named = format
        .and_then(|switch| switch.strip_prefix("-t"))
        .map(|name| name.strip_suffix(".split").unwrap_or(name))
        .is_some_and(|name| {
            IDENTIFIED_FORMATS
                .iter()
                .map(|(_, _, allowed)| *allowed)
                .chain(ROUTED_TYPES.iter().copied())
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
        });
    if !named {
        bail!(crate::Refused(crate::refused::UNNAMED_FOR_7ZIP));
    }
    Ok(())
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
/// [`pinned_format`] or [`identified_format`], so 7-Zip cannot open the file
/// as another one. A file in a format Zipper refuses, and a `format` that
/// names no format Zipper hands to 7-Zip, fail with [`crate::Refused`] before
/// 7-Zip starts.
pub fn run(
    action: &str,
    archive: &Path,
    destination: Option<&Path>,
    format: Option<&str>,
    existing: crate::Existing,
) -> Result<()> {
    run_until_cancelled(action, archive, destination, format, existing, None)
}

const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(10);
#[cfg(unix)]
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Like [`run`], but when `cancel` is set it stops 7-Zip, waits for it to
/// exit, removes the staging folder and fails with [`crate::Cancelled`].
pub fn run_until_cancelled(
    action: &str,
    archive: &Path,
    destination: Option<&Path>,
    format: Option<&str>,
    existing: crate::Existing,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<()> {
    ensure!(
        ["x", "l", "t"].contains(&action),
        "unsupported backend action"
    );
    if let Some(format) = crate::refused::format(archive)? {
        bail!(crate::Refused(format));
    }
    ensure_named_for_7zip(format)?;
    let archive = archive.canonicalize()?;
    ensure_no_wildcards(&archive)?;
    let mut command = Command::new(backend()?);
    command
        .arg(action)
        .arg("-y")
        .args(format)
        .stdin(std::process::Stdio::null());
    let staging = if let Some(dst) = destination {
        let temp = crate::staging::stage(&archive, dst, existing, crate::Metadata::Restore)?;
        let staging = temp.path().canonicalize()?;
        ensure_no_wildcards(&staging)?;
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(staging);
        command.arg(flag);
        Some(temp)
    } else {
        None
    };
    let mut child = command
        .arg("--")
        .arg(archive)
        .spawn()
        .context("failed to launch 7-Zip backend")?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancelled(cancel) {
            stop(&mut child)?;
            return Err(crate::Cancelled.into());
        }
        std::thread::sleep(CANCEL_POLL);
    };
    if cancelled(cancel) {
        return Err(crate::Cancelled.into());
    }
    ensure!(status.success(), "7-Zip backend failed: {status}");
    if let Some(staging) = staging {
        staging.publish(&[], cancel)?;
    }
    Ok(())
}

fn cancelled(cancel: Option<&std::sync::atomic::AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
}

fn stop(child: &mut std::process::Child) -> Result<()> {
    #[cfg(unix)]
    if let Ok(id) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill takes plain integers, and the child has not been waited for, so its
        // process id still names it.
        unsafe { libc::kill(id, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + STOP_GRACE;
        while std::time::Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return Ok(());
            }
            std::thread::sleep(CANCEL_POLL);
        }
    }
    let _ = child.kill();
    child.wait()?;
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
