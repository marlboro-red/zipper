use crate::{BadField, Field, field_check, volumes::ZipDisks};
use anyhow::{Context, Result, ensure};
use std::{
    borrow::Cow,
    io::{Read, Seek, SeekFrom},
};

const END_RECORD: usize = 22;
const END_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const LONGEST_COMMENT: usize = u16::MAX as usize;
const ZIP64_LOCATOR: usize = 20;
const ZIP64_LOCATOR_SIGNATURE: &[u8; 4] = b"PK\x06\x07";
const ZIP64_END_RECORD: usize = 56;
const ZIP64_END_SIGNATURE: &[u8; 4] = b"PK\x06\x06";
const ZIP64_END_RECORD_SIZE_FIELD: u64 = 44;
const CENTRAL_ENTRY: usize = 46;
const CENTRAL_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
const MAX_ENTRIES: u64 = 100_000;
const MAX_METADATA: u64 = 64 * 1024 * 1024;
const ZIP64_SENTINEL: u32 = u32::MAX;
const ZIP64_COUNT_SENTINEL: u64 = u16::MAX as u64;
const ZIP64_DISK_SENTINEL: u64 = u16::MAX as u64;
const ZIP64_VALUE: usize = 8;

const ENCRYPTED: u16 = 0x0001;
const UTF8_NAME: u16 = 0x0800;

const ZIP64: u16 = 0x0001;
const NTFS: u16 = 0x000a;
const EXTENDED_TIMESTAMP: u16 = 0x5455;
const UNICODE_COMMENT: u16 = 0x6375;
const UNICODE_PATH: u16 = 0x7075;
const AES: u16 = 0x9901;
const ALIGNMENT: u16 = 0xa11e;
const ANDROID_ALIGNMENT: u16 = 0xd935;
const UNICODE_FIELD_HEADER: usize = 5;

const MADE_ON_DOS: u16 = 0;
const MADE_ON_UNIX: u16 = 3;
const DOS_DIRECTORY: u8 = 0x10;
const DOS_READ_ONLY: u8 = 0x01;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const SPECIAL_BITS: u32 = 0o7000;
const WRITE_BITS: u32 = 0o222;

const MULTI_DISK: &str = "ZIP is the last volume of a split archive, but its other volumes, named like name.z01 beside name.zip, are not beside it";
const MULTI_DISK_ZIP64: &str = "ZIP64 is the last volume of a split archive, but its other volumes, named like name.z01 beside name.zip, are not beside it";

pub(crate) struct Directory {
    pub(crate) start: u64,
    pub(crate) archive_start: u64,
    pub(crate) entries: Vec<CentralEntry>,
}

pub(crate) struct CentralEntry {
    pub(crate) stored_name: Box<[u8]>,
    pub(crate) unicode_name: Option<Box<[u8]>>,
    pub(crate) time: crate::native_zip::EntryTime,
    pub(crate) made_by: u16,
    pub(crate) flags: u16,
    pub(crate) method: u16,
    pub(crate) crc32: u32,
    pub(crate) compressed_size: u64,
    pub(crate) size: u64,
    pub(crate) external_attributes: u32,
    pub(crate) header_start: u64,
    pub(crate) disk: u16,
}

impl CentralEntry {
    pub(crate) fn name_raw(&self) -> &[u8] {
        self.unicode_name.as_deref().unwrap_or(&self.stored_name)
    }

    pub(crate) fn name(&self) -> Cow<'_, str> {
        decode_name_like_zip_crate(self.name_raw())
    }

    pub(crate) fn is_dir(&self) -> bool {
        matches!(self.name_raw().last(), Some(b'/' | b'\\'))
    }

    pub(crate) fn encrypted(&self) -> bool {
        self.flags & ENCRYPTED != 0
    }

    pub(crate) fn unix_mode_like_zip_crate(&self) -> Option<u32> {
        let attributes = self.external_attributes;
        if attributes == 0 {
            return None;
        }
        let high_half = attributes >> 16;
        match self.made_by >> 8 {
            MADE_ON_UNIX => Some(high_half & !SPECIAL_BITS),
            MADE_ON_DOS => {
                let dos = attributes as u8;
                let mode = if dos & DOS_DIRECTORY != 0 {
                    S_IFDIR | 0o775
                } else {
                    S_IFREG | 0o664
                };
                Some(if dos & DOS_READ_ONLY != 0 {
                    mode & !WRITE_BITS
                } else {
                    mode
                })
            }
            _ => (high_half != 0).then_some(high_half & !SPECIAL_BITS),
        }
    }

    pub(crate) fn is_symlink(&self) -> bool {
        self.unix_mode_like_zip_crate()
            .is_some_and(|mode| mode & S_IFLNK == S_IFLNK)
    }
}

pub(crate) fn read_directory<R: Read + Seek>(
    file: &mut R,
    disks: Option<&ZipDisks>,
) -> Result<Directory> {
    let length = file.seek(SeekFrom::End(0))?;
    ensure!(
        length >= END_RECORD as u64,
        "truncated ZIP; if the file is another format or a split volume, pass --zip-engine backend"
    );
    let tail_size = length.min((END_RECORD + LONGEST_COMMENT) as u64);
    file.seek(SeekFrom::Start(length - tail_size))?;
    let mut tail = vec![0; tail_size as usize];
    file.read_exact(&mut tail)?;
    let end = end_record(&tail)?;
    let end_offset = length - tail_size + end as u64;
    let mut extent = end_extent(fixed(&tail, end), end_offset)?;
    if disks.is_none() {
        field_check(extent.on_one_disk(), MULTI_DISK)?;
    }
    let mut locator_shift = None;
    if extent.reads_zip64_end() {
        ensure!(end_offset >= ZIP64_LOCATOR as u64, "missing ZIP64 locator");
        file.seek(SeekFrom::Start(end_offset - ZIP64_LOCATOR as u64))?;
        let locator = crate::number(file)?;
        locator_shift = (end_offset - ZIP64_LOCATOR as u64)
            .checked_sub(ZIP64_END_RECORD as u64)
            .and_then(|record| record.checked_sub(le64(&locator, 8)))
            .filter(|_| disks.is_none());
        let record_start = match disks {
            None => zip64_record_start(&locator, end_offset, |disk, offset, disks| {
                (disk == 0 && disks == 1)
                    .then(|| offset.checked_add(locator_shift.unwrap_or(0)))
                    .flatten()
            })?,
            Some(found) => zip64_record_start(&locator, end_offset, |disk, offset, disks| {
                (disks == found.count())
                    .then(|| found.place(disk, offset))
                    .flatten()
            })
            .with_context(|| {
                let [disk, disks] = [4, 16].map(|at| u64::from(le32(&locator, at)));
                format!(
                    "the ZIP64 locator in {} counts {} and points at offset {} of {}; the set is {}",
                    found.name(found.count() - 1),
                    volumes(disks),
                    le64(&locator, 8),
                    found.name(disk),
                    found.names()
                )
            })?,
        };
        file.seek(SeekFrom::Start(record_start))?;
        let zip64 = zip64_extent(&crate::number(file)?, record_start)?;
        if disks.is_none() {
            field_check(zip64.on_one_disk(), MULTI_DISK_ZIP64)?;
        }
        extent = extent.replaced_by(zip64)?;
    } else if end_offset >= ZIP64_LOCATOR as u64 {
        file.seek(SeekFrom::Start(end_offset - ZIP64_LOCATOR as u64))?;
        let signature: [u8; 4] = crate::number(file)?;
        ensure!(
            signature != *ZIP64_LOCATOR_SIGNATURE,
            "ZIP has a ZIP64 locator its end record does not use"
        );
    }
    let shift = match disks {
        Some(disks) => {
            extent.start = directory_on_disks(&extent, disks)?;
            0
        }
        None => prefix_shift(&extent, locator_shift)?,
    };
    extent.start += shift;
    check_extent(&extent)?;
    file.seek(SeekFrom::Start(extent.start))?;
    let mut entries = central_entries(file, extent).map_err(WalkError::into_anyhow)?;
    match disks {
        Some(disks) => place_entries(&mut entries, disks)?,
        None => shift_entries(&mut entries, shift)?,
    }
    let archive_start = archive_start(file, &entries, extent.start, shift)?;
    Ok(Directory {
        start: extent.start,
        archive_start,
        entries,
    })
}

// EXTERNAL(Python zipfile): it reads a directory that ends before its end record as shifted by the gap.
fn prefix_shift(extent: &Extent, locator_shift: Option<u64>) -> Result<u64> {
    let Some(shift) = extent
        .start
        .checked_add(extent.bytes)
        .and_then(|end| extent.end.checked_sub(end))
    else {
        return Ok(0);
    };
    ensure!(
        locator_shift.is_none_or(|locator| locator == shift),
        "ZIP64 locator and central directory count {} and {shift} bytes before the archive",
        locator_shift.unwrap_or(0)
    );
    Ok(shift)
}

fn shift_entries(entries: &mut [CentralEntry], shift: u64) -> Result<()> {
    for entry in entries {
        entry.header_start = entry
            .header_start
            .checked_add(shift)
            .context("ZIP local header offset is too large")?;
    }
    Ok(())
}

