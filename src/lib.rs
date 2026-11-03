pub mod compat;
pub mod counters;
pub mod duplicates;
mod extract;
pub mod native_tar;
pub mod native_zip;
mod pipeline;
mod progress;
mod staging;
mod v1;
mod v2;
mod zip_reader;
use anyhow::{Context, Result, bail, ensure};
pub use extract::{Engine, ExtractOptions, Reader, choose_reader, extract_archive};
pub use progress::{Cancelled, Control, Progress};
pub use staging::Existing;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

fn number<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn safe_path(s: &str) -> Result<()> {
    ensure!(!s.is_empty() && s.len() <= 4096, "invalid path length");
    ensure!(!s.contains(['\\', ':', '\0']), "unsafe archive path: {s:?}");
    ensure!(
        s.split('/').all(|c| !c.is_empty() && c != "." && c != ".."),
        "unsafe archive path: {s:?}"
    );
    #[cfg(windows)]
    for component in s.split('/') {
        ensure!(!component.ends_with(['.', ' ']), "ambiguous Windows path");
        let base = component
            .split('.')
            .next()
            .unwrap()
            .trim_end_matches(' ')
            .to_uppercase();
        let device = matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        ensure!(
            !device,
            "Windows device path is not a regular archive entry"
        );
    }
    Ok(())
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn create_new_file(path: &Path, mode_masked_by_umask: u32) -> std::io::Result<File> {
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode_masked_by_umask);
    #[cfg(not(unix))]
    let _ = mode_masked_by_umask;
    options.open(path)
}

fn set_modified_best_effort(file: &File, time: std::time::SystemTime) {
    let _ = file.set_modified(time);
    counters::add(counters::Counter::MetadataCalls, 1);
}

pub(crate) const MAX_LINK_TARGET: u64 = 4096;

fn ensure_link_stays_inside(format: &str, name: &str, target: &str) -> Result<()> {
    ensure!(
        !target.is_empty()
            && target.len() as u64 <= MAX_LINK_TARGET
            && !target.contains('\0')
            && !target.starts_with('/'),
        "{format} symlink {name:?} has an unsafe target"
    );
    let mut depth = name.matches('/').count();
    let mut descended = false;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                ensure!(
                    !descended,
                    "{format} symlink {name:?} has `..` after a name"
                );
                depth = depth.checked_sub(1).with_context(|| {
                    format!("{format} symlink {name:?} points outside the destination")
                })?;
            }
            _ => descended = true,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn later_spellings_of_same_directory<'a>(
    root: &Path,
    directories: impl Iterator<Item = Option<&'a str>>,
) -> Result<std::collections::HashSet<usize>> {
    use std::os::unix::fs::MetadataExt;
    let mut claimed = std::collections::HashSet::new();
    let mut merged = std::collections::HashSet::new();
    for (index, name) in directories.enumerate() {
        if let Some(name) = name {
            let metadata = std::fs::metadata(root.join(name))?;
            if !claimed.insert((metadata.dev(), metadata.ino())) {
                merged.insert(index);
            }
        }
    }
    Ok(merged)
}

#[cfg(not(unix))]
fn later_spellings_of_same_directory<'a>(
    _: &Path,
    _: impl Iterator<Item = Option<&'a str>>,
) -> Result<std::collections::HashSet<usize>> {
    Ok(std::collections::HashSet::new())
}

pub(crate) fn checked_selection(paths: &[String]) -> Result<Vec<&str>> {
    paths
        .iter()
        .map(|path| {
            let path = path.trim_end_matches('/');
            safe_path(path)?;
            Ok(path)
        })
        .collect()
}

pub(crate) fn is_selected(name: &str, selection: &[&str]) -> bool {
    selection.is_empty()
        || selection.iter().any(|path| {
            name.strip_prefix(path)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
        })
}

pub(crate) fn ensure_each_selected_path_matches<'a>(
    selection: &[&str],
    names: impl Iterator<Item = &'a str> + Clone,
) -> Result<()> {
    for path in selection {
        ensure!(
            names.clone().any(|name| is_selected(name, &[path])),
            "selected path not found: {path}"
        );
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) struct Directory<'a> {
    pub name: &'a str,
    pub mode: Option<u32>,
    pub modified: Option<std::time::SystemTime>,
}

