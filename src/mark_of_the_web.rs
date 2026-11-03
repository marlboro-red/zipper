use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
fn stream(path: &Path) -> std::ffi::OsString {
    let mut name = path.as_os_str().to_owned();
    name.push(":Zone.Identifier");
    name
}

#[cfg(windows)]
pub(crate) fn archive_mark(archive: &Path) -> Option<Vec<u8>> {
    std::fs::read(stream(archive)).ok()
}

#[cfg(not(windows))]
pub(crate) fn archive_mark(_: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(windows)]
pub(crate) fn mark_extracted_files(root: &Path, zone: &[u8]) -> Result<()> {
    use anyhow::Context;
    if !holds_named_streams(root)? {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        std::fs::write(stream(entry.path()), zone)
            .with_context(|| format!("cannot mark {} as downloaded", entry.path().display()))?;
        std::fs::File::options()
            .write(true)
            .open(entry.path())?
            .set_modified(modified)?;
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
