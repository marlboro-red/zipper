#![cfg_attr(not(feature = "all-formats"), allow(dead_code, unused_imports))]
#[cfg(feature = "dmg")]
mod adc;
#[cfg(feature = "tar")]
mod base64;
#[cfg(feature = "7z")]
mod bcj2;
#[cfg(any(feature = "7z", feature = "tar"))]
mod bzip2_streams;
pub mod compat;
pub mod counters;
#[cfg(any(feature = "tar", feature = "xar"))]
mod dictionary;
#[cfg(feature = "disk")]
pub mod disk;
#[cfg(feature = "disk")]
mod disk_qcow;
#[cfg(feature = "disk")]
mod disk_sparse;
#[cfg(feature = "disk")]
mod disk_vdi;
#[cfg(feature = "disk")]
mod disk_vmdk;
pub mod duplicates;
mod extract;
#[cfg(any(feature = "apfs", feature = "dmg", feature = "disk"))]
mod hfs;
#[cfg(any(
    feature = "apfs",
    feature = "cramfs",
    feature = "ext",
    feature = "squashfs"
))]
mod image;
#[cfg(feature = "zip")]
mod implode;
#[cfg(feature = "zip")]
mod inflate;
#[cfg(any(feature = "tar", feature = "squashfs"))]
mod lz4;
#[cfg(feature = "dmg")]
mod lzfse;
#[cfg(any(feature = "lzh", feature = "arj"))]
mod lzh;
mod lzma;
#[cfg(feature = "tar")]
mod lzma86;
#[cfg(feature = "wim")]
mod lzms;
#[cfg(feature = "ntfs")]
mod lznt1;
#[cfg(feature = "squashfs")]
mod lzo;
#[cfg(any(feature = "apfs", feature = "dmg", feature = "disk"))]
mod lzvn;
#[cfg(feature = "tar")]
mod lzw;
#[cfg(any(feature = "cab", feature = "chm", feature = "wim"))]
mod lzx;
mod memory;
mod metadata;
#[cfg(feature = "tar")]
mod mslz;
#[cfg(feature = "7z")]
pub mod native_7z;
#[cfg(feature = "apfs")]
pub mod native_apfs;
#[cfg(feature = "ar")]
pub mod native_ar;
#[cfg(feature = "arj")]
pub mod native_arj;
#[cfg(feature = "cab")]
pub mod native_cab;
#[cfg(feature = "chm")]
pub mod native_chm;
#[cfg(feature = "cpio")]
pub mod native_cpio;
#[cfg(feature = "cramfs")]
pub mod native_cramfs;
#[cfg(feature = "dmg")]
pub mod native_dmg;
#[cfg(feature = "exe")]
pub mod native_exe;
#[cfg(feature = "ext")]
pub mod native_ext;
#[cfg(feature = "disk")]
mod native_fat;
#[cfg(feature = "flv")]
pub mod native_flv;
#[cfg(feature = "iso")]
pub mod native_iso;
#[cfg(feature = "lzh")]
pub mod native_lzh;
#[cfg(feature = "msi")]
pub mod native_msi;
#[cfg(feature = "ntfs")]
pub mod native_ntfs;
#[cfg(feature = "rpm")]
pub mod native_rpm;
#[cfg(feature = "squashfs")]
pub mod native_squashfs;
#[cfg(feature = "tar")]
pub mod native_stream;
#[cfg(feature = "swf")]
pub mod native_swf;
#[cfg(feature = "tar")]
pub mod native_tar;
#[cfg(feature = "wim")]
pub mod native_wim;
#[cfg(feature = "xar")]
pub mod native_xar;
#[cfg(feature = "zip")]
pub mod native_zip;
mod ntfs_streams;
#[cfg(any(feature = "flv", feature = "swf"))]
mod output_name;
mod pattern;
mod placement;
#[cfg(feature = "tar")]
mod pmd;
mod progress;
#[cfg(feature = "cab")]
mod quantum;
#[cfg(any(feature = "zip", feature = "7z"))]
mod scrub;
#[cfg(feature = "7z")]
mod sevenz_crypto;
#[cfg(feature = "zip")]
mod shrink;
mod staging;
mod tree;
#[cfg(feature = "iso")]
mod udf;
mod volumes;
#[cfg(any(feature = "7z", feature = "tar"))]
mod writer_pool;
mod xattr;
#[cfg(feature = "wim")]
mod xpress;
mod xz_filters;
#[cfg(feature = "zip")]
mod zip_crypto;
#[cfg(feature = "zip")]
mod zip_directory;
#[cfg(feature = "zip")]
mod zip_reader;
use anyhow::{Context, Result, ensure};
pub use extract::{
    Engine, ExtractOptions, NotBuilt, Reader, choose_reader, extract_archive, read_volumes,
};
pub use metadata::Metadata;
pub use placement::Strip;
pub use progress::{Cancelled, Control, Phase, Progress};
pub use staging::Existing;
use std::{fs::File, io::Read, path::Path};