pub(crate) fn publish(staging: &Path, destination: &Path, directories: &[Directory]) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(destination).is_err(),
        "destination appeared during extraction"
    );
    let published = (|| {
        #[cfg(unix)]
        unix_modes::apply(staging, directories)?;
        #[cfg(not(unix))]
        let _ = directories;
        std::fs::rename(staging, destination)?;
        anyhow::Ok(())
    })();
    if published.is_err() {
        #[cfg(unix)]
        unix_modes::unlock_tree_for_cleanup(staging);
    }
    published
}

#[cfg(unix)]
pub(crate) mod unix_modes {
    use super::Directory;
    use anyhow::Result;
    use rayon::prelude::*;
    use std::{
        fs::{self, File, Permissions},
        os::unix::fs::PermissionsExt,
        path::Path,
    };

    pub(super) fn apply(staging: &Path, directories: &[Directory]) -> Result<()> {
        let default = fs::metadata(staging)?.permissions().mode() & 0o777;
        apply_with_default(staging, directories, default)
    }

    pub(crate) fn apply_with_default(
        staging: &Path,
        directories: &[Directory],
        default: u32,
    ) -> Result<()> {
        let mut ordered: Vec<&Directory> = directories.iter().collect();
        ordered.sort_by_key(|directory| std::cmp::Reverse(directory.name.matches('/').count()));
        restore_times(staging, &ordered)?;
        restore_modes_deepest_first(staging, &ordered, default)
    }

    fn restore_times(staging: &Path, directories: &[&Directory]) -> Result<()> {
        directories
            .par_iter()
            .try_for_each(|directory| -> Result<()> {
                if let Some(time) = directory.modified
                    && let Ok(handle) = File::open(staging.join(directory.name))
                {
                    let _ = handle.set_modified(time);
                    crate::counters::add(crate::counters::Counter::MetadataCalls, 1);
                }
                Ok(())
            })
    }

    fn restore_modes_deepest_first(
        staging: &Path,
        directories: &[&Directory],
        default: u32,
    ) -> Result<()> {
        // RECORD(benchmarks/native-zip/permissions/README.md): new directories have the default mode.
        // EXTERNAL(bsdtar): the umask limits restored modes. Info-ZIP unzip ignores it.
        for directory in directories {
            if let Some(mode) = directory.mode.filter(|mode| mode & default != default) {
                fs::set_permissions(
                    staging.join(directory.name),
                    Permissions::from_mode(mode & default),
                )?;
                crate::counters::add(crate::counters::Counter::MetadataCalls, 1);
            }
        }
        Ok(())
    }
    pub(super) fn unlock_tree_for_cleanup(directory: &Path) {
        if let Ok(metadata) = fs::symlink_metadata(directory) {
            let mode = metadata.permissions().mode() | 0o700;
            let _ = fs::set_permissions(directory, Permissions::from_mode(mode));
        }
        for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                unlock_tree_for_cleanup(&entry.path());
            }
        }
    }
}

const ALLOCATION_UNIT: u64 = 4096;