// SPEC(APPNOTE 8.5.3, 8.5.4): a split archive may start with the spanning signature, or with PK00 when it fits one piece.
const SPANNING_MARKER: usize = 4;
const SPANNING_MARKERS: [&[u8; SPANNING_MARKER]; 2] = [b"PK\x07\x08", b"PK00"];

fn archive_start<R: Read + Seek>(
    file: &mut R,
    entries: &[CentralEntry],
    directory_start: u64,
    shift: u64,
) -> Result<u64> {
    let first = entries
        .iter()
        .map(|entry| entry.header_start)
        .min()
        .unwrap_or(directory_start);
    if first == 0 {
        return Ok(0);
    }
    ensure!(
        shift == 0 || first == shift,
        "ZIP offsets count {shift} bytes before the archive, but its first record starts at offset {first}, so the bytes before it cannot be reconciled"
    );
    ensure!(
        !entries.is_empty(),
        "ZIP has {first} bytes before an archive with no entries"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut head = Vec::with_capacity(SPANNING_MARKER);
    file.take(first.min(SPANNING_MARKER as u64))
        .read_to_end(&mut head)?;
    if first == SPANNING_MARKER as u64 && SPANNING_MARKERS.iter().any(|marker| head == marker[..]) {
        return Ok(first);
    }
    let mut read_at = |buffer: &mut [u8], offset: u64| {
        file.seek(SeekFrom::Start(offset))?;
        file.read(buffer)
    };
    if let Some(why) = crate::volumes::check_stub(&mut read_at, first)?.refusal() {
        anyhow::bail!("ZIP has {first} bytes before its first record, and they {why}");
    }
    Ok(first)
}

fn volumes(count: u64) -> String {
    match count {
        1 => "1 volume".to_owned(),
        _ => format!("{count} volumes"),
    }
}

fn directory_on_disks(extent: &Extent, disks: &ZipDisks) -> Result<u64> {
    let last = disks.name(disks.count() - 1);
    ensure!(
        extent.disk.checked_add(1) == Some(disks.count()),
        "the end record of {last} says the archive has {}, but {} were found, {}",
        volumes(extent.disk.saturating_add(1)),
        disks.count(),
        disks.names()
    );
    ensure!(
        extent.count_here <= extent.count,
        "the end record of {last} counts more entries on its last volume than in the archive"
    );
    disks
        .place(extent.start_disk, extent.start)
        .with_context(|| {
            format!(
                "the ZIP central directory starts at offset {} of {}, outside that volume; the set is {}",
                extent.start,
                disks.name(extent.start_disk),
                disks.names()
            )
        })
}

fn place_entries(entries: &mut [CentralEntry], disks: &ZipDisks) -> Result<()> {
    for entry in entries {
        let disk = u64::from(entry.disk);
        entry.header_start = disks.place(disk, entry.header_start).with_context(|| {
            format!(
                "ZIP entry {} has its local header at offset {} of {}, outside that volume; the set is {}",
                entry.name(),
                entry.header_start,
                disks.name(disk),
                disks.names()
            )
        })?;
    }
    Ok(())
}

struct DirectoryBytes<'r, R> {
    reader: &'r mut R,
    remaining: u64,
}

enum WalkError {
    Read(std::io::Error),
    Bad(BadField),
}

impl From<std::io::Error> for WalkError {
    fn from(error: std::io::Error) -> Self {
        Self::Read(error)
    }
}

impl From<BadField> for WalkError {
    fn from(error: BadField) -> Self {
        Self::Bad(error)
    }
}

impl WalkError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Read(error) => error.into(),
            Self::Bad(error) => error.into(),
        }
    }
}

type Walk<T> = std::result::Result<T, WalkError>;

impl<R: Read> DirectoryBytes<'_, R> {
    fn within_directory(&self, wanted: usize) -> usize {
        usize::try_from(self.remaining).map_or(wanted, |left| left.min(wanted))
    }

    fn fill(&mut self, bytes: &mut [u8]) -> Walk<()> {
        match self.reader.read_exact(bytes) {
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                Err(BadField("truncated ZIP central directory"))?
            }
            outcome => outcome?,
        }
        self.remaining -= bytes.len() as u64;
        Ok(())
    }

    fn append(&mut self, wanted: usize, record: &mut Vec<u8>) -> Walk<()> {
        let wanted = self.within_directory(wanted);
        let start = record.len();
        record.reserve_exact(wanted);
        record.resize(start + wanted, 0);
        self.fill(&mut record[start..])
    }

    fn next_record(&mut self, record: &mut Vec<u8>) -> Walk<()> {
        let mut header = [0; CENTRAL_ENTRY];
        let header = &mut header[..self.within_directory(CENTRAL_ENTRY)];
        self.fill(header)?;
        record.clear();
        record.reserve_exact(header.len());
        record.extend_from_slice(header);
        if header.len() == CENTRAL_ENTRY {
            let variable = [28, 30, 32].map(|at| usize::from(le16(header, at)));
            self.append(variable.iter().sum(), record)?;
        }
        Ok(())
    }
}

fn central_entries<R: Read>(file: &mut R, extent: Extent) -> Walk<Vec<CentralEntry>> {
    walk_records(file, extent, |record, position| {
        central_entry(record, position, is_utf8)
    })
}

fn is_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

fn walk_records<R: Read, T>(
    file: &mut R,
    extent: Extent,
    mut entry: impl FnMut(&[u8], u64) -> Field<T>,
) -> Walk<Vec<T>> {
    let mut directory = DirectoryBytes {
        reader: file,
        remaining: extent.bytes,
    };
    let mut entries = Vec::new();
    let mut record = Vec::new();
    let mut position = extent.start;
    for _ in 0..extent.count {
        directory.next_record(&mut record)?;
        entries.push(entry(&record, position)?);
        position += record.len() as u64;
    }
    field_check(directory.remaining == 0, "ZIP directory count mismatch")?;
    Ok(entries)
}

fn central_entry(
    bytes: &[u8],
    position: u64,
    is_utf8: impl Fn(&[u8]) -> bool,
) -> Field<CentralEntry> {
    let record = central_record(bytes)?;
    // EXTERNAL(Python zipfile, 7-Zip): no CP437 fallback for a flagged name.
    field_check(
        record.flags & UTF8_NAME == 0 || is_utf8(record.name),
        "ZIP name flagged as UTF-8 is not UTF-8",
    )?;
    let unicode_name = unicode_name_judged_by(record.extra, record.name, &is_utf8)?;
    // EXTERNAL(zip crate): it refuses a local header offset that overflows when added to the entry's position.
    field_check(
        record.header_start.checked_add(position).is_some(),
        "ZIP local header offset is too large",
    )?;
    Ok(CentralEntry {
        stored_name: record.name.into(),
        unicode_name,
        time: crate::native_zip::entry_time(record.extra, record.dos_time),
        made_by: record.made_by,
        flags: record.flags,
        method: record.method,
        crc32: record.crc32,
        compressed_size: record.compressed_size,
        size: record.size,
        external_attributes: record.external_attributes,
        header_start: record.header_start,
        disk: record.disk,
    })
}

fn fixed<const N: usize>(bytes: &[u8], at: usize) -> &[u8; N] {
    bytes[at..at + N].try_into().expect("a slice of N bytes")
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(*fixed(bytes, at))
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(*fixed(bytes, at))
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(*fixed(bytes, at))
}

fn end_record(tail: &[u8]) -> Field<usize> {
    let comment = |at: usize| usize::from(le16(tail, at + 20));
    let end = (0..=tail.len().saturating_sub(END_RECORD))
        .rev()
        .find(|&at| {
            tail.len() >= at + END_RECORD
                && tail[at..at + 4] == *END_SIGNATURE
                && at + END_RECORD + comment(at) == tail.len()
        })
        .ok_or(BadField(
            "ZIP end record not found; if the file is another format or a split volume, pass --zip-engine backend",
        ))?;
    // EXTERNAL(zip crate): it accepts trailing data and may pick an end record inside a comment.
    field_check(
        !tail[end + 4..]
            .windows(4)
            .any(|bytes| bytes == END_SIGNATURE),
        "ZIP has ambiguous end records",
    )?;
    Ok(end)
}

#[derive(Clone, Copy)]
struct Extent {
    count: u64,
    bytes: u64,
    start: u64,
    end: u64,
    disk: u64,
    start_disk: u64,
    count_here: u64,
}

impl Extent {
    fn needs_zip64(&self) -> bool {
        self.count == ZIP64_COUNT_SENTINEL
            || self.bytes == u64::from(ZIP64_SENTINEL)
            || self.start == u64::from(ZIP64_SENTINEL)
            || self.disk == ZIP64_DISK_SENTINEL
            || self.start_disk == ZIP64_DISK_SENTINEL
            || self.count_here == ZIP64_COUNT_SENTINEL
    }

    fn on_one_disk(&self) -> bool {
        self.disk == 0 && self.start_disk == 0 && self.count_here == self.count
    }

    fn ends_before_zip64_records(&self) -> bool {
        let records = (ZIP64_END_RECORD + ZIP64_LOCATOR) as u64;
        self.end
            .checked_sub(records)
            .is_some_and(|records_start| self.start.checked_add(self.bytes) == Some(records_start))
    }

    fn reads_zip64_end(&self) -> bool {
        self.needs_zip64() || self.ends_before_zip64_records()
    }