fn number<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
// EXTERNAL(Kani 0.68): building an anyhow error reaches catch_unwind, which crashes Kani's
// compiler, so the field readers Kani proves return this type and convert at the boundary.
#[derive(Debug)]
pub(crate) struct BadField(pub(crate) &'static str);

impl std::fmt::Display for BadField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for BadField {}

#[derive(Debug)]
pub(crate) struct OverLimit(pub(crate) String);

impl std::fmt::Display for OverLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OverLimit {}

#[derive(Debug)]
pub(crate) struct DestinationRefused(pub(crate) String);

impl std::fmt::Display for DestinationRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DestinationRefused {}

pub(crate) type Field<T> = std::result::Result<T, BadField>;

pub(crate) fn field_check(holds: bool, message: &'static str) -> Field<()> {
    if holds {
        Ok(())
    } else {
        Err(BadField(message))
    }
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
        ensure_plain_windows_name(component)?;
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_plain_windows_name(component: &str) -> Result<()> {
    ensure!(!component.ends_with(['.', ' ']), "ambiguous Windows path");
    ensure!(
        !windows_device_name(component),
        "Windows device path is not a regular archive entry"
    );
    Ok(())
}

// LINK: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
pub(crate) fn windows_device_name(component: &str) -> bool {
    let base = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|number| {
            number.len() == 1 && number.as_bytes()[0].is_ascii_digit()
                || matches!(number, "¹" | "²" | "³")
        })
    })
}

/// Input for a decoder that treats an early end of input as a clean finish:
/// it records whether the decoder ever asked past the last byte.
pub(crate) struct EndWatched<R> {
    pub(crate) inner: R,
    pub(crate) ran_out: bool,
}

impl<R: std::io::Read> std::io::Read for EndWatched<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.ran_out |= read == 0 && !buffer.is_empty();
        Ok(read)
    }
}

pub(crate) fn dos_local_time(date: u16, time: u16) -> Option<std::time::SystemTime> {
    let fields = [
        1980 + i64::from(date >> 9),
        i64::from((date >> 5) & 15),
        i64::from(date & 31),
        i64::from(time >> 11),
        i64::from((time >> 5) & 63),
        i64::from(time & 31) * 2,
    ];
    let [_, month, day, hour, minute, second] = fields;
    let valid = (1..=12).contains(&month) && day >= 1 && hour <= 23 && minute <= 59 && second <= 59;
    let seconds = local_seconds(valid.then_some(fields)?)?;
    let offset = std::time::Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        std::time::UNIX_EPOCH.checked_add(offset)
    } else {
        std::time::UNIX_EPOCH.checked_sub(offset)
    }
}

#[cfg(unix)]
fn local_seconds([year, month, day, hour, minute, second]: [i64; 6]) -> Option<i64> {
    // SAFETY: tm is plain data, and zeroed is a valid value for every field.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (year - 1900) as i32;
    tm.tm_mon = (month - 1) as i32;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour as i32;
    tm.tm_min = minute as i32;
    tm.tm_sec = second as i32;
    tm.tm_isdst = -1;
    // SAFETY: mktime reads and normalizes the tm value it is given.
    let seconds = unsafe { libc::mktime(&mut tm) };
    (seconds != -1).then_some(seconds as i64)
}

