//! Opt-in ZIP extraction of stored and Deflate entries, published only after
//! full verification. See `docs/reference/zip-reader.md` for what it restores.
use crate::zip_directory::{CentralEntry, Directory, decode_name_like_zip_crate};
use crate::{BadField, Field, field_check};
use crate::{
    Entry,
    counters::{self, Counter},
    parent, safe_path,
};
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Item {
    entry: Entry,
    crc: u32,
    compressed: u64,
    method: u16,
    stored_name_if_different: Option<Box<[u8]>>,
    header_start: u64,
    next_record_start: u64,
    mode: Option<u32>,
    windows_attributes: u32,
    modified: Option<i64>,
    symlink: bool,
}

const CROWDED_FOLDER_FILES: usize = 1024;

fn crowded_folder(items: &[Item]) -> bool {
    let mut files_per_folder: HashMap<&str, usize> = HashMap::new();
    items
        .iter()
        .filter(|item| !item.entry.directory)
        .any(|item| {
            let name = &item.entry.name;
            let folder = name.rfind('/').map_or("", |slash| &name[..slash]);
            let files = files_per_folder.entry(folder).or_default();
            *files += 1;
            *files >= CROWDED_FOLDER_FILES
        })
}

fn later_spellings_of_same_directory(root: &Path, items: &[Item]) -> Result<HashSet<usize>> {
    crate::later_spellings_of_same_directory(
        root,
        items
            .iter()
            .map(|item| item.entry.directory.then_some(item.entry.name.as_str())),
    )
}

fn create_directories_in_archive_order(root: &Path, items: &[Item]) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        let path = root.join(&item.entry.name);
        let directory = if item.entry.directory {
            path
        } else {
            parent(&path).to_owned()
        };
        if seen.insert(directory.clone()) {
            fs::create_dir_all(directory)?;
            counters::add(Counter::DirectoryCreateCalls, 1);
        }
    }
    Ok(())
}

fn create_links_after_files(
    staging: Option<&Path>,
    items: &[Item],
    targets: Vec<(usize, Vec<u8>)>,
) -> Result<()> {
    for (index, target) in targets {
        let name = &items[index].entry.name;
        ensure_link_stays_inside(name, &target)?;
        if let Some(root) = staging {
            crate::create_symlink("ZIP", root, name, std::str::from_utf8(&target)?)?;
        }
    }
    Ok(())
}

fn ensure_link_stays_inside(name: &str, target: &[u8]) -> Result<()> {
    let target = std::str::from_utf8(target).context("ZIP symlink target is not UTF-8")?;
    crate::ensure_link_stays_inside("ZIP", name, target)
}

const LOCAL_HEADER: u64 = 30;

impl Item {
    fn stored_name_len(&self) -> usize {
        self.stored_name_if_different.as_ref().map_or(
            self.entry.name.len() + usize::from(self.entry.directory),
            |raw| raw.len(),
        )
    }

    fn stored_name_is(&self, bytes: &[u8]) -> bool {
        match &self.stored_name_if_different {
            Some(raw) => **raw == *bytes,
            None => {
                let (name, slash) = bytes.split_at(self.entry.name.len().min(bytes.len()));
                name == self.entry.name.as_bytes()
                    && slash
                        == if self.entry.directory {
                            b"/" as &[u8]
                        } else {
                            b""
                        }
            }
        }
    }
}

struct Budget(AtomicUsize);

impl Budget {
    fn reserve(&self, units: usize) -> Option<Reservation<'_>> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |available| {
                available.checked_sub(units)
            })
            .ok()
            .map(|_| Reservation {
                budget: self,
                units,
            })
    }
}

struct Reservation<'a> {
    budget: &'a Budget,
    units: usize,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.0.fetch_add(self.units, Ordering::Relaxed);
    }
}

std::thread_local! {
    static EXTRACTIONS_IN_ENTRY: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct EntryOnThisThread;

impl EntryOnThisThread {
    fn enter<T>(extraction: &T) -> Option<Self> {
        let extraction = std::ptr::from_ref(extraction).addr();
        EXTRACTIONS_IN_ENTRY.with_borrow_mut(|extractions| {
            if extractions.contains(&extraction) {
                return None;
            }
            extractions.push(extraction);
            Some(EntryOnThisThread)
        })
    }
}

impl Drop for EntryOnThisThread {
    fn drop(&mut self) {
        EXTRACTIONS_IN_ENTRY.with_borrow_mut(|extractions| extractions.pop());
    }
}

struct Recycled<'a, T> {
    idle: &'a std::sync::Mutex<Vec<T>>,
    value: Option<T>,
}

impl<'a, T> Recycled<'a, T> {
    fn take(idle: &'a std::sync::Mutex<Vec<T>>, create: impl FnOnce() -> T) -> Self {
        let value = idle
            .lock()
            .ok()
            .and_then(|mut idle| idle.pop())
            .unwrap_or_else(create);
        Self {
            idle,
            value: Some(value),
        }
    }
}

impl<T> std::ops::Deref for Recycled<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value.as_ref().unwrap()
    }
}

impl<T> std::ops::DerefMut for Recycled<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value.as_mut().unwrap()
    }
}

impl<T> Drop for Recycled<'_, T> {
    fn drop(&mut self) {
        if let (Some(value), Ok(mut idle)) = (self.value.take(), self.idle.lock()) {
            idle.push(value);
        }
    }
}

type ExtractEntry<'a> = dyn Fn(&mut crate::zip_reader::Reader, &mut [u8], &mut Option<ExactInflater>, usize) -> Result<()>
    + Sync
    + 'a;

fn entry_buffer(size: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(size)
        .context("cannot allocate ZIP entry buffer")?;
    buffer.resize(size, 0);
    Ok(buffer)
}

struct EntryTime {
    exact: Option<u32>,
    ntfs: Option<i64>,
    dos: u32,
}

const EXTENDED_TIMESTAMP: u16 = 0x5455;
const INFOZIP_UNIX: u16 = 0x5855;

// EXTERNAL(Info-ZIP unzip, bsdtar): extended time wins over every other field.
fn exact_time(mut extra: &[u8]) -> Option<u32> {
    let (mut extended, mut unix) = (None, None);
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra.get(4..4 + length)?;
        let seconds = |at: usize| {
            body.get(at..at + 4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        };
        match id {
            EXTENDED_TIMESTAMP if body.first().is_some_and(|flags| flags & 1 != 0) => {
                extended = seconds(1)
            }
            INFOZIP_UNIX => unix = seconds(4),
            _ => {}
        }
        extra = &extra[4 + length..];
    }
    extended.or(unix)
}

const NTFS_TIMES: u16 = 0x000A;
const NTFS_TIMES_TAG: u16 = 0x0001;
const FILETIME_TICKS_PER_SECOND: i64 = 10_000_000;
const UNIX_EPOCH_IN_FILETIME_SECONDS: i64 = 11_644_473_600;

fn ntfs_time(mut extra: &[u8]) -> Option<i64> {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra.get(4..4 + length)?;
        if id == NTFS_TIMES {
            let mut attributes = body.get(4..)?;
            while attributes.len() >= 4 {
                let tag = u16::from_le_bytes([attributes[0], attributes[1]]);
                let size = u16::from_le_bytes([attributes[2], attributes[3]]) as usize;
                let value = attributes.get(4..4 + size)?;
                if tag == NTFS_TIMES_TAG && size >= 8 {
                    let ticks =
                        i64::try_from(u64::from_le_bytes(value[..8].try_into().unwrap())).ok()?;
                    return Some(
                        ticks / FILETIME_TICKS_PER_SECOND - UNIX_EPOCH_IN_FILETIME_SECONDS,
                    );
                }
                attributes = &attributes[4 + size..];
            }
        }
        extra = &extra[4 + length..];
    }
    None
}

