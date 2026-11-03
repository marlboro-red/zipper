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