    fn replaced_by(self, zip64: Extent) -> Field<Extent> {
        let agrees = |value: u64, sentinel: u64, wide: u64| value == sentinel || value == wide;
        field_check(
            agrees(self.count, ZIP64_COUNT_SENTINEL, zip64.count)
                && agrees(self.bytes, ZIP64_SENTINEL.into(), zip64.bytes)
                && agrees(self.start, ZIP64_SENTINEL.into(), zip64.start)
                && agrees(self.disk, ZIP64_DISK_SENTINEL, zip64.disk)
                && agrees(self.start_disk, ZIP64_DISK_SENTINEL, zip64.start_disk)
                && agrees(self.count_here, ZIP64_COUNT_SENTINEL, zip64.count_here),
            "ZIP64 end record disagrees with the end record",
        )?;
        Ok(zip64)
    }
}

fn end_extent(record: &[u8; END_RECORD], end: u64) -> Field<Extent> {
    Ok(Extent {
        count: le16(record, 10).into(),
        bytes: le32(record, 12).into(),
        start: le32(record, 16).into(),
        end,
        disk: le16(record, 4).into(),
        start_disk: le16(record, 6).into(),
        count_here: le16(record, 8).into(),
    })
}

fn zip64_record_start(
    locator: &[u8; ZIP64_LOCATOR],
    end_offset: u64,
    place: impl Fn(u64, u64, u64) -> Option<u64>,
) -> Field<u64> {
    field_check(
        locator[..4] == *ZIP64_LOCATOR_SIGNATURE,
        "invalid ZIP64 locator",
    )?;
    let start = place(
        le32(locator, 4).into(),
        le64(locator, 8),
        le32(locator, 16).into(),
    )
    .ok_or(BadField("invalid ZIP64 locator"))?;
    field_check(
        start.checked_add((ZIP64_END_RECORD + ZIP64_LOCATOR) as u64) == Some(end_offset),
        "unsupported ZIP64 extension or offset",
    )?;
    Ok(start)
}

fn zip64_extent(record: &[u8; ZIP64_END_RECORD], end: u64) -> Field<Extent> {
    field_check(
        record[..4] == *ZIP64_END_SIGNATURE && le64(record, 4) == ZIP64_END_RECORD_SIZE_FIELD,
        "unsupported ZIP64 record",
    )?;
    Ok(Extent {
        count: le64(record, 32),
        bytes: le64(record, 40),
        start: le64(record, 48),
        end,
        disk: le32(record, 16).into(),
        start_disk: le32(record, 20).into(),
        count_here: le64(record, 24),
    })
}

fn check_extent(extent: &Extent) -> Field<()> {
    field_check(
        extent.count <= MAX_ENTRIES && extent.bytes <= MAX_METADATA,
        "ZIP metadata limit exceeded",
    )?;
    field_check(
        extent.start.checked_add(extent.bytes) == Some(extent.end),
        "unsupported ZIP central-directory extent",
    )
}

struct Record<'a> {
    made_by: u16,
    flags: u16,
    method: u16,
    dos_time: u32,
    crc32: u32,
    compressed_size: u64,
    size: u64,
    external_attributes: u32,
    header_start: u64,
    disk: u16,
    name: &'a [u8],
    extra: &'a [u8],
}

fn central_record(rest: &[u8]) -> Field<Record<'_>> {
    field_check(rest.len() >= CENTRAL_ENTRY, "truncated ZIP central entry")?;
    field_check(rest[..4] == *CENTRAL_SIGNATURE, "invalid ZIP central entry")?;
    let [name, extra, comment] = [28, 30, 32].map(|at| usize::from(le16(rest, at)));
    let length = CENTRAL_ENTRY + name + extra + comment;
    field_check(length <= rest.len(), "ZIP central entry exceeds directory")?;
    let extra_start = CENTRAL_ENTRY + name;
    let extra_field = &rest[extra_start..extra_start + extra];
    let [size, compressed_size, header_start] = extra_values(
        extra_field,
        [le32(rest, 24), le32(rest, 20), le32(rest, 42)],
    )?;
    Ok(Record {
        made_by: le16(rest, 4),
        flags: le16(rest, 8),
        method: le16(rest, 10),
        dos_time: le32(rest, 12),
        crc32: le32(rest, 16),
        compressed_size,
        size,
        external_attributes: le32(rest, 38),
        header_start,
        disk: le16(rest, 34),
        name: &rest[CENTRAL_ENTRY..extra_start],
        extra: extra_field,
    })
}

fn extra_values(mut extra: &[u8], entry: [u32; 3]) -> Field<[u64; 3]> {
    let mut values = entry.map(u64::from);
    let mut zip64_seen = false;
    while extra.len() >= 4 {
        let length = usize::from(le16(extra, 2));
        let body = extra
            .get(4..4 + length)
            .ok_or(BadField("truncated ZIP extra field"))?;
        match le16(extra, 0) {
            ZIP64 => {
                field_check(!zip64_seen, "ZIP entry has two ZIP64 extra fields")?;
                zip64_seen = true;
                values = zip64_values(body, entry)?;
            }
            // EXTERNAL(zip crate): it refuses these three kinds when too short for their own header.
            EXTENDED_TIMESTAMP => {
                field_check(!body.is_empty(), "empty ZIP extended timestamp field")?
            }
            UNICODE_COMMENT | UNICODE_PATH => field_check(
                body.len() >= UNICODE_FIELD_HEADER,
                "short ZIP Unicode extra field",
            )?,
            ALIGNMENT | ANDROID_ALIGNMENT => {
                field_check(body.len() >= 2, "short ZIP alignment extra field")?
            }
            _ => {}
        }
        extra = &extra[4 + length..];
    }
    // EXTERNAL(zip crate): it refuses two or three trailing bytes that start with an id it parses.
    field_check(
        extra.len() < 2 || !the_zip_crate_parses(le16(extra, 0)),
        "truncated ZIP extra field header",
    )?;
    Ok(values)
}

fn the_zip_crate_parses(id: u16) -> bool {
    matches!(
        id,
        ZIP64
            | NTFS
            | EXTENDED_TIMESTAMP
            | UNICODE_COMMENT
            | UNICODE_PATH
            | AES
            | ALIGNMENT
            | ANDROID_ALIGNMENT
    )
}

// SPEC(APPNOTE 4.5.3): only the fields that hold 0xFFFFFFFF appear, in this order.
fn zip64_values(body: &[u8], entry: [u32; 3]) -> Field<[u64; 3]> {
    let mut values = entry.map(u64::from);
    let mut read = 0;
    for (value, field) in values.iter_mut().zip(entry) {
        if field == ZIP64_SENTINEL {
            let bytes = body
                .get(read..)
                .and_then(<[u8]>::first_chunk::<ZIP64_VALUE>)
                .ok_or(BadField("ZIP64 extra field is too short"))?;
            *value = u64::from_le_bytes(*bytes);
            read += ZIP64_VALUE;
        }
    }
    Ok(values)
}

fn extra_fields(mut extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        let header = extra.first_chunk::<4>()?;
        let length = usize::from(u16::from_le_bytes([header[2], header[3]]));
        let body = extra.get(4..4 + length)?;
        let id = u16::from_le_bytes([header[0], header[1]]);
        extra = &extra[4 + length..];
        Some((id, body))
    })
}

fn unicode_name_judged_by(
    extra: &[u8],
    stored: &[u8],
    is_utf8: impl Fn(&[u8]) -> bool,
) -> Field<Option<Box<[u8]>>> {
    let mut renamed = None;
    for (_, body) in extra_fields(extra).filter(|&(id, _)| id == UNICODE_PATH) {
        let path = body
            .get(UNICODE_FIELD_HEADER..)
            .filter(|path| is_utf8(path))
            .ok_or(BadField("malformed ZIP Unicode path field"))?;
        if path != stored {
            // EXTERNAL(zip crate): it takes a Unicode path only when the checksum matches the stored name.
            field_check(
                le32(body, 1) == name_checksum(stored)
                    && respells_stored_name_as_zip_crate_decodes(path, stored, &is_utf8),
                "ZIP Unicode path field renames an entry",
            )?;
            renamed = Some(path);
        }
    }
    Ok(renamed.map(Box::from))
}

fn respells_stored_name_as_zip_crate_decodes(
    path: &[u8],
    stored: &[u8],
    is_utf8: impl Fn(&[u8]) -> bool,
) -> bool {
    let decodes_to_itself = is_utf8(stored);
    !decodes_to_itself && is_cp437_spelling(path, stored)
}

fn is_cp437_spelling(path: &[u8], stored: &[u8]) -> bool {
    let mut rest = path;
    for &byte in stored {
        let mut buffer = [0; 4];
        match rest.strip_prefix(cp437_char(byte).encode_utf8(&mut buffer).as_bytes()) {
            Some(after) => rest = after,
            None => return false,
        }
    }
    rest.is_empty()
}

#[cfg(not(kani))]
fn name_checksum(name: &[u8]) -> u32 {
    crc32fast::hash(name)
}

// EXTERNAL(Kani 0.68): its compiler panics on crc32fast's Arm CRC32 intrinsics.
#[cfg(kani)]
fn name_checksum(name: &[u8]) -> u32 {
    reference_crc32(name)
}