pub(crate) fn check_output_size<'a>(
    staging: &Path,
    entries: impl IntoIterator<Item = &'a Entry>,
    limit: Option<u64>,
) -> Result<()> {
    let (mut declared, mut estimate) = (0u64, 0u64);
    for entry in entries {
        declared = declared
            .checked_add(entry.size)
            .context("archive output size overflows")?;
        let rounded = entry
            .size
            .div_ceil(ALLOCATION_UNIT)
            .saturating_mul(ALLOCATION_UNIT);
        estimate = estimate.saturating_add(rounded);
    }
    match limit {
        Some(limit) => ensure!(
            declared <= limit,
            "archive expands to {declared} bytes, over --max-extracted-bytes {limit}"
        ),
        None => {
            let free = available_bytes(staging)
                .with_context(|| format!("cannot measure free space at {}", staging.display()))?;
            ensure!(
                estimate <= free,
                "archive needs about {estimate} bytes but {free} are free; \
                 pass --max-extracted-bytes to override"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn available_bytes(path: &Path) -> Result<u64> {
    let stats = rustix::fs::statvfs(path)?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

#[cfg(windows)]
fn available_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call,
    // `free` is a valid u64, and the two other outputs are optional nulls.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    ensure!(ok != 0, "{}", std::io::Error::last_os_error());
    Ok(free)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub directory: bool,
}

pub const MAGIC: &[u8; 8] = b"ZIPPER\0\x02";

/// Native creation settings. Larger blocks can improve compression at the cost
/// of memory and selective-extraction work. Defaults preserve the v2 layout.
#[derive(Clone, Copy, Debug)]
pub struct CreateOptions {
    pub level: i32,
    /// Power of two from 64 KiB through 16 MiB.
    pub block_size: usize,
}
impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            level: 1,
            block_size: 1024 * 1024,
        }
    }
}

pub fn create(output: &Path, inputs: &[PathBuf], level: i32) -> Result<()> {
    create_with_options(
        output,
        inputs,
        CreateOptions {
            level,
            ..CreateOptions::default()
        },
    )
}
pub fn create_with_options(
    output: &Path,
    inputs: &[PathBuf],
    options: CreateOptions,
) -> Result<()> {
    v2::create(output, inputs, options)
}
fn version(path: &Path) -> Result<u8> {
    let magic = number::<8>(&mut File::open(path)?)?;
    ensure!(&magic[..7] == b"ZIPPER\0", "not a Zipper archive");
    Ok(magic[7])
}
/// Validate all payloads, optionally extracting into a new directory.
pub fn read_archive(archive: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    match version(archive)? {
        1 => v1::read_archive(archive, destination, Existing::Refuse),
        2 => v2::read_archive(archive, destination, &[]),
        n => bail!("unsupported Zipper version: {n}"),
    }
}
/// v2 listing checks the index only. Use read_archive to validate payloads.
pub fn list_archive(archive: &Path) -> Result<Vec<Entry>> {
    match version(archive)? {
        1 => v1::read_archive(archive, None, Existing::Refuse),
        2 => v2::list(archive),
        n => bail!("unsupported Zipper version: {n}"),
    }
}
/// Extract selected exact paths or directory subtrees from an indexed v2 archive.
pub fn extract_selected(
    archive: &Path,
    destination: &Path,
    paths: &[String],
) -> Result<Vec<Entry>> {
    extract(archive, destination, paths, None)
}

/// Extract all entries, or the selected paths of a v2 archive. `limit` caps the
/// declared output bytes; without it a v2 archive must fit in free space.
/// v1 archives have no size index and are not checked.
pub fn extract(
    archive: &Path,
    destination: &Path,
    paths: &[String],
    limit: Option<u64>,
) -> Result<Vec<Entry>> {
    extract_with(archive, destination, paths, limit, Existing::Refuse)
}

/// [`extract`] that can also merge into an existing destination.
pub fn extract_with(
    archive: &Path,
    destination: &Path,
    paths: &[String],
    limit: Option<u64>,
    existing: Existing,
) -> Result<Vec<Entry>> {
    match version(archive)? {
        1 if paths.is_empty() => {
            ensure!(
                limit.is_none(),
                "--max-extracted-bytes requires a v2 archive; v1 has no size index"
            );
            v1::read_archive(archive, Some(destination), existing)
        }
        2 => v2::read_archive_limited(archive, Some(destination), paths, limit, existing),
        1 => bail!("selective extraction requires a v2 archive"),
        n => bail!("unsupported Zipper version: {n}"),
    }
}

#[cfg(all(test, unix))]
mod publish_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn locked_staging_is_unlocked_for_cleanup() {
        let staging = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staging.path().join("p/q/r")).unwrap();
        std::fs::write(staging.path().join("p/q/r/file"), b"x").unwrap();
        let lock = |path: &str| {
            std::fs::set_permissions(
                staging.path().join(path),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap()
        };
        lock("p/q/r");
        lock("p/q");
        lock("p");
        unix_modes::unlock_tree_for_cleanup(staging.path());
        std::fs::remove_dir_all(staging.path().join("p")).unwrap();
    }
}
