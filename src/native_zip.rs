//! Opt-in ZIP extraction for regular files: stored/Deflate, bounded streaming,
//! independent per-worker readers, and publication only after full verification.
use crate::{Entry, parent, safe_path};
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};
use zip::{CompressionMethod, ZipArchive};

struct Item {
    entry: Entry,
    crc: u32,
    compressed: u64,
    method: CompressionMethod,
    /// Stored name bytes, kept only when they differ from the decoded name.
    raw_name: Option<Box<[u8]>>,
    header_start: u64,
    /// Start of the next local record, or of the central directory.
    record_limit: u64,
    /// Permission bits to restore, without setuid, setgid or sticky bits.
    mode: Option<u32>,
    /// Modification time in seconds since the Unix epoch.
    modified: Option<i64>,
    /// The entry's data is a symlink target, not file content.
    symlink: bool,
}

/// Longest symlink target the native engines accept, in bytes.
const MAX_LINK_TARGET: u64 = 4096;

/// Checks a symlink target against the policy in docs/reference/zip-reader.md:
/// relative, with every `..` before the first name, and resolving lexically
/// inside the destination. The parent of every link is a real directory, so
/// the leading `..` steps are exact. The names after them only descend, and a
/// link they pass through was checked the same way. So the check holds however
/// the filesystem matches names, such as ignoring case or normalization.
fn check_link_target(name: &str, target: &[u8]) -> Result<()> {
    let target = std::str::from_utf8(target).context("ZIP symlink target is not UTF-8")?;
    ensure!(
        !target.is_empty() && !target.contains('\0') && !target.starts_with('/'),
        "ZIP symlink {name:?} has an unsafe target; use the compatibility backend"
    );
    let mut depth = name.matches('/').count();
    let mut descended = false;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                ensure!(
                    !descended,
                    "ZIP symlink {name:?} has `..` after a name; use the compatibility backend"
                );
                depth = depth.checked_sub(1).with_context(|| {
                    format!(
                        "ZIP symlink {name:?} points outside the destination; use the compatibility backend"
                    )
                })?;
            }
            _ => descended = true,
        }
    }
    Ok(())
}

const LOCAL_HEADER: u64 = 30;

impl Item {
    fn stored_name_len(&self) -> usize {
        self.raw_name.as_ref().map_or(
            self.entry.name.len() + usize::from(self.entry.directory),
            |raw| raw.len(),
        )
    }

    /// Whether `bytes` equals the name exactly as the central directory stores it.
    fn stored_name_is(&self, bytes: &[u8]) -> bool {
        match &self.raw_name {
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

const MAX_METADATA: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: u64 = 100_000;

// This accounts only for the two whole-entry buffers, not process RSS.
struct BufferBudget(AtomicUsize);

impl BufferBudget {
    fn reserve(&self, bytes: usize) -> Option<BufferReservation<'_>> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |available| {
                available.checked_sub(bytes)
            })
            .ok()
            .map(|_| BufferReservation {
                budget: self,
                bytes,
            })
    }
}

struct BufferReservation<'a> {
    budget: &'a BufferBudget,
    bytes: usize,
}

impl Drop for BufferReservation<'_> {
    fn drop(&mut self) {
        self.budget.0.fetch_add(self.bytes, Ordering::Relaxed);
    }
}

fn entry_buffer(size: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(size)
        .context("cannot allocate ZIP entry buffer")?;
    buffer.resize(size, 0);
    Ok(buffer)
}

