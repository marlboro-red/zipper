use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
pub(crate) fn archive_mark(archive: &Path) -> Option<Vec<u8>> {
    let mut stream = archive.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::read(stream).ok()
}

#[cfg(not(windows))]
pub(crate) fn archive_mark(_: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(windows)]
pub(crate) fn mark_extracted_files(root: &Path, zone: &[u8]) -> Result<()> {
    if !holds_named_streams(root)? {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_file() {
            write_stream(entry.path(), "Zone.Identifier", zone)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "Windows only, where it clears FILE_ATTRIBUTE_READONLY"
)]
pub(crate) fn write_stream(base: &Path, name: &str, data: &[u8]) -> Result<()> {
    use anyhow::Context;
    let metadata = std::fs::symlink_metadata(base)?;
    let mut permissions = metadata.permissions();
    let read_only = permissions.readonly();
    if read_only {
        permissions.set_readonly(false);
        std::fs::set_permissions(base, permissions.clone())?;
    }
    let mut stream = base.as_os_str().to_owned();
    stream.push(":");
    stream.push(name);
    std::fs::write(&stream, data)
        .with_context(|| format!("cannot write stream {name:?} of {}", base.display()))?;
    if metadata.is_file() {
        std::fs::File::options()
            .write(true)
            .open(base)?
            .set_modified(metadata.modified()?)?;
    }
    if read_only {
        permissions.set_readonly(true);
        std::fs::set_permissions(base, permissions)?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn mark_extracted_files(_: &Path, _: &[u8]) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn holds_named_streams(folder: &Path) -> Result<bool> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, GetVolumeInformationByHandleW},
        System::SystemServices::FILE_NAMED_STREAMS,
    };
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(folder)?;
    let mut flags = 0u32;
    // SAFETY: the handle is open for the whole call, every other pointer is
    // null with both name sizes zero, and `flags` outlives the call.
    let succeeded = unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    anyhow::ensure!(
        succeeded != 0,
        "cannot read the volume flags of {}: {}",
        folder.display(),
        std::io::Error::last_os_error()
    );
    Ok(flags & FILE_NAMED_STREAMS != 0)
}

// EXTERNAL(NTFS): a stream name holds at most 255 UTF-16 units, as a file name does.
pub(crate) const MAX_STREAM_NAME: usize = 255;
pub(crate) const MARK_OF_THE_WEB: &str = "Zone.Identifier";
pub(crate) const WRITES_STREAMS: bool = cfg!(windows);
const FORBIDDEN: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
const NUMBERED_DEVICES: [&str; 2] = ["COM", "LPT"];
const DEVICE_DIGITS: &str = "0123456789\u{b9}\u{b2}\u{b3}";

fn is_device_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or_default().trim_end();
    let upper = base.to_ascii_uppercase();
    DEVICES.contains(&upper.as_str())
        || NUMBERED_DEVICES.iter().any(|device| {
            upper.strip_prefix(device).is_some_and(|digit| {
                let mut chars = digit.chars();
                matches!(
                    (chars.next(), chars.next()),
                    (Some(only), None) if DEVICE_DIGITS.contains(only)
                )
            })
        })
}

// EXTERNAL(NTFS $UpCase): names compare after mapping each UTF-16 unit to one unit, so a
// letter whose capital is longer, such as `ß`, and units outside the BMP stay as they are.
pub(crate) fn upcase(name: &str) -> String {
    name.chars()
        .map(|letter| {
            let mut capital = letter.to_uppercase();
            match (capital.next(), capital.next()) {
                (Some(one), None) if letter.len_utf16() == 1 && one.len_utf16() == 1 => one,
                _ => letter,
            }
        })
        .collect()
}

pub(crate) fn stream_name_problem(name: &str) -> Option<&'static str> {
    let units = name.encode_utf16().count();
    if units == 0 {
        Some("is empty")
    } else if units > MAX_STREAM_NAME {
        Some("is longer than the 255 UTF-16 units NTFS allows")
    } else if name
        .chars()
        .any(|c| c.is_control() || FORBIDDEN.contains(&c))
    {
        Some(
            "holds a control character, a path separator, a colon or another character Windows refuses",
        )
    } else if name.ends_with(['.', ' ']) {
        Some("ends with a dot or a space, which Windows drops")
    } else if is_device_name(name) {
        Some("is a reserved Windows device name")
    } else {
        None
    }
}

#[cfg(windows)]
pub(crate) fn destination_keeps_streams(root: &Path) -> Result<bool> {
    holds_named_streams(root)
}

#[cfg(not(windows))]
pub(crate) fn destination_keeps_streams(_: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
pub(crate) fn create_stream(file: &Path, name: &str) -> std::io::Result<std::fs::File> {
    let mut stream = file.as_os_str().to_owned();
    stream.push(":");
    stream.push(name);
    std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(stream)
}

#[cfg(not(windows))]
pub(crate) fn create_stream(_: &Path, _: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "NTFS alternate data streams are written only on Windows",
    ))
}

#[cfg(windows)]
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "Windows only, where it clears FILE_ATTRIBUTE_READONLY"
)]
pub(crate) fn make_writable(file: &std::fs::File) -> Result<Option<std::fs::Permissions>> {
    let locked = file.metadata()?.permissions();
    if !locked.readonly() {
        return Ok(None);
    }
    let mut writable = locked.clone();
    writable.set_readonly(false);
    file.set_permissions(writable)?;
    Ok(Some(locked))
}

#[cfg(not(windows))]
pub(crate) fn make_writable(_: &std::fs::File) -> Result<Option<std::fs::Permissions>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_names_that_windows_would_change_or_refuse_are_refused() {
        let longest = "x".repeat(255);
        let longest_pairs = "\u{1F600}".repeat(127);
        for good in [
            "Zone.Identifier",
            "second",
            "a b",
            "$DATA",
            "CONSOLE",
            "COM10",
            "LPTX",
            "é名",
            &longest,
            &longest_pairs,
        ] {
            assert_eq!(stream_name_problem(good), None, "{good:?}");
        }
        let long = "x".repeat(256);
        let long_pairs = "\u{1F600}".repeat(128);
        let mut bad = vec![
            "",
            &long,
            &long_pairs,
            "x.",
            "x ",
            ".",
            "con",
            "NUL.txt",
            "Aux ",
            "com1",
            "LPT9.log",
            "COM\u{b9}",
            "conout$",
            "a\u{7f}b",
        ];
        let controls: Vec<String> = (0..0x20u8).map(|c| format!("a{}b", c as char)).collect();
        let forbidden: Vec<String> = "\\/:*?\"<>|".chars().map(|c| format!("a{c}b")).collect();
        bad.extend(controls.iter().map(String::as_str));
        bad.extend(forbidden.iter().map(String::as_str));
        for name in bad {
            assert!(stream_name_problem(name).is_some(), "{name:?}");
        }
    }
}