#[cfg(windows)]
fn local_seconds([year, month, day, hour, minute, second]: [i64; 6]) -> Option<i64> {
    use windows_sys::Win32::{
        Foundation::SYSTEMTIME, System::Time::TzSpecificLocalTimeToSystemTimeEx,
    };
    let field = |value: i64| u16::try_from(value).ok();
    let local = SYSTEMTIME {
        wYear: field(year)?,
        wMonth: field(month)?,
        wDayOfWeek: 0,
        wDay: field(day)?,
        wHour: field(hour)?,
        wMinute: field(minute)?,
        wSecond: field(second)?,
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME::default();
    // SAFETY: both pointers refer to live SYSTEMTIME values for the call, and
    // a null time zone pointer is allowed and selects the current zone.
    let converted =
        unsafe { TzSpecificLocalTimeToSystemTimeEx(std::ptr::null(), &local, &mut utc) };
    (converted != 0).then(|| {
        let (year, month) = (i64::from(utc.wYear), i64::from(utc.wMonth));
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + i64::from(utc.wDay) - 1;
        let days = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
            + day_of_year
            - 719_468;
        days * 86_400
            + i64::from(utc.wHour) * 3_600
            + i64::from(utc.wMinute) * 60
            + i64::from(utc.wSecond)
    })
}

#[cfg(not(any(unix, windows)))]
fn local_seconds(_: [i64; 6]) -> Option<i64> {
    None
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

const DOS_READ_ONLY: u32 = 0x01;
const DOS_HIDDEN: u32 = 0x02;
const DOS_SYSTEM: u32 = 0x04;

fn windows_attributes(dos: u32, mode: Option<u32>) -> u32 {
    let unwritable = mode.is_some_and(|mode| mode & 0o777 != 0 && mode & 0o222 == 0);
    dos & (DOS_READ_ONLY | DOS_HIDDEN | DOS_SYSTEM) | if unwritable { DOS_READ_ONLY } else { 0 }
}

fn create_new_file(
    path: &Path,
    mode_masked_by_umask: u32,
    windows_attributes: u32,
) -> std::io::Result<File> {
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode_masked_by_umask);
    #[cfg(not(unix))]
    let _ = mode_masked_by_umask;
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::attributes(&mut options, windows_attributes);
    #[cfg(not(windows))]
    let _ = windows_attributes;
    options.open(path)
}

fn set_modified_best_effort(file: &File, time: std::time::SystemTime) {
    let _ = file.set_modified(time);
    counters::add(counters::Counter::MetadataCalls, 1);
}

pub(crate) const MAX_LINK_TARGET: u64 = 4096;

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn windows_link_target(target: &str) -> String {
    target
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join("\\")
}

fn create_symlink(format: &str, root: &Path, name: &str, target: &str) -> Result<()> {
    let link = root.join(name);
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(target, &link);
    #[cfg(windows)]
    let created = {
        let target = windows_link_target(target);
        let resolved = parent(&link).join(&target);
        if std::fs::metadata(resolved).is_ok_and(|metadata| metadata.is_dir()) {
            std::os::windows::fs::symlink_dir(&target, &link)
        } else {
            std::os::windows::fs::symlink_file(&target, &link)
        }
    };
    #[cfg(windows)]
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
    created.map_err(|error| match error.raw_os_error() {
        _ if error.kind() == std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(
            "{format} symlink {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
        ),
        #[cfg(windows)]
        Some(ERROR_PRIVILEGE_NOT_HELD) => anyhow::anyhow!(
            "{format} symlink {name:?} needs Windows Developer Mode or administrator rights; pass --zip-engine backend to read it with 7-Zip"
        ),
        _ => anyhow::Error::from(error).context(format!("cannot create {format} symlink {name:?}")),
    })?;
    counters::add(counters::Counter::SymlinksCreated, 1);
    Ok(())
}

fn ensure_link_stays_inside(format: &str, name: &str, target: &str) -> Result<()> {
    ensure!(
        !target.is_empty()
            && target.len() as u64 <= MAX_LINK_TARGET
            && !target.contains('\0')
            && !target.starts_with('/'),
        "{format} symlink {name:?} has an unsafe target"
    );
    ensure!(
        !cfg!(windows) || !target.contains(['\\', ':']),
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
            _ => {
                #[cfg(windows)]
                ensure_plain_windows_name(component).map_err(|_| {
                    anyhow::anyhow!("{format} symlink {name:?} has an unsafe target")
                })?;
                descended = true
            }
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

pub(crate) struct Selection<'a> {
    paths: Vec<&'a str>,
    include: Vec<pattern::Pattern>,
    exclude: Vec<pattern::Pattern>,
}

fn under(name: &str, path: &str) -> bool {
    name.strip_prefix(path)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
}

impl<'a> Selection<'a> {
    pub(crate) fn new(paths: &'a [String], include: &[String], exclude: &[String]) -> Result<Self> {
        let paths = paths
            .iter()
            .map(|path| {
                let path = path.trim_end_matches('/');
                safe_path(path).map_err(|error| {
                    match path
                        .strip_prefix("./")
                        .filter(|rest| safe_path(rest).is_ok())
                    {
                        Some(rest) => error.context(format!(
                            "selected paths have no leading ./; select {rest:?}"
                        )),
                        None => error,
                    }
                })?;
                Ok(path)
            })
            .collect::<Result<_>>()?;
        let patterns = |texts: &[String]| {
            texts
                .iter()
                .map(|text| pattern::Pattern::new(text))
                .collect::<Result<Vec<_>>>()
        };
        Ok(Self {
            paths,
            include: patterns(include)?,
            exclude: patterns(exclude)?,
        })
    }

    pub(crate) fn selects(&self, name: &str) -> bool {
        let named = self.paths.is_empty() && self.include.is_empty()
            || self.paths.iter().any(|path| under(name, path))
            || self
                .include
                .iter()
                .any(|pattern| pattern.matches_entry_or_folder_above(name));
        named
            && !self
                .exclude
                .iter()
                .any(|pattern| pattern.matches_entry_or_folder_above(name))
    }

    pub(crate) fn ensure_each_path_matches<'n>(
        &self,
        names: impl Iterator<Item = &'n str> + Clone,
    ) -> Result<()> {
        for path in &self.paths {
            ensure!(
                names.clone().any(|name| under(name, path)),
                "selected path not found: {path}"
            );
        }
        for pattern in &self.include {
            ensure!(
                names
                    .clone()
                    .any(|name| pattern.matches_entry_or_folder_above(name)),
                "no archive entry matches --include {}",
                pattern.text()
            );
        }
        Ok(())
    }
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
        #[cfg(windows)]
        restore_directory_times(staging, directories);
        std::fs::rename(staging, destination)?;
        anyhow::Ok(())
    })();
    if published.is_err() {
        #[cfg(unix)]
        unix_modes::unlock_tree_for_cleanup(staging);
    }
    published
}