fn dos_fields(dos: u32) -> [i64; 6] {
    let (time, date) = (dos & 0xffff, dos >> 16);
    [
        1980 + (date >> 9) as i64,
        ((date >> 5) & 15) as i64,
        (date & 31) as i64,
        (time >> 11) as i64,
        ((time >> 5) & 63) as i64,
        ((time & 31) * 2) as i64,
    ]
}

#[derive(Default)]
struct Clock {
    local_offset_by_dos_hour: HashMap<u32, Option<i64>>,
}

impl Clock {
    fn modified(&mut self, time: &EntryTime) -> Option<i64> {
        let dos_utc = {
            let [year, month, day, hour, minute, second] = dos_fields(time.dos);
            days_since_unix_epoch(year, month.clamp(1, 12), day.max(1)) * 86_400
                + hour * 3_600
                + minute * 60
                + second
        };
        if let Some(raw) = time.exact {
            return Some(reading_nearer_dos_time(raw, dos_utc));
        }
        // EXTERNAL(7-Zip): it reads the NTFS field, which unzip and bsdtar ignore.
        if time.ntfs.is_some() {
            return time.ntfs;
        }
        let [year, month, day, hour, minute, second] = dos_fields(time.dos);
        let base = *self
            .local_offset_by_dos_hour
            .entry(time.dos & 0xffff_f800)
            .or_insert_with(|| mktime_seconds([year, month, day, hour, 0, 0]));
        Some(base? + minute * 60 + second)
    }
}

// EXTERNAL(ZIP writers): the 32-bit time field has no agreed sign.
fn reading_nearer_dos_time(raw: u32, dos_utc: i64) -> i64 {
    let (unsigned, signed) = (raw as i64, raw as i32 as i64);
    if (unsigned - dos_utc).abs() <= (signed - dos_utc).abs() {
        unsigned
    } else {
        signed
    }
}

fn days_since_unix_epoch(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

#[cfg(unix)]
fn mktime_seconds([year, month, day, hour, minute, second]: [i64; 6]) -> Option<i64> {
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
fn mktime_seconds([year, month, day, hour, minute, second]: [i64; 6]) -> Option<i64> {
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
        days_since_unix_epoch(utc.wYear.into(), utc.wMonth.into(), utc.wDay.into()) * 86_400
            + i64::from(utc.wHour) * 3_600
            + i64::from(utc.wMinute) * 60
            + i64::from(utc.wSecond)
    })
}

#[cfg(not(any(unix, windows)))]
fn mktime_seconds(_: [i64; 6]) -> Option<i64> {
    None
}

fn system_time(seconds: i64) -> std::time::SystemTime {
    let offset = std::time::Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        std::time::UNIX_EPOCH + offset
    } else {
        std::time::UNIX_EPOCH - offset
    }
}

struct Opened {
    reader: crate::zip_reader::Reader,
    directory: Directory,
}

fn open(path: &Path, cache: bool) -> Result<Opened> {
    let mut file = BufReader::with_capacity(64 * 1024, File::open(path)?);
    let directory = crate::zip_directory::read_directory(&mut file)?;
    let reader = if cache {
        crate::zip_reader::Reader::cached(file.into_inner(), 8 * 1024 * 1024)?
    } else {
        crate::zip_reader::Reader::new(file.into_inner())?
    };
    Ok(Opened { reader, directory })
}

fn entry_time(entry: &CentralEntry) -> EntryTime {
    EntryTime {
        exact: exact_time(&entry.extra),
        ntfs: ntfs_time(&entry.extra),
        dos: entry.dos_time,
    }
}

fn scan(directory: &Directory, restore_times: bool) -> Result<Vec<Item>> {
    let mut raw_names = HashSet::with_capacity(directory.entries.len());
    ensure!(
        directory
            .entries
            .iter()
            .all(|entry| raw_names.insert(entry.name_raw())),
        "duplicate ZIP path"
    );
    drop(raw_names);
    let mut items = Vec::with_capacity(directory.entries.len());
    let mut names = HashMap::new();
    let mut metadata_bytes = 0usize;
    let mut clock = Clock::default();
    for file in &directory.entries {
        ensure!(
            !file.encrypted(),
            "encrypted ZIP is not supported; pass --zip-engine backend to read it with 7-Zip"
        );
        let symlink = file.is_symlink();
        ensure!(
            !symlink || file.size <= crate::MAX_LINK_TARGET,
            "ZIP symlink target is too long"
        );
        let directory = file.is_dir();
        let unix_mode = file.unix_mode_like_zip_crate();
        if let Some(mode) = unix_mode {
            let kind = mode & 0o170000;
            let expected = match (symlink, directory) {
                (true, _) => 0o120000,
                (false, true) => 0o040000,
                (false, false) => 0o100000,
            };
            ensure!(kind == 0 || kind == expected, "unsupported ZIP file type");
        }
        ensure!(
            matches!(
                file.method,
                STORED | DEFLATE | DEFLATE64 | BZIP2 | LZMA | ZSTANDARD | XZ | PPMD
            ),
            "ZIP compression method is not supported; pass --zip-engine backend to read it with 7-Zip"
        );
        let name = file.name().into_owned();
        let name = if directory {
            name.strip_suffix('/').unwrap_or(&name).to_owned()
        } else {
            name
        };
        safe_path(&name)?;
        ensure!(
            names.insert(name.clone(), directory).is_none(),
            "duplicate ZIP path"
        );
        let decoded_len = name.len() + usize::from(directory);
        let raw = &*file.stored_name;
        let stored_name_if_different = (raw.len() != decoded_len
            || !raw.starts_with(name.as_bytes()))
        .then(|| Box::<[u8]>::from(raw));
        metadata_bytes +=
            name.len() + stored_name_if_different.as_ref().map_or(0, |raw| raw.len()) + 64;
        ensure!(
            metadata_bytes <= 64 * 1024 * 1024,
            "ZIP metadata limit exceeded"
        );
        ensure!(!directory || file.size == 0, "ZIP directory has payload");
        items.push(Item {
            entry: Entry {
                name,
                size: file.size,
                directory,
            },
            crc: file.crc32,
            compressed: file.compressed_size,
            method: file.method,
            stored_name_if_different,
            header_start: file.header_start,
            next_record_start: 0,
            mode: unix_mode.map(|mode| mode & 0o777),
            windows_attributes: crate::windows_attributes(file.external_attributes, unix_mode),
            modified: restore_times
                .then(|| clock.modified(&entry_time(file)))
                .flatten(),
            symlink,
        });
    }
    for item in &items {
        let mut prefix = item.entry.name.as_str();
        while let Some((p, _)) = prefix.rsplit_once('/') {
            ensure!(names.get(p) != Some(&false), "ZIP file used as directory");
            prefix = p;
        }
    }
    assign_record_limits(&mut items, directory.start)?;
    Ok(items)
}

fn assign_record_limits(items: &mut [Item], central_start: u64) -> Result<()> {
    let records: Vec<_> = items
        .iter()
        .map(|item| {
            let span = (LOCAL_HEADER + item.stored_name_len() as u64).checked_add(item.compressed);
            (item.header_start, span)
        })
        .collect();
    for (item, limit) in items
        .iter_mut()
        .zip(record_limits(&records, central_start)?)
    {
        item.next_record_start = limit;
    }
    Ok(())
}