// Bound directory allocation before the general ZIP parser sees untrusted
// counts/lengths. The opt-in path accepts conventional single-disk ZIP/ZIP64
// records with no prepended executable, trailing data, or ZIP64 extension sector.
fn preflight(file: &mut BufReader<File>) -> Result<(u64, Vec<EntryTime>)> {
    let length = file.get_ref().metadata()?.len();
    ensure!(length >= 22, "truncated ZIP");
    let tail_size = length.min(22 + 65535) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0; tail_size];
    file.read_exact(&mut tail)?;
    let end = (0..=tail.len() - 22)
        .rev()
        .find(|&i| {
            tail[i..i + 4] == *b"PK\x05\x06"
                && i + 22 + u16::from_le_bytes(tail[i + 20..i + 22].try_into().unwrap()) as usize
                    == tail.len()
        })
        .context("ZIP end record not found")?;
    let eocd = length - tail_size as u64 + end as u64;
    // zip-rs tolerates trailing data and may select a record embedded in the
    // comment. Restrict this opt-in reader to one unambiguous final record.
    ensure!(
        !tail[end + 4..]
            .windows(4)
            .any(|bytes| bytes == b"PK\x05\x06"),
        "ambiguous ZIP end records require the compatibility backend"
    );
    let read16 = |at| u16::from_le_bytes(tail[end + at..end + at + 2].try_into().unwrap());
    let read32 = |at| u32::from_le_bytes(tail[end + at..end + at + 4].try_into().unwrap());
    ensure!(
        read16(4) == 0 && read16(6) == 0 && read16(8) == read16(10),
        "multi-disk ZIP requires backend"
    );
    let mut count = read16(10) as u64;
    let mut bytes = read32(12) as u64;
    let mut start = read32(16) as u64;
    let mut directory_end = eocd;
    if count == u16::MAX as u64 || bytes == u32::MAX as u64 || start == u32::MAX as u64 {
        ensure!(eocd >= 20, "missing ZIP64 locator");
        file.seek(SeekFrom::Start(eocd - 20))?;
        let locator = crate::number::<20>(file)?;
        ensure!(
            &locator[..4] == b"PK\x06\x07"
                && locator[4..8] == [0; 4]
                && u32::from_le_bytes(locator[16..20].try_into()?) == 1,
            "invalid ZIP64 locator"
        );
        directory_end = u64::from_le_bytes(locator[8..16].try_into()?);
        ensure!(
            directory_end.checked_add(56) == Some(eocd - 20),
            "unsupported ZIP64 extension or offset"
        );
        file.seek(SeekFrom::Start(directory_end))?;
        let record = crate::number::<56>(file)?;
        ensure!(
            &record[..4] == b"PK\x06\x06"
                && u64::from_le_bytes(record[4..12].try_into()?) == 44
                && record[16..24] == [0; 8],
            "unsupported ZIP64 record"
        );
        count = u64::from_le_bytes(record[32..40].try_into()?);
        ensure!(
            u64::from_le_bytes(record[24..32].try_into()?) == count,
            "multi-disk ZIP64 requires backend"
        );
        bytes = u64::from_le_bytes(record[40..48].try_into()?);
        start = u64::from_le_bytes(record[48..56].try_into()?);
    }
    ensure!(
        count <= MAX_ENTRIES && bytes <= MAX_METADATA,
        "ZIP metadata limit exceeded"
    );
    ensure!(
        start.checked_add(bytes) == Some(directory_end),
        "unsupported ZIP central-directory extent"
    );
    // Validate the complete raw record chain, including comments and extras.
    file.seek(SeekFrom::Start(start))?;
    let mut position = start;
    let (mut name_field, mut extra_field) = (Vec::new(), Vec::new());
    let mut times = Vec::with_capacity(count as usize);
    for _ in 0..count {
        ensure!(
            directory_end - position >= 46,
            "truncated ZIP central entry"
        );
        let header = crate::number::<46>(file)?;
        ensure!(&header[..4] == b"PK\x01\x02", "invalid ZIP central entry");
        let [name, extra, comment] = [28, 30, 32]
            .map(|at| u16::from_le_bytes(header[at..at + 2].try_into().unwrap()) as u64);
        position += 46 + name + extra + comment;
        ensure!(
            position <= directory_end,
            "ZIP central entry exceeds directory"
        );
        name_field.resize(name as usize, 0);
        file.read_exact(&mut name_field)?;
        extra_field.resize(extra as usize, 0);
        file.read_exact(&mut extra_field)?;
        zip64_values(&extra_field)?;
        check_unicode_path(&extra_field, &name_field)?;
        times.push(EntryTime {
            exact: exact_time(&extra_field),
            dos: u32::from_le_bytes(header[12..16].try_into().unwrap()),
        });
        file.seek_relative(comment as i64)?;
    }
    ensure!(position == directory_end, "ZIP directory count mismatch");
    file.seek(SeekFrom::Start(0))?;
    Ok((count, times))
}