#[cfg(windows)]
pub(crate) fn restore_directory_times(root: &Path, directories: &[Directory]) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
    };
    for directory in directories {
        // EXTERNAL(Windows): a folder opens only with backup semantics.
        if let Some(time) = directory.modified
            && let Ok(handle) = std::fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(root.join(directory.name))
        {
            let _ = handle.set_modified(time);
            counters::add(counters::Counter::MetadataCalls, 1);
        }
    }
}

#[cfg(unix)]
pub(crate) mod unix_modes {
    use super::Directory;
    use anyhow::{Context, Result};
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
                let path = staging.join(directory.name);
                fs::set_permissions(&path, Permissions::from_mode(mode & default))
                    .with_context(|| format!("cannot set the mode of {}", path.display()))?;
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
            OverLimit(format!(
                "archive expands to {declared} bytes, over --max-extracted-bytes {limit}"
            ))
        ),
        None => {
            let free = available_bytes(staging)
                .with_context(|| format!("cannot measure free space at {}", staging.display()))?;
            ensure!(
                estimate <= free,
                OverLimit(format!(
                    "archive needs about {estimate} bytes but {free} are free; \
                     pass --max-extracted-bytes to override"
                ))
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

#[cfg(test)]
mod escape_spec {
    pub(super) fn is_plain_relative_name(path: &[u8]) -> bool {
        !path.is_empty()
            && path.iter().all(|byte| !matches!(byte, b'\\' | b':' | 0))
            && path
                .split(|byte| *byte == b'/')
                .all(|component| !matches!(component, b"" | b"." | b".."))
    }

    pub(super) fn stays_inside(name: &[u8], target: &[u8]) -> bool {
        let mut depth = name.iter().filter(|byte| **byte == b'/').count() as i64;
        if target.first() == Some(&b'/') {
            return false;
        }
        for component in target.split(|byte| *byte == b'/') {
            match component {
                b"" | b"." => {}
                b".." => depth -= 1,
                _ => depth += 1,
            }
            if depth < 0 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Component;

    #[test]
    fn every_numbered_port_from_0_to_9_and_superscripts_is_a_device() {
        for prefix in ["COM", "LPT", "com", "Lpt"] {
            for number in ["0", "1", "5", "9", "¹", "²", "³"] {
                for name in [format!("{prefix}{number}"), format!("{prefix}{number}.txt")] {
                    assert!(windows_device_name(&name), "{name}");
                }
            }
            for number in ["", "10", "a", "⁴", "0x"] {
                assert!(
                    !windows_device_name(&format!("{prefix}{number}")),
                    "{prefix}{number}"
                );
            }
        }
        for name in ["CON", "nul.tar.gz", "aux ", "CONIN$", "conout$.log"] {
            assert!(windows_device_name(name), "{name}");
        }
        for name in ["CONSOLE", "NULL", "xCON", "", "."] {
            assert!(!windows_device_name(name), "{name}");
        }
    }

    fn every_string(alphabet: &[u8], longest: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut previous = vec![String::new()];
        for _ in 0..longest {
            previous = previous
                .iter()
                .flat_map(|text| {
                    alphabet
                        .iter()
                        .map(move |byte| format!("{text}{}", *byte as char))
                })
                .collect();
            all.extend(previous.iter().cloned());
        }
        all
    }

    #[test]
    fn the_escape_spec_matches_how_the_os_reads_paths() {
        for path in every_string(b"a/.", 7) {
            if escape_spec::is_plain_relative_name(path.as_bytes()) {
                assert!(
                    Path::new(&path)
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                    "{path:?}"
                );
            }
        }
        let names: Vec<_> = every_string(b"a/", 4)
            .into_iter()
            .filter(|name| escape_spec::is_plain_relative_name(name.as_bytes()))
            .collect();
        for name in &names {
            let link = Path::new(name).parent().unwrap();
            for target in every_string(b"a/.", 6) {
                if escape_spec::stays_inside(name.as_bytes(), target.as_bytes()) {
                    let mut depth = link.components().count() as i64;
                    for component in Path::new(&target).components() {
                        depth += match component {
                            Component::Normal(_) => 1,
                            Component::ParentDir => -1,
                            Component::CurDir => 0,
                            other => panic!("{target:?} has {other:?}"),
                        };
                        assert!(depth >= 0, "{name:?} -> {target:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn windows_link_targets_only_drop_empty_parts_and_swap_separators() {
        for target in every_string(b"a/.", 6) {
            let converted = windows_link_target(&target);
            let kept: Vec<&str> = target.split('/').filter(|part| !part.is_empty()).collect();
            assert_eq!(
                converted
                    .split('\\')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>(),
                kept
            );
            assert!(
                !converted.contains('/') && !converted.contains("\\\\"),
                "{target:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn accepted_link_targets_are_plain_relative_paths_to_windows() {
        for target in every_string(b"a/. \\:", 5) {
            if ensure_link_stays_inside("ZIP", "n", &target).is_err() {
                continue;
            }
            let converted = windows_link_target(&target);
            for component in Path::new(&converted).components() {
                match component {
                    Component::Normal(part) => {
                        let part = part.to_str().unwrap();
                        assert!(!part.ends_with(['.', ' ']), "{target:?} gave {converted:?}");
                    }
                    Component::ParentDir | Component::CurDir => {}
                    other => panic!("{target:?} gave {converted:?} with {other:?}"),
                }
            }
        }
    }

    #[test]
    fn the_checks_agree_with_the_escape_spec_on_every_short_string() {
        let strings = every_string(b"a/.:\\\0", 5);
        for path in &strings {
            if safe_path(path).is_ok() {
                assert!(
                    escape_spec::is_plain_relative_name(path.as_bytes()),
                    "{path:?}"
                );
            }
        }
        for name in every_string(b"a/", 3)
            .iter()
            .filter(|name| safe_path(name).is_ok())
        {
            for target in &strings {
                if ensure_link_stays_inside("ZIP", name, target).is_ok() {
                    assert!(escape_spec::stays_inside(
                        name.as_bytes(),
                        target.as_bytes()
                    ));
                }
            }
        }
    }
}