fn record_limits(records: &[(u64, Option<u64>)], central_start: u64) -> Field<Vec<u64>> {
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_unstable_by_key(|&index| records[index].0);
    let first = order
        .first()
        .map_or(central_start, |&index| records[index].0);
    field_check(
        first == 0,
        "ZIP has bytes before its first record, as a self-extractor does; pass --zip-engine backend to read it with 7-Zip",
    )?;
    let ends = order.iter().skip(1).map(|&index| records[index].0);
    let mut limits = vec![0; records.len()];
    for (&index, limit) in order.iter().zip(ends.chain(std::iter::once(central_start))) {
        let (start, span) = records[index];
        field_check(
            span.and_then(|span| start.checked_add(span))
                .is_some_and(|end| end <= limit),
            "overlapping ZIP entries",
        )?;
        limits[index] = limit;
    }
    Ok(limits)
}

fn unicode_path_respells_name(mut extra: &[u8], stored_name: &[u8]) -> Result<bool> {
    let mut renames = false;
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra
            .get(4..4 + length)
            .context("truncated ZIP extra field")?;
        if id == 0x7075 {
            ensure!(
                body.len() >= 5 && std::str::from_utf8(&body[5..]).is_ok(),
                "malformed ZIP Unicode path field"
            );
            let crc = u32::from_le_bytes(body[1..5].try_into().unwrap());
            ensure!(
                body[5..] == *stored_name
                    || (crc == crc32fast::hash(stored_name)
                        && body[5..] == *decode_name_like_zip_crate(stored_name).as_bytes()),
                "ZIP Unicode path field renames an entry"
            );
            renames |= body[5..] != *stored_name;
        }
        extra = &extra[4 + length..];
    }
    Ok(renames)
}

fn single_zip64_field(mut extra: &[u8]) -> Field<Vec<u64>> {
    let mut values = Vec::new();
    let mut seen = false;
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra
            .get(4..4 + length)
            .ok_or(BadField("truncated ZIP extra field"))?;
        if id == 1 {
            field_check(!seen, "ZIP entry has two ZIP64 extra fields")?;
            seen = true;
            values.extend(
                body.chunks_exact(8)
                    .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap())),
            );
        }
        extra = &extra[4 + length..];
    }
    Ok(values)
}

const DESCRIPTOR_SIGNATURE: &[u8; 4] = b"PK\x07\x08";

fn descriptor_layout(tail: u64) -> Field<(bool, bool)> {
    match tail {
        12 => Ok((false, false)),
        16 => Ok((true, false)),
        20 => Ok((false, true)),
        24 => Ok((true, true)),
        _ => Err(BadField("ZIP data descriptor has an unexpected length")),
    }
}

fn descriptor_values(descriptor: &[u8], signature: bool, wide: bool) -> Field<[u64; 3]> {
    field_check(
        !signature || descriptor.starts_with(DESCRIPTOR_SIGNATURE),
        "invalid ZIP data descriptor",
    )?;
    let body = &descriptor[if signature { 4 } else { 0 }..];
    let field = |at: usize, width: usize| {
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&body[at..at + width]);
        u64::from_le_bytes(bytes)
    };
    let width = if wide { 8 } else { 4 };
    Ok([field(0, 4), field(4, width), field(4 + width, width)])
}

struct LocalRecord {
    data_start: u64,
    has_descriptor: bool,
}

fn check_local_header(reader: &mut crate::zip_reader::Reader, item: &Item) -> Result<LocalRecord> {
    reader.seek(SeekFrom::Start(item.header_start))?;
    let header = crate::number::<30>(reader)?;
    let read16 = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let read32 = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    ensure!(&header[..4] == b"PK\x03\x04", "invalid ZIP local header");
    let mut name = vec![0; read16(26) as usize];
    reader.read_exact(&mut name)?;
    let mut extra = vec![0; read16(28) as usize];
    reader.read_exact(&mut extra)?;
    let flags = read16(6);
    ensure!(
        flags & 1 == 0,
        "ZIP local header is encrypted but its central entry is not"
    );
    let method = item.method;
    ensure!(
        item.stored_name_is(&name) && read16(8) == method,
        "ZIP local header disagrees with central directory"
    );
    // EXTERNAL(Info-ZIP zip): writing to a pipe zeroes the local CRC and compressed size.
    let deferred = flags & 8 != 0;
    let agrees = |local: u64, central: u64| {
        local == central || (deferred && (local == 0 || local == u32::MAX as u64))
    };
    let (crc, mut compressed, mut size) = (read32(14), read32(18) as u64, read32(22) as u64);
    // SPEC(APPNOTE 4.5.3): the local ZIP64 field holds both sizes, original first.
    unicode_path_respells_name(&extra, &name)?;
    let zip64 = single_zip64_field(&extra)?;
    let marked = |value: u64| value == u32::MAX as u64;
    let (original, packed) = match zip64[..] {
        [original, packed, ..] => (Some(original), Some(packed)),
        [only] if marked(size) => (Some(only), None),
        [only] => (None, Some(only)),
        [] => (None, None),
    };
    if let (true, Some(value)) = (marked(size), original) {
        size = value;
    }
    if let (true, Some(value)) = (marked(compressed), packed) {
        compressed = value;
    }
    ensure!(
        agrees(crc as u64, item.crc as u64)
            && agrees(compressed, item.compressed)
            && agrees(size, item.entry.size)
            && original.is_none_or(|value| agrees(value, item.entry.size))
            && packed.is_none_or(|value| agrees(value, item.compressed)),
        "ZIP local header disagrees with central directory"
    );
    let data_start = item.header_start + LOCAL_HEADER + name.len() as u64 + extra.len() as u64;
    let data_end = data_start
        .checked_add(item.compressed)
        .filter(|&end| end <= item.next_record_start)
        .context("overlapping ZIP entries")?;
    let tail = item.next_record_start - data_end;
    if !deferred {
        ensure!(tail == 0, "ZIP has bytes between records");
        return Ok(LocalRecord {
            data_start,
            has_descriptor: false,
        });
    }
    let (signature, wide) = descriptor_layout(tail)?;
    reader.seek(SeekFrom::Start(data_end))?;
    let mut descriptor = vec![0; tail as usize];
    reader.read_exact(&mut descriptor)?;
    ensure!(
        descriptor_values(&descriptor, signature, wide)?
            == [item.crc as u64, item.compressed, item.entry.size],
        "ZIP data descriptor disagrees with central directory"
    );
    Ok(LocalRecord {
        data_start,
        has_descriptor: true,
    })
}

struct ExactInflater(std::ptr::NonNull<libdeflate_sys::libdeflate_decompressor>);

impl ExactInflater {
    fn new() -> Self {
        // SAFETY: the allocator takes no arguments and returns null on failure.
        let decompressor = unsafe { libdeflate_sys::libdeflate_alloc_decompressor() };
        Self(std::ptr::NonNull::new(decompressor).expect("libdeflate decompressor allocation"))
    }