/// Time fields from a central record: an exact Unix time from an extra field,
/// and the DOS time (low 16 bits) and date (high 16 bits).
struct EntryTime {
    exact: Option<u32>,
    dos: u32,
}

/// Exact modification time from a central extra field. The extended
/// timestamp (0x5455) wins over the older Info-ZIP Unix field (0x5855), as in
/// Info-ZIP unzip. Like unzip and bsdtar, this ignores the NTFS field.
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
            0x5455 if body.first().is_some_and(|flags| flags & 1 != 0) => extended = seconds(1),
            0x5855 => unix = seconds(4),
            _ => {}
        }
        extra = &extra[4 + length..];
    }
    extended.or(unix)
}

/// Fields of a DOS date and time, unvalidated: year, month, day, hour,
/// minute and second.
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

/// Converts entry times to Unix seconds, caching the local offset of each DOS
/// date and hour, because `mktime` is slow and DOS times cluster.
#[derive(Default)]
struct Clock {
    offsets: HashMap<u32, Option<i64>>,
}

impl Clock {
    fn modified(&mut self, time: &EntryTime) -> Option<i64> {
        let dos_utc = {
            let [year, month, day, hour, minute, second] = dos_fields(time.dos);
            days_from_civil(year, month.clamp(1, 12), day.max(1)) * 86_400
                + hour * 3_600
                + minute * 60
                + second
        };
        if let Some(raw) = time.exact {
            // Writers store this field as 32 bits with no agreed sign. Take
            // whichever reading lies nearer the DOS time, so a 1969 file is
            // not dated 2106 and a 2040 file is not dated 1904.
            let (unsigned, signed) = (raw as i64, raw as i32 as i64);
            return Some(if (unsigned - dos_utc).abs() <= (signed - dos_utc).abs() {
                unsigned
            } else {
                signed
            });
        }
        let [year, month, day, hour, minute, second] = dos_fields(time.dos);
        let base = *self
            .offsets
            .entry(time.dos & 0xffff_f800)
            .or_insert_with(|| local_time([year, month, day, hour, 0, 0]));
        Some(base? + minute * 60 + second)
    }
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

/// Interprets local wall-clock fields with `mktime`, which normalizes values
/// out of range the way bsdtar does. Other platforms keep the extraction time.
#[cfg(unix)]
fn local_time([year, month, day, hour, minute, second]: [i64; 6]) -> Option<i64> {
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

#[cfg(not(unix))]
fn local_time(_: [i64; 6]) -> Option<i64> {
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

/// An opened archive and each entry's exact modification time.
type Opened = (ZipArchive<crate::zip_reader::Reader>, Vec<EntryTime>);

fn open(path: &Path, cache: bool) -> Result<Opened> {
    let mut file = BufReader::with_capacity(64 * 1024, File::open(path)?);
    let (count, times) = preflight(&mut file)?;
    let reader = if cache {
        crate::zip_reader::Reader::cached(file.into_inner(), 8 * 1024 * 1024)?
    } else {
        crate::zip_reader::Reader::new(file.into_inner())?
    };
    let archive = ZipArchive::new(reader)?;
    ensure!(
        archive.len() as u64 == count && archive.offset() == 0,
        "ZIP parser disagrees with validated directory"
    );
    Ok((archive, times))
}

/// `times` is `None` when the caller does not need modification times.
fn scan(
    archive: &ZipArchive<crate::zip_reader::Reader>,
    times: Option<&[EntryTime]>,
) -> Result<Vec<Item>> {
    ensure!(archive.len() <= 1_000_000, "ZIP entry limit exceeded");
    ensure!(
        !archive.metadata().has_duplicate_names(),
        "duplicate ZIP path"
    );
    let mut items = Vec::with_capacity(archive.len());
    let mut names = HashMap::new();
    let mut metadata_bytes = 0usize;
    let mut clock = Clock::default();
    for index in 0..archive.len() {
        let file = archive.by_index_data(index)?;
        ensure!(
            !file.encrypted(),
            "encrypted ZIP requires the compatibility backend"
        );
        let symlink = file.is_symlink();
        ensure!(
            !symlink || cfg!(unix),
            "ZIP symlinks on this platform require the compatibility backend"
        );
        ensure!(
            !symlink || file.size() <= MAX_LINK_TARGET,
            "ZIP symlink target is too long"
        );
        let directory = file.is_dir();
        if let Some(mode) = file.unix_mode() {
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
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ),
            "ZIP compression method requires the compatibility backend"
        );
        let name = file.name()?.into_owned();
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
        let raw = file.name_raw();
        let raw_name = (raw.len() != decoded_len || !raw.starts_with(name.as_bytes()))
            .then(|| Box::<[u8]>::from(raw));
        metadata_bytes += name.len() + raw_name.as_ref().map_or(0, |raw| raw.len()) + 64;
        ensure!(
            metadata_bytes <= 64 * 1024 * 1024,
            "ZIP metadata limit exceeded"
        );
        ensure!(!directory || file.size() == 0, "ZIP directory has payload");
        items.push(Item {
            entry: Entry {
                name,
                size: file.size(),
                directory,
            },
            crc: file.crc32(),
            compressed: file.compressed_size(),
            method: file.compression(),
            raw_name,
            header_start: file.header_start(),
            record_limit: 0,
            mode: file.unix_mode().map(|mode| mode & 0o777),
            modified: times.and_then(|times| clock.modified(&times[index])),
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
    // Each local record must end before the next one starts. Overlapping or
    // shared records let one archive mean different things to different readers.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_unstable_by_key(|&index| items[index].header_start);
    // Records tile the archive from offset 0, so no unreferenced bytes can
    // hold a record that only a streaming reader would see.
    let first = order
        .first()
        .map_or(archive.central_directory_start(), |&index| {
            items[index].header_start
        });
    ensure!(first == 0, "ZIP has bytes before its first record");
    let ends = order.iter().skip(1).map(|&index| items[index].header_start);
    let limits: Vec<u64> = ends
        .chain(std::iter::once(archive.central_directory_start()))
        .collect();
    for (&index, limit) in order.iter().zip(limits) {
        let item = &mut items[index];
        let minimum = item
            .header_start
            .checked_add(LOCAL_HEADER + item.stored_name_len() as u64)
            .and_then(|end| end.checked_add(item.compressed));
        ensure!(
            minimum.is_some_and(|end| end <= limit),
            "overlapping ZIP entries"
        );
        item.record_limit = limit;
    }
    Ok(items)
}

/// Rejects an Info-ZIP Unicode path field (ID 0x7075) that names the entry
/// differently from its stored name. Readers disagree on whether to honor it.
fn check_unicode_path(mut extra: &[u8], stored_name: &[u8]) -> Result<()> {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra
            .get(4..4 + length)
            .context("truncated ZIP extra field")?;
        if id == 0x7075 {
            ensure!(body.len() >= 5, "malformed ZIP Unicode path field");
            ensure!(
                body[5..] == *stored_name,
                "ZIP Unicode path field renames an entry; use the compatibility backend"
            );
        }
        extra = &extra[4 + length..];
    }
    Ok(())
}

/// Values of the ZIP64 extra field (ID 1), if present. Readers disagree on
/// which of two ZIP64 fields wins, so a second one is rejected.
fn zip64_values(mut extra: &[u8]) -> Result<Vec<u64>> {
    let mut values = Vec::new();
    let mut seen = false;
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra
            .get(4..4 + length)
            .context("truncated ZIP extra field")?;
        if id == 1 {
            ensure!(!seen, "ZIP entry has two ZIP64 extra fields");
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

/// Require the local header to agree with the central directory, and the
/// record to end exactly where the next record or the central directory
/// starts. Returns the payload offset and whether a data descriptor follows.
fn check_local_header(reader: &mut crate::zip_reader::Reader, item: &Item) -> Result<(u64, bool)> {
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
        "encrypted ZIP requires the compatibility backend"
    );
    let method = match item.method {
        CompressionMethod::Stored => 0,
        _ => 8,
    };
    ensure!(
        item.stored_name_is(&name) && read16(8) == method,
        "ZIP local header disagrees with central directory"
    );
    // With a data descriptor, producers may zero or mark unknown any local
    // field. Info-ZIP writing to a pipe zeroes the CRC and compressed size but
    // keeps the size.
    let deferred = flags & 8 != 0;
    let agrees = |local: u64, central: u64| {
        local == central || (deferred && (local == 0 || local == u32::MAX as u64))
    };
    let (crc, mut compressed, mut size) = (read32(14), read32(18) as u64, read32(22) as u64);
    // APPNOTE 4.5.3: the local ZIP64 field holds both sizes, original first.
    // Accept the shorter form some writers use, in order.
    check_unicode_path(&extra, &name)?;
    let zip64 = zip64_values(&extra)?;
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
        .filter(|&end| end <= item.record_limit)
        .context("overlapping ZIP entries")?;
    let tail = item.record_limit - data_end;
    if !deferred {
        ensure!(tail == 0, "ZIP has bytes between records");
        return Ok((data_start, false));
    }
    // The descriptor fills the rest of the record. Each length has one form.
    let (signature, wide) = match tail {
        12 => (false, false),
        16 => (true, false),
        20 => (false, true),
        24 => (true, true),
        _ => anyhow::bail!("ZIP data descriptor has an unexpected length"),
    };
    reader.seek(SeekFrom::Start(data_end))?;
    let mut descriptor = vec![0; tail as usize];
    reader.read_exact(&mut descriptor)?;
    ensure!(
        !signature || descriptor.starts_with(b"PK\x07\x08"),
        "invalid ZIP data descriptor"
    );
    let body = &descriptor[if signature { 4 } else { 0 }..];
    let field = |at: usize, width: usize| {
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&body[at..at + width]);
        u64::from_le_bytes(bytes)
    };
    let width = if wide { 8 } else { 4 };
    ensure!(
        field(0, 4) == item.crc as u64
            && field(4, width) == item.compressed
            && field(4 + width, width) == item.entry.size,
        "ZIP data descriptor disagrees with central directory"
    );
    Ok((data_start, true))
}

/// libdeflate whole-buffer decoding that also requires the Deflate stream to
/// end exactly at the end of its input. Trailing bytes inside an entry's
/// compressed size could hold a record that only a streaming reader sees.
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

/// Finds a data descriptor hidden inside stored data. A streaming reader ends
/// a stored entry at the first descriptor signature whose CRC and sizes match
/// the bytes before it, so such a match could hide a record after it. Nested
/// archives contain descriptors for their own entries, which do not match.
#[derive(Default)]
struct DescriptorScan {
    /// Unscanned bytes kept from earlier chunks, and this chunk.
    window: Vec<u8>,
    /// CRC state and entry offset at the start of `window`.
    crc: crc32fast::Hasher,
    offset: u64,
}

impl DescriptorScan {
    /// Signature plus CRC plus two 64-bit sizes.
    const LONGEST: usize = 4 + 4 + 16;

    fn feed(&mut self, chunk: &[u8]) -> Result<()> {
        self.window.extend_from_slice(chunk);
        // Keep the last 3 bytes, which may start a signature split by a read.
        let mut retain_from = self.window.len().saturating_sub(3);
        let mut scanned = 0;
        while let Some(found) = self.window[scanned..]
            .windows(4)
            .position(|bytes| bytes == b"PK\x07\x08")
        {
            let at = scanned + found;
            let fields = &self.window[at + 4..];
            if fields.len() < Self::LONGEST - 4 {
                // Wait for the rest of this descriptor before judging it.
                retain_from = at;
                break;
            }
            let mut crc = self.crc.clone();
            crc.update(&self.window[..at]);
            let (crc, length) = (crc.finalize() as u64, self.offset + at as u64);
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
        self.crc.update(&self.window[..retain_from]);
        self.offset += retain_from as u64;
        self.window.drain(..retain_from);
        Ok(())
    }
}

/// Flushes an extracted file and then sets its modification time, which a
/// later write would otherwise replace.
fn finish(output: &mut BufWriter<File>, item: &Item) -> Result<()> {
    output.flush()?;
    // Best effort, as unzip and bsdtar only warn when a time cannot be set.
    if let Some(seconds) = item.modified {
        let _ = output.get_ref().set_modified(system_time(seconds));
    }
    Ok(())
}

/// Streams one entry's payload to `sink`, checking its CRC and exact length.
/// Deflate input must end exactly at the compressed size. A stored entry
/// followed by a data descriptor must not hide a matching descriptor inside
/// its data.
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
    if item.method == CompressionMethod::Stored {
        ensure!(
            item.compressed == item.entry.size,
            "stored ZIP entry sizes differ"
        );
        let mut scan = descriptor.then(DescriptorScan::default);
        loop {
            let length = input.read(buffer)?;
            if length == 0 {
                break;
            }
            let chunk = &buffer[..length];
            if let Some(scan) = &mut scan {
                // Small pieces keep the scan window near 4 KiB per worker.
                chunk.chunks(4096).try_for_each(|piece| scan.feed(piece))?;
            }
            emit(chunk)?;
        }
    } else {
        let (input_buffer, output_buffer) = buffer.split_at_mut(64 * 1024);
        let mut inflater = flate2::Decompress::new(false);
        let (mut start, mut end) = (0, 0);
        loop {
            if start == end {
                end = input.read(input_buffer)?;
                start = 0;
            }
            let (consumed, produced) = (inflater.total_in(), inflater.total_out());
            let status = inflater.decompress(
                &input_buffer[start..end],
                output_buffer,
                flate2::FlushDecompress::None,
            )?;
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
    let (archive, _) = open(path, false)?;
    Ok(scan(&archive, None)?
        .into_iter()
        .map(|item| item.entry)
        .collect())
}

/// Verify and optionally extract a stored/Deflate ZIP. This opt-in engine keeps
/// file bytes and directories, not permissions, timestamps, or extended metadata.
/// The archive and destination parent must not be modified concurrently.
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
pub struct ReadOptions {
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
    let budget = BufferBudget(AtomicUsize::new(options.buffer_bytes.unwrap_or(
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            128 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        },
    )));
    // Linux extraction benefits from avoiding shared input seeks/reads for tiny
    // archives. Other platforms retain caching only for integrity verification.
    // This retained-input cache is separate from the live entry-buffer budget.
    let (archive, times) = open(
        path,
        fast && (destination.is_none() || cfg!(target_os = "linux")),
    )?;
    let items = scan(&archive, Some(&times))?;
    drop(times);
    let staging = if let Some(dst) = destination {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "extraction destination must not exist"
        );
        crate::check_output_size(
            dst,
            items.iter().map(|item| &item.entry),
            options.max_output_bytes,
        )?;
        Some(tempfile::tempdir_in(parent(dst))?)
    } else {
        None
    };
    if let Some(temp) = &staging {
        let mut directories = HashSet::new();
        for item in &items {
            let path = temp.path().join(&item.entry.name);
            directories.insert(if item.entry.directory {
                path
            } else {
                parent(&path).to_owned()
            });
        }
        for directory in directories {
            fs::create_dir_all(directory)?;
        }
    }
    // ZIP entries are independent outputs. Use descending-size order and work
    // stealing instead of fixed file lanes; Rayon does not promise start order.
    let mut work: Vec<_> = (0..items.len()).collect();
    work.sort_unstable_by_key(|&index| std::cmp::Reverse(items[index].entry.size));
    let targets = std::sync::Mutex::new(Vec::new());
    let run = || {
        work.par_iter().try_for_each_init(
            || {
                (
                    archive.clone().into_inner(),
                    vec![0u8; 256 * 1024],
                    fast.then(ExactInflater::new),
                )
            },
            |(raw, buffer, codec), &index| {
                let item = &items[index];
                let (data_start, descriptor) = check_local_header(raw, item)?;
                // Never wait for memory while holding worker resources. If a
                // reservation cannot fit now, this entry uses streaming instead.
                // Declare the lease before buffers so it outlives them on errors.
                // Large, very compressible outputs are faster through streaming
                // on measured workloads and avoid whole-entry allocation/copying.
                // Verification-only reads keep their existing decoder policy.
                let prefer_streaming = destination.is_some()
                    && item.entry.size >= 16 * 1024 * 1024
                    && item.compressed <= item.entry.size / 128;
                let reservation = if fast
                    && !prefer_streaming
                    && !item.symlink
                    && item.method == CompressionMethod::Deflated
                {
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
                    targets.lock().unwrap().push((index, target));
                    return Ok(());
                }
                let mut output = if let Some(temp) = &staging {
                    if !item.entry.directory {
                        let mut options = File::options();
                        options.write(true).create_new(true);
                        // The OS applies the umask, as bsdtar does.
                        #[cfg(unix)]
                        std::os::unix::fs::OpenOptionsExt::mode(
                            &mut options,
                            item.mode.unwrap_or(0o666),
                        );
                        Some(BufWriter::new(
                            options.open(temp.path().join(&item.entry.name))?,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if buffered {
                    // Allocate exact entry lengths while holding the shared lease.
                    // Reusing independently grown buffers would violate accounting.
                    let mut compressed = entry_buffer(item.compressed as usize)?;
                    let mut decoded = entry_buffer(item.entry.size as usize)?;
                    source.read_exact(&mut compressed)?;
                    let length = codec.as_mut().unwrap().inflate(&compressed, &mut decoded)?;
                    ensure!(
                        length as u64 == item.entry.size,
                        "ZIP decoded size mismatch"
                    );
                    ensure!(crc32fast::hash(&decoded) == item.crc, "ZIP CRC mismatch");
                    if let Some(output) = &mut output {
                        // Large single writes were costly on measured Windows
                        // workloads. Bound write size without allocating copies;
                        // other platforms retain their measured whole-buffer path.
                        if cfg!(windows) {
                            for chunk in decoded.chunks(256 * 1024) {
                                output.write_all(chunk)?;
                            }
                        } else {
                            output.write_all(&decoded)?;
                        }
                        finish(output, item)?;
                    }
                    return Ok(());
                }
                stream_entry(&mut source, item, descriptor, buffer, |bytes| {
                    if let Some(output) = &mut output {
                        output.write_all(bytes)?;
                    }
                    Ok(())
                })?;
                if let Some(output) = &mut output {
                    finish(output, item)?;
                }
                Ok(())
            },
        )
    };
    // On macOS, tiny-file creation saturates before all CPU workers are useful:
    // extra concurrent creates increase filesystem contention. Keep this policy
    // limited to measured tiny-file workloads; larger entries retain parallel
    // codec scaling. The caller's pool remains an upper bound.
    let automatic_limit = if cfg!(target_os = "macos")
        && staging.is_some()
        && items.iter().filter(|item| !item.entry.directory).count() >= 1024
        && items.iter().all(|item| item.entry.size <= 16 * 1024)
    {
        4
    } else {
        64
    };
    let worker_limit = options.threads.unwrap_or(automatic_limit);
    if rayon::current_num_threads() <= worker_limit {
        run()?;
    } else {
        // Also bound library callers whose Rayon pool exceeds the CLI maximum.
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_limit)
            .build()?
            .install(run)?;
    }
    // Links are created only after every file is written, so no file is
    // written through one.
    for (index, target) in targets.into_inner().unwrap() {
        let name = &items[index].entry.name;
        check_link_target(name, &target)?;
        #[cfg(unix)]
        if let Some(temp) = &staging {
            std::os::unix::fs::symlink(std::str::from_utf8(&target)?, temp.path().join(name))
                .with_context(|| {
                    format!("cannot create ZIP symlink {name:?}; use the compatibility backend")
                })?;
        }
    }
    if let (Some(temp), Some(dst)) = (staging, destination) {
        let directories: Vec<crate::Directory> = items
            .iter()
            .filter(|item| item.entry.directory)
            .map(|item| crate::Directory {
                name: &item.entry.name,
                mode: item.mode,
                modified: item.modified.map(system_time),
            })
            .collect();
        crate::publish(temp.path(), dst, &directories)?;
    }
    Ok(items.into_iter().map(|item| item.entry).collect())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn concurrent_reservations_never_exceed_budget_and_release_on_unwind() {
        let budget = BufferBudget(AtomicUsize::new(8));
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
                    // Exactly eight leases must be live; none can release early.
                    assert_eq!(budget.0.load(Ordering::Relaxed), 0);
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
        let mut scan = DescriptorScan::default();
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