#[cfg(any(test, kani))]
const CRC32_TABLE: [u32; 256] = {
    const REFLECTED_CRC32_POLYNOMIAL: u32 = 0xEDB8_8320;
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (REFLECTED_CRC32_POLYNOMIAL & (crc & 1).wrapping_neg());
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

#[cfg(any(test, kani))]
fn reference_crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |crc, &byte| {
        (crc >> 8) ^ CRC32_TABLE[usize::from(crc as u8 ^ byte)]
    })
}

#[rustfmt::skip]
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

pub(crate) fn decode_name_like_zip_crate(stored: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(stored) {
        Ok(name) => name.into(),
        Err(_) => stored
            .iter()
            .map(|&byte| cp437_char(byte))
            .collect::<String>()
            .into(),
    }
}

fn cp437_char(byte: u8) -> char {
    match byte {
        0..0x80 => char::from(byte),
        _ => CP437_HIGH[usize::from(byte - 0x80)],
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const EXTRA: usize = 20;

    #[kani::proof]
    #[kani::unwind(10)]
    fn extra_fields_never_panic_and_keep_every_value_without_a_sentinel() {
        let extra: [u8; EXTRA] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= EXTRA);
        let entry: [u32; 3] = kani::any();
        if let Ok(values) = extra_values(&extra[..length], entry) {
            for (value, field) in values.into_iter().zip(entry) {
                assert!(field == ZIP64_SENTINEL || value == u64::from(field));
            }
        }
    }

    const RECORD: usize = CENTRAL_ENTRY + 8;

    #[kani::proof]
    #[kani::unwind(10)]
    fn central_records_never_panic_and_end_inside_the_directory() {
        let bytes: [u8; RECORD] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= RECORD);
        if let Ok(record) = central_record(&bytes[..length]) {
            assert!(CENTRAL_ENTRY + record.name.len() + record.extra.len() <= length);
        }
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn end_records_and_zip64_records_end_where_their_directory_ends() {
        let end: u64 = kani::any();
        if let Ok(extent) = end_extent(&kani::any(), end) {
            assert!(extent.end == end && extent.count <= ZIP64_COUNT_SENTINEL);
            assert!(extent.disk <= ZIP64_DISK_SENTINEL && extent.start_disk <= ZIP64_DISK_SENTINEL);
            assert!(extent.count_here <= ZIP64_COUNT_SENTINEL);
        }
        let placed: Option<u64> = kani::any();
        if let Ok(start) = zip64_record_start(&kani::any(), end, |_, _, _| placed) {
            assert!(start + (ZIP64_END_RECORD + ZIP64_LOCATOR) as u64 == end);
        }
        if let Ok(extent) = zip64_extent(&kani::any(), end) {
            if check_extent(&extent).is_ok() {
                assert!(extent.start + extent.bytes == end);
                assert!(extent.count <= MAX_ENTRIES && extent.bytes <= MAX_METADATA);
            }
        }
    }

    fn any_extent() -> Extent {
        Extent {
            count: kani::any(),
            bytes: kani::any(),
            start: kani::any(),
            end: kani::any(),
            disk: kani::any(),
            start_disk: kani::any(),
            count_here: kani::any(),
        }
    }

    #[kani::proof]
    fn a_zip64_end_replaces_only_the_values_an_end_record_leaves_to_it() {
        let plain = any_extent();
        let zip64 = any_extent();
        if plain.ends_before_zip64_records() {
            assert!(
                plain.start + plain.bytes + (ZIP64_END_RECORD + ZIP64_LOCATOR) as u64 == plain.end
            );
        }
        if let Ok(chosen) = plain.replaced_by(zip64) {
            assert!(chosen.end == zip64.end);
            assert!(plain.count == ZIP64_COUNT_SENTINEL || plain.count == chosen.count);
            assert!(plain.bytes == u64::from(ZIP64_SENTINEL) || plain.bytes == chosen.bytes);
            assert!(plain.start == u64::from(ZIP64_SENTINEL) || plain.start == chosen.start);
            assert!(plain.disk == ZIP64_DISK_SENTINEL || plain.disk == chosen.disk);
            assert!(
                plain.start_disk == ZIP64_DISK_SENTINEL || plain.start_disk == chosen.start_disk
            );
            assert!(
                plain.count_here == ZIP64_COUNT_SENTINEL || plain.count_here == chosen.count_here
            );
        }
    }

    struct ShortReads<'a> {
        bytes: &'a [u8],
        served: usize,
    }

    impl Read for ShortReads<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if kani::any() {
                return Err(std::io::ErrorKind::Other.into());
            }
            let most = buffer.len().min(self.bytes.len() - self.served);
            let count: usize = kani::any();
            kani::assume(count <= most && (count > 0 || most == 0));
            buffer[..count].copy_from_slice(&self.bytes[self.served..self.served + count]);
            self.served += count;
            Ok(count)
        }
    }

    const STREAM: usize = 4;
    const HELD: usize = 2;
    const WANTED: usize = STREAM + 1;

    #[kani::proof]
    #[kani::unwind(8)]
    fn directory_bytes_stay_inside_the_directory_and_the_record() {
        let stream: [u8; STREAM] = kani::any();
        let available: usize = kani::any();
        kani::assume(available <= STREAM);
        let held: [u8; HELD] = kani::any();
        let held_length: usize = kani::any();
        kani::assume(held_length <= HELD);
        let remaining: u64 = kani::any();
        let wanted: usize = kani::any();
        kani::assume(wanted <= WANTED);
        let mut record = held[..held_length].to_vec();
        let capacity_before = record.capacity();
        let mut reader = ShortReads {
            bytes: &stream[..available],
            served: 0,
        };
        let mut directory = DirectoryBytes {
            reader: &mut reader,
            remaining,
        };
        let outcome = directory.append(wanted, &mut record);
        let left = directory.remaining;
        let asked = remaining.min(wanted as u64) as usize;
        let served = reader.served;
        assert!(served <= asked);
        assert!(record[..held_length] == held[..held_length]);
        assert!(record.capacity() <= capacity_before.max(held_length + asked));
        match outcome {
            Ok(()) => {
                assert!(served == asked && left == remaining - asked as u64);
                assert!(record[held_length..] == stream[..asked]);
            }
            Err(_) => assert!(left == remaining),
        }
    }

    const UNICODE_EXTRA: usize = 12;
    const STORED: usize = 2;

    #[kani::proof]
    #[kani::unwind(6)]
    fn unicode_names_never_panic_and_rename_only_when_the_checksum_matches() {
        let extra: [u8; UNICODE_EXTRA] = kani::any();
        let extra_length: usize = kani::any();
        kani::assume(extra_length <= UNICODE_EXTRA);
        let extra = &extra[..extra_length];
        let stored: [u8; STORED] = kani::any();
        let stored_length: usize = kani::any();
        kani::assume(stored_length <= STORED);
        let stored = &stored[..stored_length];
        let any_utf8_verdict = |_: &[u8]| kani::any();
        if let Ok(Some(name)) = unicode_name_judged_by(extra, stored, any_utf8_verdict) {
            assert!(le16(extra, 0) == UNICODE_PATH);
            assert!(le32(extra, 5) == reference_crc32(stored));
            assert!(*name == extra[4 + UNICODE_FIELD_HEADER..4 + usize::from(le16(extra, 2))]);
            let mut spelling = Vec::new();
            for &byte in stored {
                let mut buffer = [0; 4];
                spelling.extend_from_slice(cp437_char(byte).encode_utf8(&mut buffer).as_bytes());
            }
            assert!(*name == *spelling && *name != *stored);
        }
    }

    #[kani::proof]
    fn unix_modes_never_panic_or_keep_special_bits() {
        let entry = CentralEntry {
            stored_name: Box::new([]),
            unicode_name: None,
            time: crate::native_zip::entry_time(&[], 0),
            made_by: kani::any(),
            flags: 0,
            method: 0,
            crc32: 0,
            compressed_size: 0,
            size: 0,
            external_attributes: kani::any(),
            header_start: 0,
            disk: 0,
        };
        let mode = entry.unix_mode_like_zip_crate();
        assert!(mode.is_none_or(|mode| mode & SPECIAL_BITS == 0 && mode >> 16 == 0));
        assert!(mode.is_some() || entry.external_attributes >> 16 == 0);
        let _ = entry.is_symlink();
    }

    const SHORT: usize = 4;
    const TWO_SHORT_RECORDS: usize = 2 * (CENTRAL_ENTRY + SHORT);
    const PAST_THE_DIRECTORY: usize = 1;

    fn declared_variable_length(file: &[u8], at: usize) -> usize {
        [28, 30, 32]
            .map(|field| usize::from(le16(file, at + field)))
            .iter()
            .sum()
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn the_record_walk_reads_exactly_the_directory_or_refuses() {
        let file: [u8; TWO_SHORT_RECORDS + PAST_THE_DIRECTORY] = kani::any();
        let file_length: usize = kani::any();
        kani::assume(file_length <= file.len());
        let directory_length: usize = kani::any();
        kani::assume(directory_length <= TWO_SHORT_RECORDS);
        let first = declared_variable_length(&file, 0);
        kani::assume(first <= SHORT);
        kani::assume(declared_variable_length(&file, CENTRAL_ENTRY + first) <= SHORT);
        let start: u64 = kani::any();
        let extent = Extent {
            count: kani::any(),
            bytes: directory_length as u64,
            start,
            end: start.wrapping_add(directory_length as u64),
            disk: 0,
            start_disk: 0,
            count_here: 0,
        };
        kani::assume(check_extent(&extent).is_ok());
        let mut reader = &file[..file_length];
        let accepts_only_whole_headers = |record: &[u8], _| {
            field_check(
                record.len() >= CENTRAL_ENTRY && kani::any(),
                "any refusal of a record",
            )
        };
        let outcome = walk_records(&mut reader, extent, accepts_only_whole_headers);
        let consumed = file_length - reader.len();
        assert!(consumed <= directory_length);
        if let Ok(entries) = outcome {
            assert!(consumed == directory_length);
            assert!(entries.len() as u64 == extent.count);
        }
    }

    const SHORT_RECORD: usize = CENTRAL_ENTRY + SHORT;

    #[kani::proof]
    #[kani::unwind(6)]
    fn every_short_record_becomes_an_entry_or_a_refusal() {
        let bytes: [u8; SHORT_RECORD] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= SHORT_RECORD);
        let any_utf8_verdict = |_: &[u8]| kani::any();
        let position: u64 = kani::any();
        if let Ok(entry) = central_entry(&bytes[..length], position, any_utf8_verdict) {
            assert!(CENTRAL_ENTRY + entry.stored_name.len() <= length);
            assert!(entry.header_start.checked_add(position).is_some());
        }
    }

    const TAIL: usize = 26;

    #[kani::proof]
    #[kani::unwind(27)]
    fn the_end_record_is_the_last_signature_and_reaches_the_end() {
        let bytes: [u8; TAIL] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= TAIL);
        let tail = &bytes[..length];
        if let Ok(end) = end_record(tail) {
            assert!(tail[end..end + 4] == *END_SIGNATURE);
            assert!(end + END_RECORD + usize::from(le16(tail, end + 20)) == tail.len());
            for later in end + 1..tail.len().saturating_sub(3) {
                assert!(tail[later..later + 4] != *END_SIGNATURE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Cursor, Write},
        path::Path,
    };

    #[derive(Debug, PartialEq)]
    struct Listed {
        name: String,
        name_raw: Vec<u8>,
        size: u64,
        compressed_size: u64,
        method: u16,
        crc32: u32,
        encrypted: bool,
        external_attributes: u32,
        header_start: u64,
        is_dir: bool,
        is_symlink: bool,
        unix_mode: Option<u32>,
    }

    fn listed_by_us(entry: &CentralEntry) -> Listed {
        Listed {
            name: entry.name().into_owned(),
            name_raw: entry.name_raw().to_vec(),
            size: entry.size,
            compressed_size: entry.compressed_size,
            method: entry.method,
            crc32: entry.crc32,
            encrypted: entry.encrypted(),
            external_attributes: entry.external_attributes,
            header_start: entry.header_start,
            is_dir: entry.is_dir(),
            is_symlink: entry.is_symlink(),
            unix_mode: entry.unix_mode_like_zip_crate(),
        }
    }

    #[expect(
        deprecated,
        reason = "the zip crate names other methods only behind features this build leaves off"
    )]
    fn method_code(method: zip::CompressionMethod) -> u16 {
        method.to_u16()
    }

    struct ZipCrateListing {
        start: u64,
        offset: u64,
        entries: Vec<Listed>,
    }

    fn listed_by_zip_crate(bytes: &[u8]) -> zip::result::ZipResult<ZipCrateListing> {
        let archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let file = archive.by_index_data(index)?;
            entries.push(Listed {
                name: file.name()?.into_owned(),
                name_raw: file.name_raw().to_vec(),
                size: file.size(),
                compressed_size: file.compressed_size(),
                method: method_code(file.compression()),
                crc32: file.crc32(),
                encrypted: file.encrypted(),
                external_attributes: file.external_attributes(),
                header_start: file.header_start(),
                is_dir: file.is_dir(),
                is_symlink: file.is_symlink(),
                unix_mode: file.unix_mode(),
            });
        }
        Ok(ZipCrateListing {
            start: archive.central_directory_start(),
            offset: archive.offset(),
            entries,
        })
    }

    fn merged_like_zip_crate(entries: Vec<Listed>) -> Vec<Listed> {
        let mut merged: Vec<Listed> = Vec::new();
        for entry in entries {
            match merged
                .iter_mut()
                .find(|seen| seen.name_raw == entry.name_raw)
            {
                Some(seen) => *seen = entry,
                None => merged.push(entry),
            }
        }
        merged
    }

    const REFUSALS_BEFORE_THE_ZIP_CRATE: [&str; 22] = [
        "truncated ZIP; if the file is another format or a split volume, pass --zip-engine backend",
        "ZIP end record not found; if the file is another format or a split volume, pass --zip-engine backend",
        "ZIP has ambiguous end records",
        MULTI_DISK,
        "missing ZIP64 locator",
        "invalid ZIP64 locator",
        "unsupported ZIP64 extension or offset",
        "unsupported ZIP64 record",
        MULTI_DISK_ZIP64,
        "ZIP metadata limit exceeded",
        "unsupported ZIP central-directory extent",
        "truncated ZIP central entry",
        "invalid ZIP central entry",
        "ZIP central entry exceeds directory",
        "truncated ZIP extra field",
        "ZIP directory count mismatch",
        "ZIP entry has two ZIP64 extra fields",
        "ZIP name flagged as UTF-8 is not UTF-8",
        "malformed ZIP Unicode path field",
        "ZIP Unicode path field renames an entry",
        "ZIP64 end record disagrees with the end record",
        "ZIP has a ZIP64 locator its end record does not use",
    ];
    const REPEATED_NAME_REFUSAL: &str = "duplicate ZIP path";

    const PREFIX_REFUSALS: [&str; 3] = [
        "ZIP has ",
        "ZIP offsets count ",
        "ZIP64 locator and central directory count ",
    ];

    fn refused_before_the_zip_crate(message: &str) -> bool {
        REFUSALS_BEFORE_THE_ZIP_CRATE.contains(&message)
            || PREFIX_REFUSALS
                .iter()
                .any(|refusal| message.starts_with(refusal))
    }

    fn native_listing(bytes: &[u8]) -> std::result::Result<(), String> {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("case.zip");
        std::fs::write(&path, bytes).unwrap();
        crate::native_zip::list(&path)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Outcome {
        BothRefuse,
        RefusedBeforeTheZipCrate,
        RepeatedNameRefusedAfterListing,
        Listed,
    }

    fn compare(bytes: &[u8]) -> std::result::Result<Outcome, String> {
        let ours = read_directory(&mut Cursor::new(bytes), None);
        let theirs = listed_by_zip_crate(bytes);
        match (ours, theirs) {
            (Err(_), Err(_)) => Ok(Outcome::BothRefuse),
            (Ok(_), Err(error)) => Err(format!("we accept, the zip crate refuses: {error}")),
            (Err(ours), Ok(_)) => {
                let ours = ours.to_string();
                match native_listing(bytes) {
                    Err(native) if native == ours && refused_before_the_zip_crate(&ours) => {
                        Ok(Outcome::RefusedBeforeTheZipCrate)
                    }
                    native => Err(format!(
                        "we refuse with {ours:?}, the zip crate accepts, native_zip::list gives {native:?}"
                    )),
                }
            }
            (Ok(ours), Ok(theirs)) => {
                let listed: Vec<_> = ours.entries.iter().map(listed_by_us).collect();
                let raw_count = listed.len();
                let merged = merged_like_zip_crate(listed);
                let repeated_names = merged.len() != raw_count;
                if theirs.offset != 0 && theirs.offset != ours.archive_start {
                    return Err(format!("the zip crate finds offset {}", theirs.offset));
                }
                if ours.start != theirs.start {
                    return Err(format!(
                        "directory start {} against {}",
                        ours.start, theirs.start
                    ));
                }
                if let Some((index, (ours, theirs))) = merged
                    .iter()
                    .zip(&theirs.entries)
                    .enumerate()
                    .find(|(_, (ours, theirs))| ours != theirs)
                {
                    return Err(format!("entry {index}: {ours:?} against {theirs:?}"));
                }
                if merged.len() != theirs.entries.len() {
                    return Err(format!(
                        "{} entries against {}",
                        merged.len(),
                        theirs.entries.len()
                    ));
                }
                match native_listing(bytes) {
                    Err(native) if repeated_names && native == REPEATED_NAME_REFUSAL => {
                        Ok(Outcome::RepeatedNameRefusedAfterListing)
                    }
                    _ if repeated_names => Err("a repeated name was not refused".to_owned()),
                    Err(native) if refused_before_the_zip_crate(&native) => Err(format!(
                        "we accept, native_zip::list refuses with {native:?}"
                    )),
                    _ => Ok(Outcome::Listed),
                }
            }
        }
    }

    #[derive(Clone)]
    struct Spec {
        name: Vec<u8>,
        made_by: u16,
        flags: u16,
        method: u16,
        dos_time: u32,
        crc32: u32,
        compressed_size: u32,
        size: u32,
        external_attributes: u32,
        header_start: Option<u32>,
        extra: Vec<u8>,
        comment: Vec<u8>,
    }

    const PAYLOAD: &[u8] = b"data";

    fn file(name: &str) -> Spec {
        Spec {
            name: name.as_bytes().to_vec(),
            made_by: 20,
            flags: 0,
            method: 0,
            dos_time: 0x5a21_6000,
            crc32: crc32fast::hash(PAYLOAD),
            compressed_size: PAYLOAD.len() as u32,
            size: PAYLOAD.len() as u32,
            external_attributes: 0,
            header_start: None,
            extra: Vec::new(),
            comment: Vec::new(),
        }
    }

    fn named(name: &[u8]) -> Spec {
        Spec {
            name: name.to_vec(),
            ..file("")
        }
    }

    fn unix(spec: Spec, mode: u32) -> Spec {
        Spec {
            made_by: 0x0314,
            external_attributes: mode << 16,
            ..spec
        }
    }

    fn with_extra(spec: Spec, fields: &[(u16, &[u8])]) -> Spec {
        let mut extra = spec.extra.clone();
        for (id, body) in fields {
            extra.extend_from_slice(&id.to_le_bytes());
            extra.extend_from_slice(&(body.len() as u16).to_le_bytes());
            extra.extend_from_slice(body);
        }
        Spec { extra, ..spec }
    }

    fn unicode_path(stored: &[u8], crc: Option<u32>, path: &[u8]) -> Vec<u8> {
        let mut body = vec![1];
        body.extend_from_slice(&crc.unwrap_or_else(|| crc32fast::hash(stored)).to_le_bytes());
        body.extend_from_slice(path);
        body
    }

    fn cp437_path(stored: &[u8]) -> Vec<u8> {
        unicode_path(stored, None, decode_name_like_zip_crate(stored).as_bytes())
    }

    fn values(values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    #[derive(Clone, Default)]
    struct End {
        zip64: Option<Zip64End>,
        comment: Vec<u8>,
    }

    #[derive(Clone)]
    struct Zip64End {
        sentinels: [bool; 3],
        size_field: u64,
        extensible: Vec<u8>,
        disks: u32,
    }

    fn zip64_end(sentinels: [bool; 3]) -> End {
        End {
            zip64: Some(Zip64End {
                sentinels,
                size_field: ZIP64_END_RECORD_SIZE_FIELD,
                extensible: Vec::new(),
                disks: 1,
            }),
            comment: Vec::new(),
        }
    }

    fn build(entries: &[Spec], end: &End) -> Vec<u8> {
        let mut out = Vec::new();
        let mut offsets = Vec::new();
        for entry in entries {
            offsets.push(out.len() as u32);
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&entry.flags.to_le_bytes());
            out.extend_from_slice(&entry.method.to_le_bytes());
            out.extend_from_slice(&entry.dos_time.to_le_bytes());
            out.extend_from_slice(&entry.crc32.to_le_bytes());
            out.extend_from_slice(&(PAYLOAD.len() as u32).to_le_bytes());
            out.extend_from_slice(&(PAYLOAD.len() as u32).to_le_bytes());
            out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&entry.name);
            out.extend_from_slice(PAYLOAD);
        }
        let start = out.len() as u64;
        for (entry, offset) in entries.iter().zip(offsets) {
            out.extend_from_slice(CENTRAL_SIGNATURE);
            out.extend_from_slice(&entry.made_by.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&entry.flags.to_le_bytes());
            out.extend_from_slice(&entry.method.to_le_bytes());
            out.extend_from_slice(&entry.dos_time.to_le_bytes());
            out.extend_from_slice(&entry.crc32.to_le_bytes());
            out.extend_from_slice(&entry.compressed_size.to_le_bytes());
            out.extend_from_slice(&entry.size.to_le_bytes());
            for length in [entry.name.len(), entry.extra.len(), entry.comment.len()] {
                out.extend_from_slice(&(length as u16).to_le_bytes());
            }
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&entry.external_attributes.to_le_bytes());
            out.extend_from_slice(&entry.header_start.unwrap_or(offset).to_le_bytes());
            out.extend_from_slice(&entry.name);
            out.extend_from_slice(&entry.extra);
            out.extend_from_slice(&entry.comment);
        }
        let bytes = out.len() as u64 - start;
        let count = entries.len() as u64;
        let mut fields = [count, bytes, start];
        if let Some(zip64) = &end.zip64 {
            let record_start = out.len() as u64;
            out.extend_from_slice(ZIP64_END_SIGNATURE);
            out.extend_from_slice(&zip64.size_field.to_le_bytes());
            out.extend_from_slice(&[45, 3, 45, 0]);
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&values(&[count, count, bytes, start]));
            out.extend_from_slice(&zip64.extensible);
            out.extend_from_slice(ZIP64_LOCATOR_SIGNATURE);
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&record_start.to_le_bytes());
            out.extend_from_slice(&zip64.disks.to_le_bytes());
            for (field, sentinel) in fields.iter_mut().zip(zip64.sentinels) {
                if sentinel {
                    *field = u64::MAX;
                }
            }
        }
        out.extend_from_slice(END_SIGNATURE);
        out.extend_from_slice(&[0; 4]);
        let count = fields[0].min(ZIP64_COUNT_SENTINEL) as u16;
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&(fields[1].min(u64::from(ZIP64_SENTINEL)) as u32).to_le_bytes());
        out.extend_from_slice(&(fields[2].min(u64::from(ZIP64_SENTINEL)) as u32).to_le_bytes());
        out.extend_from_slice(&(end.comment.len() as u16).to_le_bytes());
        out.extend_from_slice(&end.comment);
        out
    }

    fn built_archives() -> Vec<(String, Vec<u8>)> {
        let plain = End::default();
        let cp437 = b"caf\x82 \x80\xff".to_vec();
        let all_high_bytes: Vec<u8> = (0x80..=0xff).collect();
        let shapes: Vec<(&str, Vec<Spec>, End)> = vec![
            ("empty", vec![], plain.clone()),
            ("one file", vec![file("a.txt")], plain.clone()),
            (
                "folders, links and modes",
                vec![
                    unix(file("d/"), 0o040755),
                    unix(file("d/link"), 0o120777),
                    unix(file("d/socket"), 0o140644),
                    unix(file("d/whiteout"), 0o160644),
                    unix(file("d/setuid"), 0o104755),
                    unix(file("d/plain"), 0),
                    Spec {
                        external_attributes: 0x10,
                        ..file("dos-folder/")
                    },
                    Spec {
                        external_attributes: 0x01,
                        ..file("dos-read-only")
                    },
                    Spec {
                        made_by: 0x0a14,
                        external_attributes: 0o100600 << 16 | 0x20,
                        ..file("ntfs-made")
                    },
                    Spec {
                        made_by: 0x0a14,
                        external_attributes: 0x20,
                        ..file("ntfs-no-mode")
                    },
                    file("back\\slash\\"),
                ],
                plain.clone(),
            ),
            (
                "names in CP437 and UTF-8",
                vec![
                    named(&cp437),
                    named(&all_high_bytes),
                    Spec {
                        flags: UTF8_NAME,
                        ..file("café/ü.txt")
                    },
                ],
                plain.clone(),
            ),
            (
                "a flagged name that is not UTF-8",
                vec![Spec {
                    flags: UTF8_NAME,
                    ..named(&cp437)
                }],
                plain.clone(),
            ),
            (
                "Unicode paths",
                vec![
                    with_extra(named(&cp437), &[(UNICODE_PATH, &cp437_path(&cp437))]),
                    with_extra(
                        file("same.txt"),
                        &[(
                            UNICODE_PATH,
                            &unicode_path(b"same.txt", Some(7), b"same.txt"),
                        )],
                    ),
                    with_extra(
                        file("twice.txt"),
                        &[
                            (
                                UNICODE_PATH,
                                &unicode_path(b"twice.txt", None, b"twice.txt"),
                            ),
                            (
                                UNICODE_PATH,
                                &unicode_path(b"twice.txt", Some(1), b"twice.txt"),
                            ),
                        ],
                    ),
                ],
                plain.clone(),
            ),
            (
                "a Unicode path that renames",
                vec![with_extra(
                    file("a.txt"),
                    &[(UNICODE_PATH, &unicode_path(b"a.txt", None, b"b.txt"))],
                )],
                plain.clone(),
            ),
            (
                "a CP437 Unicode path with a wrong checksum",
                vec![with_extra(
                    named(&cp437),
                    &[(
                        UNICODE_PATH,
                        &unicode_path(
                            &cp437,
                            Some(0),
                            decode_name_like_zip_crate(&cp437).as_bytes(),
                        ),
                    )],
                )],
                plain.clone(),
            ),
            (
                "a Unicode path that is not UTF-8",
                vec![with_extra(
                    file("a.txt"),
                    &[(UNICODE_PATH, &unicode_path(b"a.txt", Some(9), b"\xff"))],
                )],
                plain.clone(),
            ),
            (
                "a short Unicode path",
                vec![with_extra(file("a.txt"), &[(UNICODE_PATH, &[1, 2, 3])])],
                plain.clone(),
            ),
            (
                "Unicode comments",
                vec![
                    with_extra(file("long"), &[(UNICODE_COMMENT, &[1, 0, 0, 0, 0, b'x'])]),
                    with_extra(file("short"), &[(UNICODE_COMMENT, &[1, 0])]),
                ],
                plain.clone(),
            ),
            (
                "timestamps",
                vec![
                    with_extra(file("one"), &[(EXTENDED_TIMESTAMP, &[1])]),
                    with_extra(file("five"), &[(EXTENDED_TIMESTAMP, &[1, 2, 3, 4, 5])]),
                    with_extra(file("thirteen"), &[(EXTENDED_TIMESTAMP, &[7; 13])]),
                    with_extra(file("ntfs"), &[(NTFS, &[0; 32])]),
                    with_extra(file("odd ntfs"), &[(NTFS, &[1; 7])]),
                ],
                plain.clone(),
            ),
            (
                "an empty timestamp",
                vec![with_extra(file("zero"), &[(EXTENDED_TIMESTAMP, &[])])],
                plain.clone(),
            ),
            (
                "alignment fields",
                vec![
                    with_extra(file("two"), &[(ALIGNMENT, &[4, 0])]),
                    with_extra(file("android"), &[(ANDROID_ALIGNMENT, &[4, 0, 0, 0])]),
                    with_extra(file("aes"), &[(AES, &[2, 0, b'A', b'E', 3, 0, 0])]),
                    with_extra(file("short aes"), &[(AES, &[])]),
                    with_extra(file("unknown"), &[(0x4242, &[])]),
                ],
                plain.clone(),
            ),
            (
                "a short alignment field",
                vec![with_extra(file("one"), &[(ALIGNMENT, &[4])])],
                plain.clone(),
            ),
            (
                "a truncated extra field",
                vec![Spec {
                    extra: vec![0x42, 0x42, 9, 0, 1],
                    ..file("a")
                }],
                plain.clone(),
            ),
            (
                "trailing extra bytes",
                vec![
                    Spec {
                        extra: vec![0x55],
                        ..file("one")
                    },
                    Spec {
                        extra: vec![0x42, 0x42, 0],
                        ..file("unknown")
                    },
                ],
                plain.clone(),
            ),
            (
                "trailing bytes of a known field",
                vec![Spec {
                    extra: vec![0x55, 0x54],
                    ..file("timestamp")
                }],
                plain.clone(),
            ),
            (
                "ZIP64 fields",
                vec![
                    with_extra(
                        Spec {
                            size: ZIP64_SENTINEL,
                            compressed_size: ZIP64_SENTINEL,
                            ..file("both")
                        },
                        &[(ZIP64, &values(&[4, 4]))],
                    ),
                    with_extra(
                        Spec {
                            compressed_size: ZIP64_SENTINEL,
                            ..file("compressed")
                        },
                        &[(ZIP64, &values(&[4, 99, 98]))],
                    ),
                    with_extra(
                        Spec {
                            header_start: Some(ZIP64_SENTINEL),
                            ..file("offset")
                        },
                        &[(ZIP64, &values(&[1 << 40]))],
                    ),
                    with_extra(file("unasked"), &[(ZIP64, &values(&[5, 6, 7]))]),
                    Spec {
                        size: ZIP64_SENTINEL,
                        ..file("no field")
                    },
                ],
                plain.clone(),
            ),
            (
                "a short ZIP64 field",
                vec![with_extra(
                    Spec {
                        size: ZIP64_SENTINEL,
                        compressed_size: ZIP64_SENTINEL,
                        ..file("short")
                    },
                    &[(ZIP64, &values(&[4]))],
                )],
                plain.clone(),
            ),
            (
                "two ZIP64 fields",
                vec![with_extra(
                    file("twice"),
                    &[(ZIP64, &values(&[1])), (ZIP64, &values(&[2]))],
                )],
                plain.clone(),
            ),
            (
                "an offset that overflows",
                vec![with_extra(
                    Spec {
                        header_start: Some(ZIP64_SENTINEL),
                        ..file("far")
                    },
                    &[(ZIP64, &values(&[u64::MAX - 20]))],
                )],
                plain.clone(),
            ),
            (
                "flags, methods and comments",
                vec![
                    Spec {
                        flags: ENCRYPTED,
                        ..file("locked")
                    },
                    Spec {
                        flags: 0x0008,
                        method: 8,
                        ..file("described")
                    },
                    Spec {
                        method: 99,
                        comment: b"\x80 comment".to_vec(),
                        ..file("aes method")
                    },
                    Spec {
                        method: 0x4242,
                        ..file("unknown method")
                    },
                ],
                End {
                    comment: b"archive comment".to_vec(),
                    ..End::default()
                },
            ),
            (
                "repeated names",
                vec![file("same"), file("other"), unix(file("same"), 0o100600)],
                plain.clone(),
            ),
            (
                "a name repeated through a Unicode path",
                vec![
                    named(&cp437),
                    with_extra(named(&cp437), &[(UNICODE_PATH, &cp437_path(&cp437))]),
                ],
                plain.clone(),
            ),
            (
                "a comment holding an end signature",
                vec![file("a")],
                End {
                    comment: b"x PK\x05\x06 y".to_vec(),
                    ..End::default()
                },
            ),
            (
                "a ZIP64 end",
                vec![file("a"), file("b")],
                zip64_end([true, false, false]),
            ),
            (
                "a ZIP64 end with every sentinel",
                vec![file("a")],
                zip64_end([true, true, true]),
            ),
            (
                "an empty ZIP64 end",
                vec![],
                zip64_end([false, false, true]),
            ),
            (
                "a ZIP64 end without sentinels",
                vec![file("a")],
                zip64_end([false, false, false]),
            ),
            (
                "a ZIP64 end with extensible data",
                vec![file("a")],
                End {
                    zip64: Some(Zip64End {
                        sentinels: [true, true, true],
                        size_field: ZIP64_END_RECORD_SIZE_FIELD + 4,
                        extensible: vec![1, 2, 3, 4],
                        disks: 1,
                    }),
                    comment: Vec::new(),
                },
            ),
            (
                "a ZIP64 locator for two disks",
                vec![file("a")],
                End {
                    zip64: Some(Zip64End {
                        sentinels: [true, true, true],
                        size_field: ZIP64_END_RECORD_SIZE_FIELD,
                        extensible: Vec::new(),
                        disks: 2,
                    }),
                    comment: Vec::new(),
                },
            ),
        ];
        let mut archives: Vec<(String, Vec<u8>)> = shapes
            .into_iter()
            .map(|(name, entries, end)| (name.to_owned(), build(&entries, &end)))
            .collect();
        let mut trailing = build(&[file("a")], &plain);
        trailing.extend_from_slice(b"trailing");
        archives.push(("trailing data".to_owned(), trailing));
        let mut prefixed = b"#!/bin/sh\n".to_vec();
        prefixed.extend_from_slice(&build(&[file("a")], &plain));
        archives.push(("a self-extractor prefix".to_owned(), prefixed));
        archives.push((
            "a ZIP64 end that disagrees with its end record".to_owned(),
            disagreeing_zip64_end(),
        ));
        archives.push((
            "a ZIP64 end whose end record names another offset".to_owned(),
            zip64_end_with_another_offset(),
        ));
        archives.push((
            "a comment ending in a ZIP64 locator".to_owned(),
            build(&[file("a"), comment_ending_in_a_zip64_locator()], &plain),
        ));
        archives.extend(written_by_zip_crate());
        archives
    }

    fn disagreeing_zip64_end() -> Vec<u8> {
        let mut archive = build(&[file("a"), file("b")], &zip64_end([false, false, false]));
        let count = archive.len() - END_RECORD + 8;
        archive[count..count + 4].copy_from_slice(&[1, 0, 1, 0]);
        archive
    }

    fn zip64_end_with_another_offset() -> Vec<u8> {
        let mut archive = build(&[file("a"), file("b")], &zip64_end([true, false, false]));
        let offset = archive.len() - END_RECORD + 16;
        archive[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
        archive
    }

    fn comment_ending_in_a_zip64_locator() -> Spec {
        let mut comment = ZIP64_END_SIGNATURE.to_vec();
        comment.resize(ZIP64_END_RECORD, 0);
        comment.extend_from_slice(ZIP64_LOCATOR_SIGNATURE);
        comment.extend_from_slice(&values(&[0]));
        comment.extend_from_slice(&1u32.to_le_bytes());
        comment.extend_from_slice(&1u32.to_le_bytes());
        Spec {
            comment,
            ..file("b")
        }
    }

    fn written_by_zip_crate() -> Vec<(String, Vec<u8>)> {
        use zip::{CompressionMethod, write::SimpleFileOptions};
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.add_directory("folder/", options).unwrap();
        writer
            .start_file("folder/large.bin", options.large_file(true))
            .unwrap();
        writer.write_all(b"large").unwrap();
        writer
            .start_file("mode.txt", options.unix_permissions(0o4755))
            .unwrap();
        writer.add_symlink("link", "mode.txt", options).unwrap();
        let mut archives = vec![(
            "written by the zip crate".to_owned(),
            writer.finish().unwrap().into_inner(),
        )];
        let mut many = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..=ZIP64_COUNT_SENTINEL {
            many.start_file(format!("{index}"), options).unwrap();
        }
        archives.push((
            "more entries than a ZIP32 count holds".to_owned(),
            many.finish().unwrap().into_inner(),
        ));
        archives
    }

    #[test]
    fn a_record_buffer_grows_only_to_the_record() {
        let directory = [0x55; 3 * CENTRAL_ENTRY];
        for wanted in [1, CENTRAL_ENTRY, 2 * CENTRAL_ENTRY] {
            let mut reader = &directory[..];
            let mut bytes = DirectoryBytes {
                reader: &mut reader,
                remaining: directory.len() as u64,
            };
            let mut record = Vec::new();
            assert!(bytes.append(wanted, &mut record).is_ok());
            assert_eq!(record.len(), wanted);
            assert_eq!(record.capacity(), wanted);
        }
    }

    #[test]
    fn the_proofs_checksum_matches_crc32fast() {
        let mut inputs: Vec<Vec<u8>> = (0..=u16::MAX)
            .flat_map(|pair| {
                let [low, high] = pair.to_le_bytes();
                [vec![low], vec![low, high]]
            })
            .collect();
        inputs.push(Vec::new());
        inputs.push(b"123456789".to_vec());
        inputs.push((0..=255).collect());
        for input in inputs {
            assert_eq!(
                reference_crc32(&input),
                crc32fast::hash(&input),
                "{input:?}"
            );
        }
    }

    #[test]
    fn entries_keep_their_stored_names_and_times() {
        let cp437 = b"caf\x82".to_vec();
        let specs = [
            Spec {
                dos_time: 0xffff_ffff,
                ..with_extra(file("odd time"), &[(EXTENDED_TIMESTAMP, &[1, 2, 3, 4, 5])])
            },
            Spec {
                flags: 0x0808,
                comment: b"comment".to_vec(),
                ..unix(file("flags"), 0o100644)
            },
            with_extra(named(&cp437), &[(UNICODE_PATH, &cp437_path(&cp437))]),
        ];
        let directory =
            read_directory(&mut Cursor::new(build(&specs, &End::default())), None).unwrap();
        assert_eq!(directory.entries.len(), specs.len());
        for (entry, spec) in directory.entries.iter().zip(&specs) {
            assert_eq!(*entry.stored_name, *spec.name);
            assert_eq!(
                entry.time,
                crate::native_zip::entry_time(&spec.extra, spec.dos_time)
            );
            assert_eq!(entry.made_by, spec.made_by);
            assert_eq!(entry.flags, spec.flags);
        }
        assert_eq!(
            directory.entries[2].unicode_name.as_deref(),
            Some("café".as_bytes())
        );
    }

    fn real_archives() -> Vec<(String, Vec<u8>)> {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut archives = Vec::new();
        for folder in ["zip-corpus", "parity/archives"] {
            let mut paths: Vec<_> = std::fs::read_dir(data.join(folder))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "zip"))
                .collect();
            paths.sort();
            for path in paths {
                let bytes = std::fs::read(&path).unwrap();
                let name = if path.with_extension("z01").exists() {
                    "the last volume of a split set read alone".to_owned()
                } else {
                    path.display().to_string()
                };
                archives.push((name, bytes));
            }
        }
        assert!(archives.len() >= 30, "the ZIP corpora are missing");
        archives
    }

    const BUILT_REFUSALS: [(&str, Outcome); 16] = [
        (
            "a flagged name that is not UTF-8",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a CP437 Unicode path with a wrong checksum",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a Unicode path that renames",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a Unicode path that is not UTF-8",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        ("a short Unicode path", Outcome::BothRefuse),
        ("an empty timestamp", Outcome::BothRefuse),
        ("a short alignment field", Outcome::BothRefuse),
        ("a truncated extra field", Outcome::BothRefuse),
        ("trailing bytes of a known field", Outcome::BothRefuse),
        ("a short ZIP64 field", Outcome::BothRefuse),
        ("two ZIP64 fields", Outcome::RefusedBeforeTheZipCrate),
        ("an offset that overflows", Outcome::BothRefuse),
        ("repeated names", Outcome::RepeatedNameRefusedAfterListing),
        (
            "a ZIP64 end that disagrees with its end record",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a ZIP64 end whose end record names another offset",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a comment ending in a ZIP64 locator",
            Outcome::RefusedBeforeTheZipCrate,
        ),
    ];
    const MORE_BUILT_REFUSALS: [(&str, Outcome); 6] = [
        ("Unicode comments", Outcome::BothRefuse),
        (
            "a comment holding an end signature",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        (
            "a ZIP64 end with extensible data",
            Outcome::RefusedBeforeTheZipCrate,
        ),
        ("a ZIP64 locator for two disks", Outcome::BothRefuse),
        ("trailing data", Outcome::RefusedBeforeTheZipCrate),
        (
            "the last volume of a split set read alone",
            Outcome::RefusedBeforeTheZipCrate,
        ),
    ];

    #[test]
    fn every_archive_lists_as_the_zip_crate_lists_it_or_both_refuse() {
        let mut failures = Vec::new();
        for (name, bytes) in real_archives().into_iter().chain(built_archives()) {
            let expected = BUILT_REFUSALS
                .iter()
                .chain(&MORE_BUILT_REFUSALS)
                .find(|(refused, _)| *refused == name)
                .map_or(Outcome::Listed, |&(_, outcome)| outcome);
            match compare(&bytes) {
                Ok(outcome) if outcome == expected => {}
                outcome => failures.push(format!("{name}: expected {expected:?}, got {outcome:?}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound.max(1) as u64) as usize
        }
    }

    const SIGNATURES: [&[u8; 4]; 5] = [
        CENTRAL_SIGNATURE,
        END_SIGNATURE,
        ZIP64_END_SIGNATURE,
        ZIP64_LOCATOR_SIGNATURE,
        b"PK\x03\x04",
    ];
    const EDGE_VALUES: [u64; 8] = [0, 1, 4, 0x7fff, 0xffff, 0xffff_fffe, 0xffff_ffff, u64::MAX];

    fn directory_position(bytes: &[u8], rng: &mut Rng) -> usize {
        let directory = bytes
            .windows(4)
            .position(|window| window == CENTRAL_SIGNATURE)
            .unwrap_or(0);
        if rng.below(4) == 0 {
            rng.below(bytes.len())
        } else {
            directory + rng.below(bytes.len() - directory)
        }
    }

    fn overwrite_field_near_signature(bytes: &mut [u8], rng: &mut Rng) {
        let signature = SIGNATURES[rng.below(SIGNATURES.len())];
        let starts: Vec<usize> = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, window)| window == signature)
            .map(|(at, _)| at)
            .collect();
        if starts.is_empty() {
            return;
        }
        let at = starts[rng.below(starts.len())] + 4 + rng.below(52);
        let value = EDGE_VALUES[rng.below(EDGE_VALUES.len())].to_le_bytes();
        let width = [1, 2, 4, 8][rng.below(4)];
        for (offset, byte) in value.iter().take(width).enumerate() {
            if let Some(slot) = bytes.get_mut(at + offset) {
                *slot = *byte;
            }
        }
    }

    fn mutate(archive: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut bytes = archive.to_vec();
        for _ in 0..1 + rng.below(3) {
            if bytes.is_empty() {
                break;
            }
            let at = directory_position(&bytes, rng);
            match rng.below(6) {
                0 => bytes[at] ^= 1 << rng.below(8),
                1 => bytes[at] = EDGE_VALUES[rng.below(EDGE_VALUES.len())] as u8,
                2 => bytes.truncate(at),
                3 => {
                    let count = (1 + rng.below(8)).min(bytes.len() - at);
                    bytes.drain(at..at + count);
                }
                4 => {
                    let insert: Vec<u8> = (0..1 + rng.below(8)).map(|_| rng.next() as u8).collect();
                    bytes.splice(at..at, insert);
                }
                _ => overwrite_field_near_signature(&mut bytes, rng),
            }
        }
        bytes
    }

    const LARGE_ARCHIVE: usize = 1 << 20;

    #[test]
    fn every_damaged_archive_lists_as_the_zip_crate_lists_it_or_both_refuse() {
        let cases: u64 =
            std::env::var("ZIPPER_DIRECTORY_CASES").map_or(300, |cases| cases.parse().unwrap());
        let seed: u64 = std::env::var("ZIPPER_DIRECTORY_SEED")
            .map_or(0x5eed_d1ec, |seed| seed.parse().unwrap());
        let mut failures = Vec::new();
        let mut outcomes = std::collections::HashMap::new();
        for (name, archive) in real_archives().into_iter().chain(built_archives()) {
            let cases = if archive.len() > LARGE_ARCHIVE {
                cases / 30
            } else {
                cases
            };
            let archive_seed = seed ^ u64::from(crc32fast::hash(name.as_bytes()));
            for case in 0..cases {
                let case_seed = archive_seed ^ case << 32 | 1;
                let damaged = mutate(&archive, &mut Rng(case_seed));
                match compare(&damaged) {
                    Ok(outcome) => *outcomes.entry(outcome).or_insert(0) += 1,
                    Err(failure) => {
                        failures.push(format!("{name} case seed {case_seed}: {failure}"))
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} disagreements:\n{}",
            failures.len(),
            failures.join("\n")
        );
        println!("outcomes of damaged archives: {outcomes:?}");
        assert_eq!(
            outcomes.len(),
            4,
            "a kind of outcome never happened: {outcomes:?}"
        );
    }
}