    fn inflate(&mut self, input: &[u8], output: &mut [u8]) -> Result<usize> {
        let (mut read, mut written) = (0, 0);
        // SAFETY: the decompressor is live until drop and used by one thread.
        // Both slices are valid for their stated lengths, and libdeflate writes
        // at most `output.len()` bytes.
        let result = unsafe {
            libdeflate_sys::libdeflate_deflate_decompress_ex(
                self.0.as_ptr(),
                input.as_ptr().cast(),
                input.len(),
                output.as_mut_ptr().cast(),
                output.len(),
                &mut read,
                &mut written,
            )
        };
        ensure!(
            result == libdeflate_sys::libdeflate_result_LIBDEFLATE_SUCCESS,
            "invalid ZIP Deflate data"
        );
        ensure!(
            read == input.len(),
            "ZIP Deflate stream ends before its compressed size"
        );
        Ok(written)
    }
}

impl Drop for ExactInflater {
    fn drop(&mut self) {
        // SAFETY: the pointer came from libdeflate_alloc_decompressor and is
        // freed exactly once.
        unsafe { libdeflate_sys::libdeflate_free_decompressor(self.0.as_ptr()) }
    }
}

#[derive(Default)]
struct HiddenDescriptorScan {
    pending: Vec<u8>,
    crc_before_pending: crc32fast::Hasher,
    pending_offset: u64,
}

const SIGNATURE_LEN: usize = 4;
const WIDE_DESCRIPTOR_LEN: usize = SIGNATURE_LEN + 4 + 2 * 8;
const DESCRIPTOR_SCAN_PIECE: usize = 4096;

impl HiddenDescriptorScan {
    fn feed(&mut self, chunk: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(chunk);
        let mut retain_from = self.pending.len().saturating_sub(SIGNATURE_LEN - 1);
        let mut scanned = 0;
        while let Some(found) = self.pending[scanned..]
            .windows(4)
            .position(|bytes| bytes == b"PK\x07\x08")
        {
            let at = scanned + found;
            let fields = &self.pending[at + SIGNATURE_LEN..];
            if fields.len() < WIDE_DESCRIPTOR_LEN - SIGNATURE_LEN {
                retain_from = at;
                break;
            }
            let mut crc = self.crc_before_pending.clone();
            crc.update(&self.pending[..at]);
            let (crc, length) = (crc.finalize() as u64, self.pending_offset + at as u64);
            let narrow =
                |at: usize| u32::from_le_bytes(fields[at..at + 4].try_into().unwrap()) as u64;
            let wide = |at: usize| u64::from_le_bytes(fields[at..at + 8].try_into().unwrap());
            let describes_prefix = narrow(0) == crc
                && ((narrow(4), narrow(8)) == (length, length)
                    || (wide(4), wide(12)) == (length, length));
            ensure!(
                !describes_prefix,
                "stored ZIP entry contains a data descriptor for its own prefix"
            );
            scanned = at + 1;
        }
        self.crc_before_pending.update(&self.pending[..retain_from]);
        self.pending_offset += retain_from as u64;
        self.pending.drain(..retain_from);
        Ok(())
    }
}

struct OutputFile(File);

impl Write for OutputFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.0.write(bytes)?;
        counters::add(Counter::OutputWrites, 1);
        counters::add(Counter::OutputBytesWritten, written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

fn finish(output: &mut BufWriter<OutputFile>, item: &Item) -> Result<()> {
    output.flush()?;
    if let Some(seconds) = item.modified {
        crate::set_modified_best_effort(&output.get_ref().0, system_time(seconds));
    }
    Ok(())
}

const WRITE_BEHIND_MIN: u64 = 16 * 1024 * 1024;
const WRITE_BEHIND_CHUNK: usize = 1024 * 1024;
const WRITE_BEHIND_DEPTH: usize = 4;

fn write_behind(
    mut output: BufWriter<OutputFile>,
    written: &(dyn Fn(u64) + Sync),
    produce: impl FnOnce(&mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()>,
) -> Result<(BufWriter<OutputFile>, u64)> {
    std::thread::scope(|scope| {
        let (full, filled) = std::sync::mpsc::sync_channel::<Vec<u8>>(WRITE_BEHIND_DEPTH);
        let (empty, emptied) = std::sync::mpsc::sync_channel::<Vec<u8>>(WRITE_BEHIND_DEPTH + 1);
        for _ in 0..WRITE_BEHIND_DEPTH + 1 {
            empty.send(Vec::with_capacity(WRITE_BEHIND_CHUNK))?;
        }
        let writer = scope.spawn(move || -> std::io::Result<(BufWriter<OutputFile>, u64)> {
            let mut held = 0;
            for mut chunk in filled {
                // RECORD(benchmarks/native-zip/windows-write-size/README.md): 256 KiB slices.
                if cfg!(windows) {
                    for slice in chunk.chunks(256 * 1024) {
                        output.write_all(slice)?;
                    }
                } else {
                    output.write_all(&chunk)?;
                }
                written(std::mem::replace(&mut held, chunk.len() as u64));
                chunk.clear();
                let _ = empty.send(chunk);
            }
            Ok((output, held))
        });
        let mut current = emptied.recv()?;
        let mut writer_stopped = false;
        let produced = produce(&mut |mut bytes: &[u8]| {
            while !bytes.is_empty() {
                let room = WRITE_BEHIND_CHUNK - current.len();
                let (now, later) = bytes.split_at(room.min(bytes.len()));
                current.extend_from_slice(now);
                bytes = later;
                if current.len() == WRITE_BEHIND_CHUNK {
                    let next = emptied.recv().inspect_err(|_| writer_stopped = true)?;
                    full.send(std::mem::replace(&mut current, next))
                        .inspect_err(|_| writer_stopped = true)?;
                }
            }
            Ok(())
        });
        if produced.is_ok() && !current.is_empty() {
            let _ = full.send(current);
        }
        drop(full);
        let written = writer
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        match (produced, written) {
            (Ok(()), Ok(output)) => Ok(output),
            (Err(_), Err(io)) if writer_stopped => Err(io.into()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(io)) => Err(io.into()),
        }
    })
}

// SPEC(APPNOTE 4.4.5): compression method numbers.
const STORED: u16 = 0;
const DEFLATE: u16 = 8;
const DEFLATE64: u16 = 9;
const BZIP2: u16 = 12;
const LZMA: u16 = 14;
const ZSTANDARD: u16 = 93;
const XZ: u16 = 95;
const PPMD: u16 = 98;
const PPMD_LARGEST_MODEL: u64 = 256 << 20;
const DECODER_MEMORY_PATIENCE: std::time::Duration = std::time::Duration::from_millis(1000);

fn decoder_need(item: &Item) -> Option<u64> {
    match item.method {
        XZ | LZMA => Some(item.entry.size),
        PPMD => Some(PPMD_LARGEST_MODEL),
        _ => None,
    }
}
const OTHER_METHOD_INPUT: usize = 256 * 1024;
const LZMA_SMALLEST_DICTIONARY: u64 = 4096;
const ZIP_DECODER_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

#[inline(never)]
fn inflate64(
    input: &mut impl Read,
    compressed: u64,
    buffer: &mut [u8],
    mut emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let (input_buffer, outputs) = buffer.split_at_mut(512 * 1024);
    let (output, spare_output) = outputs.split_at_mut(outputs.len() / 2);
    let mut whole = deflate64::InflaterManaged::new();
    let mut without_last_byte = deflate64::InflaterManaged::new();
    let (mut position, mut used) = (0u64, 0u64);
    loop {
        let length = input.read(input_buffer)?;
        if length == 0 {
            break;
        }
        let chunk = &input_buffer[..length];
        used += inflate64_chunk(&mut whole, chunk, output, &mut emit)?;
        let before_last = compressed.saturating_sub(1).saturating_sub(position);
        let shorter = &chunk[..length.min(usize::try_from(before_last).unwrap_or(usize::MAX))];
        inflate64_chunk(&mut without_last_byte, shorter, spare_output, &mut |_| {
            Ok(())
        })?;
        position += length as u64;
    }
    ensure!(
        whole.finished() && used == compressed,
        "truncated ZIP Deflate64 stream"
    );
    ensure!(
        compressed == 0 || !without_last_byte.input_finished(),
        "ZIP Deflate64 stream has data after its end"
    );
    Ok(())
}

fn inflate64_chunk(
    inflater: &mut deflate64::InflaterManaged,
    mut bytes: &[u8],
    output: &mut [u8],
    emit: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<u64> {
    let mut used = 0;
    loop {
        let result = inflater.inflate(bytes, output);
        ensure!(!result.data_error, "invalid ZIP Deflate64 data");
        used += result.bytes_consumed as u64;
        bytes = &bytes[result.bytes_consumed..];
        emit(&output[..result.bytes_written])?;
        if result.bytes_consumed == 0 && result.bytes_written == 0 {
            return Ok(used);
        }
    }
}

fn lzma_alone_header(prefix: [u8; 9], size: u64) -> Field<[u8; 13]> {
    // SPEC(APPNOTE 5.8.8): two version bytes, a two-byte properties size of 5, then the properties.
    field_check(
        u16::from_le_bytes([prefix[2], prefix[3]]) == 5,
        "unsupported ZIP LZMA properties size",
    )?;
    let mut header = [0u8; 13];
    header[0] = prefix[4];
    let dictionary = u32::from_le_bytes([prefix[5], prefix[6], prefix[7], prefix[8]]);
    let needed = u32::try_from(size.max(LZMA_SMALLEST_DICTIONARY)).unwrap_or(u32::MAX);
    header[1..5].copy_from_slice(&dictionary.min(needed).to_le_bytes());
    header[5..].copy_from_slice(&size.to_le_bytes());
    Ok(header)
}

fn ppmd8_fields(fields: u16) -> Field<(u32, u32, u16)> {
    // SPEC(APPNOTE 5.10): order - 1 in bits 0-3, model MiB - 1 in bits 4-11, then the restore method.
    let order = u32::from(fields & 0x0f) + 1;
    let memory = (u32::from((fields >> 4) & 0xff) + 1) << 20;
    let restore = fields >> 12;
    field_check(
        order >= ppmd_rust::PPMD8_MIN_ORDER,
        "ZIP PPMd order is below the smallest",
    )?;
    field_check(restore <= 1, "ZIP PPMd restore method is not supported")?;
    Ok((order, memory, restore))
}

struct EndMarked<R: Read> {
    decoder: ppmd_rust::Ppmd8Decoder<crate::EndWatched<R>>,
    remaining: u64,
}

impl<R: Read> Read for EndMarked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            if self.decoder.read(&mut probe)? > 0 {
                return Err(std::io::Error::other(
                    "ZIP PPMd data goes on past its declared size",
                ));
            }
            if self.decoder.get_ref().ran_out {
                return Err(std::io::Error::other(
                    "ZIP PPMd stream ends before its end marker",
                ));
            }
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.decoder.read(&mut buffer[..wanted])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn method_decoder<'a>(
    item: &Item,
    input: &'a mut std::io::BufReader<&mut dyn Read>,
) -> Result<Box<dyn Read + 'a>> {
    Ok(match item.method {
        BZIP2 => Box::new(bzip2::bufread::BzDecoder::new(input)),
        ZSTANDARD => Box::new(zstd::stream::read::Decoder::with_buffer(input)?.single_frame()),
        XZ => Box::new(liblzma::bufread::XzDecoder::new(input)),
        LZMA => {
            let mut prefix = [0u8; 9];
            input
                .read_exact(&mut prefix)
                .context("truncated ZIP LZMA header")?;
            let header = lzma_alone_header(prefix, item.entry.size)?;
            Box::new(liblzma::bufread::XzDecoder::new_stream(
                std::io::Cursor::new(header).chain(input),
                liblzma::stream::Stream::new_lzma_decoder(u64::MAX)?,
            ))
        }
        PPMD => {
            let mut header = [0u8; 2];
            input
                .read_exact(&mut header)
                .context("truncated ZIP PPMd header")?;
            let (order, memory, restore) = ppmd8_fields(u16::from_le_bytes(header))?;
            let restore = ppmd_rust::RestoreMethod::from(restore);
            let input = crate::EndWatched {
                inner: input,
                ran_out: false,
            };
            let decoder = ppmd_rust::Ppmd8Decoder::new(input, order, memory, restore)
                .map_err(|error| anyhow::anyhow!("cannot start the ZIP PPMd decoder: {error:?}"))?;
            Box::new(EndMarked {
                decoder,
                remaining: item.entry.size,
            })
        }
        other => anyhow::bail!("ZIP compression method {other} is not supported"),
    })
}

fn stream_entry(
    input: &mut impl Read,
    item: &Item,
    descriptor: bool,
    buffer: &mut [u8],
    mut sink: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let mut crc = crc32fast::Hasher::new();
    let mut written = 0u64;
    let mut emit = |bytes: &[u8]| -> Result<()> {
        written = written
            .checked_add(bytes.len() as u64)
            .filter(|&total| total <= item.entry.size)
            .context("ZIP decoded size exceeds metadata")?;
        crc.update(bytes);
        sink(bytes)
    };
    if item.method == STORED {
        ensure!(
            item.compressed == item.entry.size,
            "stored ZIP entry sizes differ"
        );
        let mut scan = descriptor.then(HiddenDescriptorScan::default);
        loop {
            let length = input.read(buffer)?;
            if length == 0 {
                break;
            }
            let chunk = &buffer[..length];
            if let Some(scan) = &mut scan {
                chunk
                    .chunks(DESCRIPTOR_SCAN_PIECE)
                    .try_for_each(|piece| scan.feed(piece))?;
            }
            emit(chunk)?;
        }
    } else if item.method == DEFLATE64 {
        inflate64(input, item.compressed, buffer, emit)?;
    } else if item.method != DEFLATE {
        let input: &mut dyn Read = input;
        let mut input = std::io::BufReader::with_capacity(OTHER_METHOD_INPUT, input);
        let mut decoder = method_decoder(item, &mut input)?;
        loop {
            let length = decoder
                .read(buffer)
                .context("invalid ZIP compressed data")?;
            if length == 0 {
                break;
            }
            emit(&buffer[..length])?;
        }
        drop(decoder);
        ensure!(
            std::io::BufRead::fill_buf(&mut input)?.is_empty(),
            "ZIP compressed stream ends before its compressed size"
        );
    } else {
        let (input_buffer, output_buffer) = buffer.split_at_mut(512 * 1024);
        let mut inflater = flate2::Decompress::new(false);
        let (mut start, mut end) = (0, 0);
        loop {
            if start == end {
                end = input.read(input_buffer)?;
                start = 0;
            }
            let (consumed, produced) = (inflater.total_in(), inflater.total_out());
            let status = counters::time(Counter::DecodeNanos, || {
                inflater.decompress(
                    &input_buffer[start..end],
                    output_buffer,
                    flate2::FlushDecompress::None,
                )
            })?;
            let consumed = (inflater.total_in() - consumed) as usize;
            let produced = (inflater.total_out() - produced) as usize;
            start += consumed;
            emit(&output_buffer[..produced])?;
            if status == flate2::Status::StreamEnd {
                break;
            }
            ensure!(consumed > 0 || produced > 0, "truncated ZIP Deflate stream");
        }
        ensure!(
            inflater.total_in() == item.compressed,
            "ZIP Deflate stream ends before its compressed size"
        );
    }
    ensure!(written == item.entry.size, "ZIP decoded size mismatch");
    ensure!(crc.finalize() == item.crc, "ZIP CRC mismatch");
    Ok(())
}

/// List validated central-directory entries; payload CRCs are not checked.
pub fn list(path: &Path) -> Result<Vec<Entry>> {
    let mut file = BufReader::with_capacity(64 * 1024, File::open(path)?);
    let directory = crate::zip_directory::read_directory(&mut file)?;
    Ok(scan(&directory, false)?
        .into_iter()
        .map(|item| item.entry)
        .collect())
}

/// Verify and optionally extract a stored/Deflate ZIP. It restores files,
/// directories, Unix modes, modification times and symlinks that stay inside
/// the destination. The archive and destination parent must not be modified
/// concurrently.
pub fn read_archive(path: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    read_archive_with_options(path, destination, ReadOptions::default())
}

/// Whole-buffer Deflate fast path for eligible entries whose compressed and
/// decoded sizes fit the shared budget. Large, highly compressible extraction
/// entries and entries that cannot reserve memory use bounded streaming.
/// Integrity-only reads and Linux extraction also cache archives up to 8 MiB.
pub fn read_archive_fast(path: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    read_archive_with_options(
        path,
        destination,
        ReadOptions {
            fast: true,
            ..ReadOptions::default()
        },
    )
}

/// Native ZIP decoding and concurrency options.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions<'a> {
    /// Use the bounded whole-buffer libdeflate path for eligible entries.
    pub fast: bool,
    /// Override automatic concurrency with a maximum of 1–64 workers.
    /// The caller's Rayon pool remains an additional upper bound.
    pub threads: Option<usize>,
    /// Shared compressed + decoded entry-buffer budget. Default: 128 MiB on
    /// ARM64 macOS, 64 MiB elsewhere.
    /// Zero forces streaming. Metadata, worker state and archive cache are extra.
    pub buffer_bytes: Option<usize>,
    /// Cap on declared output bytes. Without it, extraction must fit in the
    /// destination filesystem's free space.
    pub max_output_bytes: Option<u64>,
    /// What to do when the destination exists.
    pub existing: crate::Existing,
    /// Exact entry paths or folder subtrees to read. Empty reads every entry.
    /// Each path must name an entry or a folder that holds one.
    pub paths: &'a [String],
    /// Wildcard patterns to read, written as for `ExtractOptions::include`.
    pub include: &'a [String],
    /// Wildcard patterns to leave out, written as for `ExtractOptions::include`.
    pub exclude: &'a [String],
    /// Leading path segments to drop before writing, as [`crate::Strip`]
    /// describes. Selection still matches the full archive paths.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents, as [`crate::Metadata`]
    /// describes.
    pub metadata: crate::Metadata,
    /// Progress reports and cancellation.
    pub control: crate::Control<'a>,
}

pub fn read_archive_with_options(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    ensure!(
        options.threads.is_none_or(|n| (1..=64).contains(&n)),
        "ZIP threads must be 1..64"
    );
    let fast = options.fast;
    let decoder_memory = crate::memory::Memory::new(ZIP_DECODER_MEMORY);
    let budget = Budget(AtomicUsize::new(options.buffer_bytes.unwrap_or(
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            128 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        },
    )));
    // RECORD(benchmarks/native-zip/linux-cache/README.md): Linux extraction caches archives up to 8 MiB.
    let Opened { reader, directory } = open(
        path,
        fast && (destination.is_none() || cfg!(target_os = "linux")),
    )?;
    let items = scan(&directory, true)?;
    drop(directory);
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    selection.ensure_each_path_matches(items.iter().map(|item| item.entry.name.as_str()))?;
    let placement = crate::placement::Placement::new(options.strip)?;
    placement.ensure_prefix_is_a_folder(
        items
            .iter()
            .map(|item| (item.entry.name.as_str(), item.entry.directory)),
    )?;
    let (mut selected, mut skipped): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| selection.selects(&item.entry.name));
    if placement.strips() {
        let prefix_selected = selected
            .iter()
            .any(|item| placement.is_prefix(&item.entry.name));
        let (placed, unplaced): (Vec<_>, Vec<_>) = selected
            .into_iter()
            .partition(|item| placement.place(&item.entry.name).is_some());
        selected = placed;
        skipped.extend(unplaced);
        placement.ensure_something_placed(!selected.is_empty() || prefix_selected)?;
        for item in &mut selected {
            let placed = placement
                .place(&item.entry.name)
                .expect("placed")
                .to_owned();
            if item.stored_name_if_different.is_none() {
                let mut stored = std::mem::take(&mut item.entry.name).into_bytes();
                if item.entry.directory {
                    stored.push(b'/');
                }
                item.stored_name_if_different = Some(stored.into_boxed_slice());
            }
            item.entry.name = placed;
        }
        crate::placement::ensure_placed_names_fit(
            selected
                .iter()
                .map(|item| (item.entry.name.as_str(), item.entry.directory)),
        )?;
    }
    let metadata = options.metadata;
    for item in &mut selected {
        item.mode = if item.entry.directory {
            metadata.directory_mode(item.mode)
        } else {
            metadata.file_mode(item.mode)
        };
        item.windows_attributes = metadata.windows_attributes(item.windows_attributes);
        item.modified = metadata.time(item.modified);
    }
    let mut headers = reader.clone();
    for item in &skipped {
        check_local_header(&mut headers, item)?;
    }
    let items = selected;
    let tracker = crate::progress::Tracker::new(
        options.control,
        Some(items.iter().filter(|item| !item.entry.directory).count() as u64),
        Some(items.iter().map(|item| item.entry.size).sum()),
    );
    tracker.check()?;
    let finished = |item: &Item| {
        if item.entry.directory {
            tracker.check()
        } else {
            tracker.file_done()
        }
    };
    let staging = match destination {
        Some(destination) => {
            let staging =
                crate::staging::stage(path, destination, options.existing, options.metadata)?;
            crate::check_output_size(
                staging.path(),
                items.iter().map(|item| &item.entry),
                options.max_output_bytes,
            )?;
            Some(staging)
        }
        None => None,
    };
    if let Some(temp) = &staging {
        create_directories_in_archive_order(temp.path(), &items)?;
    }
    let merged = match &staging {
        Some(temp) => later_spellings_of_same_directory(temp.path(), &items)?,
        None => HashSet::new(),
    };
    let mut largest_first: Vec<_> = (0..items.len()).collect();
    largest_first.sort_unstable_by_key(|&index| std::cmp::Reverse(items[index].entry.size));
    let tiny_files = staging.is_some()
        && items.iter().filter(|item| !item.entry.directory).count() >= 1024
        && items.iter().all(|item| item.entry.size <= 16 * 1024);
    // RECORD(benchmarks/native-zip/bsdtar-comparison.md): macOS caps tiny-file extraction workers.
    // RECORD(benchmarks/cross-platform/scoreboard-2026-11-02/README.md): one Windows folder serializes creation.
    let automatic_limit = if tiny_files && cfg!(target_os = "macos") {
        4
    } else if tiny_files && cfg!(windows) && crowded_folder(&items) {
        1
    } else {
        64
    };
    let worker_limit = options.threads.unwrap_or(automatic_limit);
    let pool_threads = rayon::current_num_threads().min(worker_limit);
    let entries_with_payload = items.iter().filter(|item| !item.entry.directory).count();
    let spare_threads = Budget(AtomicUsize::new(
        pool_threads.saturating_sub(entries_with_payload),
    ));
    let targets = std::sync::Mutex::new(Vec::new());
    let for_the_next_pass = std::sync::Mutex::new(Vec::new());
    let idle_workspaces = std::sync::Mutex::new(Vec::new());
    let extract_entry: &ExtractEntry = &|raw, buffer, codec, index| -> Result<()> {
        tracker.check()?;
        let item = &items[index];
        let _decoder = match decoder_need(item) {
            Some(need) => match decoder_memory.reserve_within(need, DECODER_MEMORY_PATIENCE) {
                Some(reservation) => Some(reservation),
                None => {
                    for_the_next_pass
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push(index);
                    return Ok(());
                }
            },
            None => None,
        };
        let LocalRecord {
            data_start,
            has_descriptor: descriptor,
        } = check_local_header(raw, item)?;
        // RECORD(benchmarks/native-zip/decoder-selection/README.md): large, compressible entries stream.
        let prefer_streaming = destination.is_some()
            && item.entry.size >= 16 * 1024 * 1024
            && item.compressed <= item.entry.size / 128;
        let reservation = if fast && !prefer_streaming && !item.symlink && item.method == DEFLATE {
            item.entry
                .size
                .checked_add(item.compressed)
                .and_then(|bytes| usize::try_from(bytes).ok())
                .and_then(|bytes| budget.reserve(bytes))
        } else {
            None
        };
        let buffered = reservation.is_some();
        raw.seek(SeekFrom::Start(data_start))?;
        let mut source = Read::take(&mut *raw, item.compressed);
        if item.symlink {
            let mut target = Vec::new();
            stream_entry(&mut source, item, descriptor, buffer, |bytes| {
                target.extend_from_slice(bytes);
                Ok(())
            })?;
            tracker.add_bytes(target.len() as u64)?;
            targets.lock().unwrap().push((index, target));
            return finished(item);
        }
        let mut output = if let Some(temp) = &staging {
            if !item.entry.directory {
                let path = temp.path().join(&item.entry.name);
                let file = crate::create_new_file(&path, item.mode.unwrap_or(0o666), item.windows_attributes)
                            .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::AlreadyExists {
                                anyhow::anyhow!(
                                    "ZIP entry {:?} collides with another entry on this filesystem, which ignores case or Unicode normalization",
                                    item.entry.name
                                )
                            } else {
                                error.into()
                            }
                        })?;
                counters::add(Counter::FilesCreated, 1);
                Some(BufWriter::new(OutputFile(file)))
            } else {
                None
            }
        } else {
            None
        };
        if buffered {
            let mut compressed = entry_buffer(item.compressed as usize)?;
            let mut decoded = entry_buffer(item.entry.size as usize)?;
            source.read_exact(&mut compressed)?;
            let length = counters::time(Counter::DecodeNanos, || {
                codec.as_mut().unwrap().inflate(&compressed, &mut decoded)
            })?;
            ensure!(
                length as u64 == item.entry.size,
                "ZIP decoded size mismatch"
            );
            ensure!(crc32fast::hash(&decoded) == item.crc, "ZIP CRC mismatch");
            if let Some(output) = &mut output {
                // RECORD(benchmarks/native-zip/windows-write-size/README.md): 256 KiB slices.
                if cfg!(windows) {
                    for chunk in decoded.chunks(256 * 1024) {
                        output.write_all(chunk)?;
                    }
                } else {
                    output.write_all(&decoded)?;
                }
                finish(output, item)?;
            }
            drop(output);
            tracker.add_bytes(item.entry.size)?;
            return finished(item);
        }
        if item.entry.size >= WRITE_BEHIND_MIN
            && let Some(writer_thread) = spare_threads.reserve(1)
            && let Some(unwritten) = output.take()
        {
            let report_written = |bytes| tracker.count_bytes(bytes);
            let (mut written, held) = write_behind(unwritten, &report_written, |sink| {
                stream_entry(&mut source, item, descriptor, buffer, |bytes| {
                    tracker.report()?;
                    sink(bytes)
                })
            })?;
            drop(writer_thread);
            finish(&mut written, item)?;
            drop(written);
            tracker.add_bytes(held)?;
            return finished(item);
        }
        let mut held = 0;
        stream_entry(&mut source, item, descriptor, buffer, |bytes| {
            if bytes.is_empty() {
                return Ok(());
            }
            if let Some(output) = &mut output {
                output.write_all(bytes)?;
            }
            tracker.add_bytes(std::mem::replace(&mut held, bytes.len() as u64))
        })?;
        if let Some(output) = &mut output {
            finish(output, item)?;
        }
        drop(output);
        tracker.add_bytes(held)?;
        finished(item)
    };
    let run_pass = |entries: &[usize]| {
        // RECORD(benchmarks/native-zip/one-entry-per-task/README.md): split to single entries,
        // and reuse each task's reader and buffer, which tiny entries cannot afford per task.
        entries.par_iter().with_max_len(1).try_for_each_init(
            || {
                let workspace = Recycled::take(&idle_workspaces, || {
                    (reader.clone(), vec![0u8; 1024 * 1024])
                });
                (workspace, fast.then(ExactInflater::new))
            },
            |(workspace, codec), &index| match EntryOnThisThread::enter(&for_the_next_pass) {
                Some(_entry) => {
                    let (raw, buffer) = &mut **workspace;
                    extract_entry(raw, buffer, codec, index)
                }
                None => {
                    for_the_next_pass
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push(index);
                    Ok(())
                }
            },
        )
    };
    let run = || -> Result<()> {
        let mut pass = largest_first;
        while !pass.is_empty() {
            run_pass(&pass)?;
            pass = std::mem::take(
                &mut *for_the_next_pass
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()),
            );
            pass.sort_unstable_by_key(|&index| std::cmp::Reverse(items[index].entry.size));
        }
        Ok(())
    };
    if rayon::current_num_threads() <= worker_limit {
        run()?;
    } else {
        let mut threads = Vec::new();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_limit)
            .spawn_handler(|thread| {
                threads.push(std::thread::Builder::new().spawn(|| thread.run())?);
                Ok(())
            })
            .build()?;
        let ran = pool.install(run);
        drop(pool);
        for thread in threads {
            let _ = thread.join();
        }
        ran?;
    }
    create_links_after_files(
        staging.as_ref().map(|temp| temp.path()),
        &items,
        targets.into_inner().unwrap(),
    )?;
    tracker.finish();
    tracker.check()?;
    if let Some(staging) = staging {
        let directories: Vec<crate::Directory> = items
            .iter()
            .enumerate()
            .filter(|(index, item)| item.entry.directory && !merged.contains(index))
            .map(|(_, item)| crate::Directory {
                name: &item.entry.name,
                mode: item.mode,
                modified: item.modified.map(system_time),
            })
            .collect();
        staging.publish(&directories)?;
    }
    Ok(items.into_iter().map(|item| item.entry).collect())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn concurrent_reservations_never_exceed_budget_and_release_on_unwind() {
        let budget = Budget(AtomicUsize::new(8));
        let held = Barrier::new(16);
        let release = Barrier::new(16);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let budget = &budget;
                let held = &held;
                let release = &release;
                scope.spawn(move || {
                    let reservation = budget.reserve(1);
                    held.wait();
                    assert_eq!(
                        budget.0.load(Ordering::Relaxed),
                        0,
                        "all eight leases are live together"
                    );
                    assert!(budget.reserve(1).is_none());
                    release.wait();
                    drop(reservation);
                });
            }
        });
        assert_eq!(budget.0.load(Ordering::Relaxed), 8);
        assert!(
            std::panic::catch_unwind(|| {
                let _reservation = budget.reserve(8).unwrap();
                panic!("simulated failed decode");
            })
            .is_err()
        );
        assert_eq!(budget.0.load(Ordering::Relaxed), 8);
        assert!(entry_buffer(usize::MAX).is_err());
        assert!(budget.reserve(usize::MAX).is_none());
        assert!(budget.reserve(8).is_some());
    }
}

#[cfg(test)]
mod descriptor_scan_tests {
    use super::*;

    fn with_descriptor(prefix: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut data = prefix.to_vec();
        data.extend_from_slice(b"PK\x07\x08");
        data.extend_from_slice(&crc32fast::hash(prefix).to_le_bytes());
        data.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
        data.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
        data.extend_from_slice(rest);
        data
    }

    fn scan_in_pieces(data: &[u8], piece: usize) -> Result<()> {
        let mut scan = HiddenDescriptorScan::default();
        data.chunks(piece).try_for_each(|chunk| scan.feed(chunk))
    }

    #[test]
    fn finds_a_matching_descriptor_at_any_read_size() {
        let hostile = with_descriptor(b"prefix bytes", &[0; 40]);
        for piece in [1, 2, 3, 5, 23, 24, 1024] {
            assert!(scan_in_pieces(&hostile, piece).is_err(), "piece {piece}");
        }
    }

    #[test]
    fn ignores_descriptors_that_describe_other_data() {
        let mut nested = b"outer ".to_vec();
        nested.extend_from_slice(&with_descriptor(b"inner", &[0; 40]));
        for piece in [1, 2, 3, 5, 23, 24, 1024] {
            assert!(scan_in_pieces(&nested, piece).is_ok(), "piece {piece}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const EXTRA: usize = 20;

    #[kani::proof]
    #[kani::unwind(8)]
    fn time_fields_never_panic_on_any_extra_field() {
        let extra: [u8; EXTRA] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= EXTRA);
        let _ = exact_time(&extra[..length]);
        let _ = ntfs_time(&extra[..length]);
    }

    #[kani::proof]
    fn dos_time_arithmetic_never_overflows() {
        let dos: u32 = kani::any();
        let [year, month, day, hour, minute, second] = dos_fields(dos);
        let utc = days_since_unix_epoch(year, month.clamp(1, 12), day.max(1)) * 86_400
            + hour * 3_600
            + minute * 60
            + second;
        let _ = reading_nearer_dos_time(kani::any(), utc);
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn zip64_fields_never_panic_and_fit_their_field() {
        let extra: [u8; EXTRA] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= EXTRA);
        if let Ok(values) = single_zip64_field(&extra[..length]) {
            assert!(values.len() <= length.saturating_sub(4) / 8);
        }
    }

    #[kani::proof]
    fn every_descriptor_layout_reads_exactly_its_bytes() {
        let tail: u64 = kani::any();
        let descriptor: [u8; 24] = kani::any();
        if let Ok((signature, wide)) = descriptor_layout(tail) {
            let width = if wide { 8 } else { 4 };
            assert!(tail as usize == usize::from(signature) * 4 + 4 + 2 * width);
            let _ = descriptor_values(&descriptor[..tail as usize], signature, wide);
        }
    }

    #[kani::proof]
    fn lzma_headers_keep_the_properties_and_never_shrink_the_dictionary_below_need() {
        let prefix: [u8; 9] = kani::any();
        let size: u64 = kani::any();
        if let Ok(header) = lzma_alone_header(prefix, size) {
            let declared = u32::from_le_bytes([prefix[5], prefix[6], prefix[7], prefix[8]]);
            let used = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
            assert!(header[0] == prefix[4]);
            assert!(u64::from_le_bytes(header[5..].try_into().unwrap()) == size);
            assert!(used <= declared);
            assert!(u64::from(used) >= u64::from(declared).min(size));
        }
    }

    #[kani::proof]
    fn ppmd_headers_stay_inside_the_decoders_bounds() {
        let fields: u16 = kani::any();
        if let Ok((order, memory, restore)) = ppmd8_fields(fields) {
            assert!((ppmd_rust::PPMD8_MIN_ORDER..=ppmd_rust::PPMD8_MAX_ORDER).contains(&order));
            assert!(
                (ppmd_rust::PPMD8_MIN_MEM_SIZE..=ppmd_rust::PPMD8_MAX_MEM_SIZE).contains(&memory)
            );
            assert!(u64::from(memory) <= PPMD_LARGEST_MODEL);
            assert!(restore <= 1);
        }
    }
}

#[cfg(test)]
mod record_limit_tests {
    use super::*;

    fn layouts(records: usize) -> Vec<Vec<(u64, Option<u64>)>> {
        let choices: Vec<(u64, Option<u64>)> = (0..6)
            .flat_map(|start| [None, Some(1), Some(2), Some(3)].map(|span| (start, span)))
            .collect();
        (0..records).fold(vec![Vec::new()], |layouts, _| {
            layouts
                .iter()
                .flat_map(|layout| {
                    choices.iter().map(move |choice| {
                        let mut next = layout.clone();
                        next.push(*choice);
                        next
                    })
                })
                .collect()
        })
    }

    fn expected_limits(records: &[(u64, Option<u64>)], central_start: u64) -> Option<Vec<u64>> {
        let mut starts: Vec<u64> = records.iter().map(|record| record.0).collect();
        starts.sort();
        starts.dedup();
        if starts.len() != records.len() || starts.first().copied().unwrap_or(central_start) != 0 {
            return None;
        }
        records
            .iter()
            .map(|&(start, span)| {
                let limit = starts
                    .iter()
                    .copied()
                    .find(|&other| other > start)
                    .unwrap_or(central_start);
                (start + span? <= limit).then_some(limit)
            })
            .collect()
    }

    #[test]
    fn record_limits_accept_exactly_the_layouts_without_overlap() {
        for count in 0..=3 {
            for records in layouts(count) {
                for central_start in 0..9 {
                    assert_eq!(
                        record_limits(&records, central_start).ok(),
                        expected_limits(&records, central_start),
                        "{records:?} with the directory at {central_start}"
                    );
                }
            }
        }
    }

    #[test]
    fn record_limits_refuse_a_span_that_overflows() {
        for records in [
            vec![(0, Some(1)), (1, Some(u64::MAX))],
            vec![(0, Some(1)), (2, Some(u64::MAX - 1))],
            vec![(0, None)],
        ] {
            assert!(record_limits(&records, u64::MAX).is_err(), "{records:?}");
        }
    }
}

#[cfg(test)]
mod entry_nesting_tests {
    use super::*;

    #[test]
    fn an_entry_is_refused_inside_any_entry_of_its_own_extraction() {
        let (outer, inner) = (0u8, 0u8);
        let outer_entry = EntryOnThisThread::enter(&outer).unwrap();
        let inner_entry = EntryOnThisThread::enter(&inner).unwrap();
        assert!(EntryOnThisThread::enter(&outer).is_none());
        assert!(EntryOnThisThread::enter(&inner).is_none());
        drop(inner_entry);
        assert!(EntryOnThisThread::enter(&outer).is_none());
        drop(outer_entry);
        assert!(EntryOnThisThread::enter(&outer).is_some());
    }
}
