//! In-process extraction of Microsoft Cabinet files and cabinet sets with
//! stored, MSZIP and Quantum folders. See `docs/reference/cab-reader.md`
//! for what it accepts and restores.
pub use crate::tree::ReadOptions;
use crate::tree::{self, BACKEND_HINT, MAX_NAME};
use crate::volumes::{ArchiveFile, Unchanged};
use crate::{BadField, Entry, Field, field_check, quantum};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Seek},
    path::{Path, PathBuf},
    time::SystemTime,
};

const SIGNATURE: &[u8; 8] = b"MSCF\0\0\0\0";
const HEADER: usize = 36;
const RESERVE_SIZES: usize = 4;
const FOLDER: usize = 8;
const FILE: usize = 16;
const DATA: usize = 8;
const PREVIOUS_CABINET: u16 = 0x0001;
const NEXT_CABINET: u16 = 0x0002;
const RESERVE_PRESENT: u16 = 0x0004;
const KNOWN_FLAGS: u16 = PREVIOUS_CABINET | NEXT_CABINET | RESERVE_PRESENT;
const CONTINUED_FROM_PREVIOUS: u16 = 0xFFFD;
const CONTINUED_TO_NEXT: u16 = 0xFFFE;
const CONTINUED_BOTH_WAYS: u16 = 0xFFFF;
const MAX_SET_RECORDS: usize = u16::MAX as usize;
const BLOCK: usize = 32 * 1024;
// SPEC(MS-CAB CFDATA): a compressed block may grow by up to 6,144 bytes.
const LARGEST_PACKED: usize = BLOCK + 6144;
const METHOD_MASK: u8 = 0x0F;
const LZX: u8 = 3;
const READ_ONLY: u16 = 0x01;
const DIRECTORY: u16 = 0x10;
const NAME_IS_UTF8: u16 = 0x80;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const MSZIP_SIGNATURE: &[u8; 2] = b"CK";
const DEFLATE_HISTORY: usize = 32 * 1024;
const MSZIP_DECODER_MEMORY: u64 = 64 * 1024;
const DECODER_MEMORY: u64 = 4 * 1024 * 1024;
const BUFFER: usize = 64 * 1024;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    cabinet_size: u32,
    files_offset: u32,
    folders: u16,
    files: u16,
    flags: u16,
    set_id: u16,
    number: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reserve {
    header: u16,
    folder: u8,
    data: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Stored,
    Mszip,
    Quantum { window_bits: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Folder {
    data_offset: u32,
    blocks: u16,
    method: Method,
    type_compress: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileRecord {
    size: u32,
    offset: u32,
    folder: u16,
    date: u16,
    time: u16,
    attributes: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DataHeader {
    checksum: u32,
    packed: u16,
    size: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Continued {
    No,
    FromPrevious,
    ToNext,
    BothWays,
}

impl Continued {
    fn continues_from_previous(self) -> bool {
        matches!(self, Continued::FromPrevious | Continued::BothWays)
    }

    fn continues_into_next(self) -> bool {
        matches!(self, Continued::ToNext | Continued::BothWays)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn header(bytes: &[u8; HEADER]) -> Field<Header> {
    field_check(&bytes[..8] == SIGNATURE, "not a CAB file")?;
    field_check(
        u32_at(bytes, 12) == 0 && u32_at(bytes, 20) == 0,
        "CAB header reserved fields are not zero",
    )?;
    let cabinet_size = u32_at(bytes, 8);
    field_check(
        cabinet_size >= HEADER as u32,
        "CAB header gives a cabinet smaller than its header",
    )?;
    let files_offset = u32_at(bytes, 16);
    field_check(
        (HEADER as u32..=cabinet_size).contains(&files_offset),
        "CAB file records lie outside the cabinet",
    )?;
    let flags = u16_at(bytes, 30);
    field_check(flags & !KNOWN_FLAGS == 0, "CAB header has unknown flags")?;
    Ok(Header {
        cabinet_size,
        files_offset,
        folders: u16_at(bytes, 26),
        files: u16_at(bytes, 28),
        flags,
        set_id: u16_at(bytes, 32),
        number: u16_at(bytes, 34),
    })
}

// EXTERNAL(7-Zip CabHandler): a later cabinet must share the set ID and carry the next
// cabinet number.
fn follows_in_set(previous: &Header, next: &Header) -> Field<()> {
    field_check(
        next.set_id == previous.set_id,
        "its set ID differs from the cabinet before it, so it belongs to another set",
    )?;
    field_check(
        previous.number.checked_add(1) == Some(next.number),
        "it is not numbered one after the cabinet that names it",
    )?;
    field_check(
        next.flags & PREVIOUS_CABINET != 0,
        "it names no previous cabinet, so it starts another set",
    )
}

fn record_folder(field: u16, folders: u16) -> Field<(u16, Continued)> {
    match field {
        CONTINUED_FROM_PREVIOUS | CONTINUED_TO_NEXT => {
            field_check(
                folders > 0,
                "it continues a folder its cabinet does not have",
            )?;
            Ok(if field == CONTINUED_TO_NEXT {
                (folders - 1, Continued::ToNext)
            } else {
                (0, Continued::FromPrevious)
            })
        }
        CONTINUED_BOTH_WAYS => {
            field_check(
                folders == 1,
                "it continues both ways, so its cabinet must hold exactly one folder",
            )?;
            Ok((0, Continued::BothWays))
        }
        _ => {
            field_check(
                field < folders,
                "it names a folder its cabinet does not have",
            )?;
            Ok((field, Continued::No))
        }
    }
}

fn reserve(bytes: &[u8; RESERVE_SIZES]) -> Reserve {
    Reserve {
        header: u16_at(bytes, 0),
        folder: bytes[2],
        data: bytes[3],
    }
}

// EXTERNAL(7-Zip CabItem): the method is the low 4 bits of typeCompress's first byte, and
// the Quantum window is its whole second byte.
fn folder(bytes: &[u8; FOLDER]) -> Field<Folder> {
    let minor = bytes[7];
    let method = match bytes[6] & METHOD_MASK {
        0 => Method::Stored,
        1 => Method::Mszip,
        2 => {
            field_check(
                minor <= quantum::MAX_WINDOW_BITS,
                "CAB Quantum window is larger than 2^21 bytes",
            )?;
            Method::Quantum { window_bits: minor }
        }
        LZX => {
            return Err(BadField(
                "CAB folder is compressed with LZX, which Zipper does not decode",
            ));
        }
        _ => return Err(BadField("CAB folder uses an unknown compression method")),
    };
    Ok(Folder {
        data_offset: u32_at(bytes, 0),
        blocks: u16_at(bytes, 4),
        method,
        type_compress: u16_at(bytes, 6),
    })
}

fn file_record(bytes: &[u8; FILE]) -> FileRecord {
    FileRecord {
        size: u32_at(bytes, 0),
        offset: u32_at(bytes, 4),
        folder: u16_at(bytes, 8),
        date: u16_at(bytes, 10),
        time: u16_at(bytes, 12),
        attributes: u16_at(bytes, 14),
    }
}

impl FileRecord {
    fn end(&self) -> u64 {
        u64::from(self.offset) + u64::from(self.size)
    }
}

// SPEC(MS-CAB CFDATA): cbUncomp is 0 in each part of a block that continues in the next
// cabinet, and the part that ends the block gives its whole size.
fn data_header(bytes: &[u8; DATA]) -> Field<DataHeader> {
    let header = DataHeader {
        checksum: u32_at(bytes, 0),
        packed: u16_at(bytes, 4),
        size: u16_at(bytes, 6),
    };
    field_check(
        usize::from(header.size) <= BLOCK,
        "CAB data block holds more than 32 KiB",
    )?;
    field_check(
        header.packed != 0 && usize::from(header.packed) <= LARGEST_PACKED,
        "CAB data block has an impossible packed size",
    )?;
    Ok(header)
}

fn joined_packed(held: usize, part: u16) -> Field<std::ops::Range<usize>> {
    let end = held.saturating_add(usize::from(part));
    field_check(
        end <= LARGEST_PACKED,
        "CAB data block split across cabinets packs more than 38,912 bytes",
    )?;
    Ok(held..end)
}

// EXTERNAL(7-Zip CabBlockInStream): the checksum XORs little-endian words, and a short tail
// with its first byte highest.
fn checksum(bytes: &[u8]) -> u32 {
    let mut words = bytes.chunks_exact(4);
    let mut sum = words.by_ref().fold(0, |sum, word| sum ^ u32_at(word, 0));
    for (index, &byte) in words.remainder().iter().enumerate() {
        let shift = 8 * (words.remainder().len() - 1 - index);
        sum ^= u32::from(byte) << shift;
    }
    sum
}

// SPEC(MS-CAB CFDATA): the checksum covers cbData through the data, reserved bytes included.
fn checksum_matches(header_and_reserve: &[u8], data: &[u8]) -> bool {
    let stored = u32_at(header_and_reserve, 0);
    stored == 0 || stored == checksum(&header_and_reserve[4..]) ^ checksum(data)
}

fn dos_fields(date: u16, time: u16) -> Option<[i64; 6]> {
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
    valid.then_some(fields)
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

fn modified(date: u16, time: u16) -> Option<SystemTime> {
    let seconds = local_seconds(dos_fields(date, time)?)?;
    let offset = std::time::Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        std::time::UNIX_EPOCH.checked_add(offset)
    } else {
        std::time::UNIX_EPOCH.checked_sub(offset)
    }
}

/// Returns whether the file starts with the CAB signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(SIGNATURE.len());
    crate::volumes::open(path)?
        .take(SIGNATURE.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == SIGNATURE)
}

fn plain_cabinet_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name != b"."
        && name != b".."
        && !name
            .iter()
            .any(|byte| matches!(byte, b'/' | b'\\' | b':' | 0))
        && !std::str::from_utf8(name).is_ok_and(crate::windows_device_name)
}

struct Item {
    name: String,
    record: FileRecord,
    cabinet: u16,
    index: u16,
    folder: u32,
}

impl Item {
    fn directory(&self) -> bool {
        self.record.attributes & DIRECTORY != 0
    }
}

struct Source {
    reader: BufReader<ArchiveFile>,
    size: u64,
}

impl Source {
    fn new(file: ArchiveFile, size: u32) -> Self {
        Source {
            reader: BufReader::with_capacity(BUFFER, file),
            size: u64::from(size),
        }
    }

    fn exact(&mut self, buffer: &mut [u8], at: &mut u64, what: &str) -> Result<()> {
        ensure!(
            *at + buffer.len() as u64 <= self.size,
            "truncated CAB {what}"
        );
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated CAB {what}"))?;
        *at += buffer.len() as u64;
        Ok(())
    }

    fn skip(&mut self, count: u64, at: &mut u64, what: &str) -> Result<()> {
        ensure!(*at + count <= self.size, "truncated CAB {what}");
        self.reader.seek_relative(count as i64)?;
        *at += count;
        Ok(())
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step =
            i64::try_from(i128::from(offset) - i128::from(here)).context("CAB offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    fn name(&mut self, at: &mut u64) -> Result<Vec<u8>> {
        let mut name = Vec::new();
        loop {
            let left = self.size - *at;
            let buffer = self.reader.fill_buf()?;
            let window = &buffer[..buffer.len().min(left as usize)];
            ensure!(!window.is_empty(), "truncated CAB name");
            let (piece, terminated) = match window.iter().position(|&byte| byte == 0) {
                Some(end) => (&window[..end], true),
                None => (window, false),
            };
            ensure!(
                name.len() + piece.len() <= MAX_NAME as usize,
                "CAB name is longer than {MAX_NAME} bytes"
            );
            name.extend_from_slice(piece);
            let used = piece.len() + usize::from(terminated);
            self.reader.consume(used);
            *at += used as u64;
            if terminated {
                return Ok(name);
            }
        }
    }

    fn set_name(&mut self, at: &mut u64, which: &str) -> Result<Vec<u8>> {
        let cabinet = self.name(at)?;
        ensure!(
            plain_cabinet_name(&cabinet),
            "CAB names its {which} cabinet {:?}, which is not a plain file name beside it",
            String::from_utf8_lossy(&cabinet)
        );
        self.name(at)?;
        Ok(cabinet)
    }
}

struct Cabinet {
    path: PathBuf,
    header: Header,
    reserve: Reserve,
    folders: Vec<Folder>,
    previous: Option<Vec<u8>>,
    next: Option<Vec<u8>>,
    unchanged: Option<Unchanged>,
    claimed: BTreeMap<u64, u64>,
    from_previous: bool,
    to_next: bool,
}

#[derive(Default)]
struct Totals {
    folders: usize,
    name_bytes: usize,
}

fn label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

impl Cabinet {
    fn label(&self) -> String {
        label(&self.path)
    }

    fn read(
        path: PathBuf,
        file: ArchiveFile,
        index: usize,
        before: Option<&Header>,
        totals: &mut Totals,
        items: &mut Vec<Item>,
    ) -> Result<(Self, Source)> {
        let length = file.len()?;
        let mut source = Source::new(file, HEADER as u32);
        let mut bytes = [0u8; HEADER];
        source
            .reader
            .read_exact(&mut bytes)
            .context("truncated CAB header")?;
        let header = header(&bytes)?;
        if let Some(before) = before {
            follows_in_set(before, &header)?;
        }
        ensure!(
            u64::from(header.cabinet_size) <= length,
            "truncated CAB file: the header gives {} bytes and the file has {length}",
            header.cabinet_size
        );
        source.size = u64::from(header.cabinet_size);
        let mut cabinet = Cabinet {
            path,
            header,
            reserve: Reserve::default(),
            folders: Vec::new(),
            previous: None,
            next: None,
            unchanged: None,
            claimed: BTreeMap::new(),
            from_previous: false,
            to_next: false,
        };
        let mut at = HEADER as u64;
        if header.flags & RESERVE_PRESENT != 0 {
            let mut sizes = [0u8; RESERVE_SIZES];
            source.exact(&mut sizes, &mut at, "reserve sizes")?;
            cabinet.reserve = reserve(&sizes);
            source.skip(u64::from(cabinet.reserve.header), &mut at, "header reserve")?;
        }
        if header.flags & PREVIOUS_CABINET != 0 {
            let previous = source.set_name(&mut at, "previous")?;
            totals.name_bytes += previous.len();
            cabinet.previous = before.is_none().then_some(previous);
        }
        if header.flags & NEXT_CABINET != 0 {
            let next = source.set_name(&mut at, "next")?;
            totals.name_bytes += next.len();
            cabinet.next = Some(next);
        }
        ensure!(
            totals.name_bytes <= MAX_NAME_BYTES,
            "CAB names exceed 64 MiB in total"
        );
        let record = FOLDER as u64 + u64::from(cabinet.reserve.folder);
        ensure!(
            u64::from(header.folders) * record <= u64::from(header.cabinet_size) - at,
            "truncated CAB folder table"
        );
        totals.folders += usize::from(header.folders);
        ensure!(
            totals.folders <= MAX_SET_RECORDS,
            "CAB set has more than {MAX_SET_RECORDS} folder records"
        );
        cabinet.folders.reserve_exact(usize::from(header.folders));
        for _ in 0..header.folders {
            let mut bytes = [0u8; FOLDER];
            source.exact(&mut bytes, &mut at, "folder record")?;
            cabinet.folders.push(folder(&bytes)?);
            source.skip(u64::from(cabinet.reserve.folder), &mut at, "folder reserve")?;
        }
        cabinet.read_files(index, &mut source, totals, items)?;
        Ok((cabinet, source))
    }

    fn read_files(
        &mut self,
        cabinet: usize,
        source: &mut Source,
        totals: &mut Totals,
        items: &mut Vec<Item>,
    ) -> Result<()> {
        let cabinet = u16::try_from(cabinet).context("CAB set has more than 65,536 cabinets")?;
        let mut at = u64::from(self.header.files_offset);
        source.seek_to(at)?;
        ensure!(
            u64::from(self.header.files) * FILE as u64 <= u64::from(self.header.cabinet_size) - at,
            "truncated CAB file table"
        );
        ensure!(
            items.len() + usize::from(self.header.files) <= MAX_SET_RECORDS,
            "CAB set has more than {MAX_SET_RECORDS} file records"
        );
        items.reserve_exact(usize::from(self.header.files));
        for index in 0..self.header.files {
            let mut bytes = [0u8; FILE];
            source.exact(&mut bytes, &mut at, "file record")?;
            let record = file_record(&bytes);
            let raw = source.name(&mut at)?;
            totals.name_bytes += raw.len();
            ensure!(
                totals.name_bytes <= MAX_NAME_BYTES,
                "CAB names exceed 64 MiB in total"
            );
            let (folder, continued) =
                record_folder(record.folder, self.header.folders).map_err(|error| {
                    if record.folder < CONTINUED_FROM_PREVIOUS {
                        anyhow::anyhow!(
                            "CAB file {index} names folder {}, and the cabinet has {}",
                            record.folder,
                            self.folders.len()
                        )
                    } else {
                        anyhow::anyhow!("CAB file {index} cannot be placed: {error}")
                    }
                })?;
            self.from_previous |= continued.continues_from_previous();
            self.to_next |= continued.continues_into_next();
            let name = String::from_utf8(raw).map_err(|_| {
                let flagged = record.attributes & NAME_IS_UTF8 != 0;
                anyhow::anyhow!(
                    "CAB file {index} has a name that is not {}; {BACKEND_HINT}",
                    if flagged {
                        "valid UTF-8"
                    } else {
                        "ASCII or UTF-8"
                    }
                )
            })?;
            items.push(Item {
                name: name.replace('\\', "/"),
                record,
                cabinet,
                index,
                folder: u32::from(folder),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Segment {
    cabinet: usize,
    folder: Folder,
}

struct Set {
    cabinets: Vec<Cabinet>,
    folders: Vec<Vec<Segment>>,
    items: Vec<Item>,
    source: Source,
    open: usize,
}

impl Set {
    fn read(path: &Path) -> Result<Self> {
        let mut totals = Totals::default();
        let mut items = Vec::new();
        let file = crate::volumes::open(path)?;
        let (first, source) =
            Cabinet::read(path.to_owned(), file, 0, None, &mut totals, &mut items)?;
        if let Some(previous) = &first.previous {
            bail!(
                "CAB file {} is cabinet {} of a set, after {:?}; open the set's first cabinet to read it",
                first.label(),
                first.header.number,
                String::from_utf8_lossy(previous)
            );
        }
        let mut set = Set {
            cabinets: vec![first],
            folders: Vec::new(),
            items,
            source,
            open: 0,
        };
        while let Some(next) = set.cabinets.last_mut().and_then(|last| last.next.take()) {
            let cabinet = set.open_next(&next, &mut totals)?;
            set.cabinets.push(cabinet);
        }
        set.join_folders()?;
        Ok(set)
    }

    fn open_next(&mut self, name: &[u8], totals: &mut Totals) -> Result<Cabinet> {
        let last = self.cabinets.last().expect("a set has its first cabinet");
        let name = std::str::from_utf8(name).with_context(|| {
            format!(
                "CAB cabinet {} names its next cabinet {:?}, which is not UTF-8",
                last.label(),
                String::from_utf8_lossy(name)
            )
        })?;
        let path = last.path.with_file_name(name);
        let named_by = || {
            format!(
                "cabinet {name}, which {} names as the next cabinet of its set",
                last.label()
            )
        };
        let (file, unchanged) = crate::volumes::open_alone(&path)
            .with_context(|| format!("cannot open {}", named_by()))?;
        let (mut cabinet, _) = Cabinet::read(
            path.clone(),
            file,
            self.cabinets.len(),
            Some(&last.header),
            totals,
            &mut self.items,
        )
        .with_context(|| format!("in {}", named_by()))?;
        cabinet.unchanged = Some(unchanged);
        Ok(cabinet)
    }

    // EXTERNAL(7-Zip CabIn): a cabinet's first folder continues the last folder of the
    // cabinet before when one of its files continues from there, with the same typeCompress.
    fn join_folders(&mut self) -> Result<()> {
        let mut first_folder = Vec::with_capacity(self.cabinets.len());
        let last = self.cabinets.len() - 1;
        for (index, cabinet) in self.cabinets.iter().enumerate() {
            let label = cabinet.label();
            match index.checked_sub(1).map(|before| &self.cabinets[before]) {
                None => ensure!(
                    !cabinet.from_previous,
                    "CAB cabinet {label} has a file that continues from a previous cabinet, and it is the first cabinet of its set"
                ),
                Some(before) => ensure!(
                    before.to_next == cabinet.from_previous,
                    "CAB cabinets {} and {label} disagree on whether a folder continues from one into the other",
                    before.label()
                ),
            }
            ensure!(
                index < last || !cabinet.to_next,
                "CAB cabinet {label} has a file that continues into a next cabinet, and it is the last cabinet of its set"
            );
            first_folder.push(self.folders.len() - usize::from(cabinet.from_previous));
            for (local, &folder) in cabinet.folders.iter().enumerate() {
                let segment = Segment {
                    cabinet: index,
                    folder,
                };
                match self.folders.last_mut() {
                    Some(joined) if local == 0 && cabinet.from_previous => {
                        ensure!(
                            joined[0].folder.type_compress == folder.type_compress,
                            "CAB cabinet {label} continues a folder with another compression method or window than the cabinet before it"
                        );
                        joined.push(segment);
                    }
                    _ => self.folders.push(vec![segment]),
                }
            }
        }
        for item in &mut self.items {
            item.folder += first_folder[usize::from(item.cabinet)] as u32;
        }
        Ok(())
    }

    fn switch_to(&mut self, index: usize) -> Result<()> {
        if self.open == index {
            return Ok(());
        }
        let cabinet = &self.cabinets[index];
        let unchanged = cabinet
            .unchanged
            .context("CAB set returns to its first cabinet")?;
        let file = crate::volumes::reopen_unchanged(&cabinet.path, unchanged)
            .with_context(|| format!("cannot reopen cabinet {}", cabinet.label()))?;
        self.source = Source::new(file, cabinet.header.cabinet_size);
        self.open = index;
        Ok(())
    }
}

// EXTERNAL(7-Zip CabIn): records sort directories first, then by folder, offset and size, and
// one that repeats the folder, offset, size and slashed name of the one before it is dropped.
fn sort_items(items: &mut Vec<Item>) {
    items.sort_by_key(|item| {
        (
            !item.directory(),
            item.folder,
            item.record.offset,
            item.record.size,
            item.cabinet,
            item.index,
        )
    });
    items.dedup_by(|later, earlier| {
        later.folder == earlier.folder
            && later.record.offset == earlier.record.offset
            && later.record.size == earlier.record.size
            && later.name == earlier.name
    });
}

fn ensure_files_do_not_overlap(items: &[Item]) -> Result<()> {
    let mut previous: Option<&Item> = None;
    for item in items.iter().filter(|item| !item.directory()) {
        if let Some(earlier) = previous.filter(|earlier| earlier.folder == item.folder) {
            ensure!(
                u64::from(item.record.offset) >= earlier.record.end(),
                "CAB files {:?} and {:?} share bytes of their folder, which Zipper does not extract",
                earlier.name,
                item.name
            );
        }
        previous = Some(item);
    }
    Ok(())
}

fn decoder_memory(method: Method) -> u64 {
    match method {
        Method::Stored => 0,
        Method::Mszip => MSZIP_DECODER_MEMORY,
        Method::Quantum { window_bits } => quantum::decoder_memory(window_bits),
    }
}

struct Mszip {
    window: Vec<u8>,
    history: Vec<u8>,
}

impl Mszip {
    fn new() -> Self {
        Self {
            window: Vec::new(),
            history: Vec::with_capacity(2 * DEFLATE_HISTORY),
        }
    }

    fn block(&mut self, packed: &[u8], output: &mut [u8], size: usize) -> Result<()> {
        let deflate = packed
            .strip_prefix(MSZIP_SIGNATURE)
            .context("CAB MSZIP block does not start with CK")?;
        let decoded = crate::framing::inflate_block(
            &mut self.window,
            &self.history,
            deflate,
            &mut output[..size],
        )
        .map_err(|error| anyhow::anyhow!("damaged CAB MSZIP block: {error}"))?;
        ensure!(
            decoded.is_some_and(|decoded| decoded.consumed == deflate.len()
                && decoded.written == size),
            "damaged CAB MSZIP block: its deflate stream does not end exactly at its {size} bytes"
        );
        if size >= DEFLATE_HISTORY {
            self.history.clear();
        }
        self.history
            .extend_from_slice(&output[size.saturating_sub(DEFLATE_HISTORY)..size]);
        let excess = self.history.len().saturating_sub(DEFLATE_HISTORY);
        self.history.drain(..excess);
        Ok(())
    }
}

enum Decoder {
    Stored,
    Mszip(Box<Mszip>),
    Quantum(Box<quantum::Quantum>),
}

struct Decoders {
    mszip: Option<Box<Mszip>>,
    quantum: Option<Box<quantum::Quantum>>,
    block: Vec<u8>,
    output: Vec<u8>,
}

impl Decoders {
    fn take(&mut self, method: Method) -> Result<Decoder> {
        let need = decoder_memory(method);
        ensure!(
            need <= DECODER_MEMORY,
            "CAB folder decoder needs {need} bytes, more than the {DECODER_MEMORY}-byte decoder budget"
        );
        if !matches!(method, Method::Quantum { .. }) {
            self.quantum = None;
        }
        if self.block.is_empty() {
            self.block = vec![0; LARGEST_PACKED];
            self.output = vec![0; BLOCK + 1];
        }
        Ok(match method {
            Method::Stored => Decoder::Stored,
            Method::Mszip => {
                let mut mszip = self.mszip.take().unwrap_or_else(|| Box::new(Mszip::new()));
                mszip.history.clear();
                Decoder::Mszip(mszip)
            }
            Method::Quantum { window_bits } => Decoder::Quantum(match self.quantum.take() {
                Some(mut quantum) => {
                    quantum.reset_to_window(window_bits)?;
                    quantum
                }
                None => Box::new(quantum::Quantum::new(window_bits)?),
            }),
        })
    }

    fn give_back(&mut self, decoder: Decoder) {
        match decoder {
            Decoder::Mszip(mszip) => self.mszip = Some(mszip),
            Decoder::Quantum(quantum) => self.quantum = Some(quantum),
            Decoder::Stored => {}
        }
    }
}

fn ensure_unclaimed(claimed: &BTreeMap<u64, u64>, start: u64, end: u64) -> Result<()> {
    let earlier = claimed.range(..end).next_back();
    ensure!(
        earlier.is_none_or(|(_, &claimed_end)| claimed_end <= start),
        "CAB folders share data blocks: the block at offset {start} belongs to an earlier folder"
    );
    Ok(())
}

struct FolderStream<'a> {
    set: &'a mut Set,
    decoders: &'a mut Decoders,
    segments: Vec<Segment>,
    segment: usize,
    method: Method,
    decoder: Option<Decoder>,
    blocks_left: u16,
    first: u64,
    at: u64,
    start: usize,
    end: usize,
    position: u64,
    short_block_read: bool,
}

impl<'a> FolderStream<'a> {
    fn open(set: &'a mut Set, index: usize, decoders: &'a mut Decoders) -> Result<Self> {
        let segments = set.folders[index].clone();
        let method = segments[0].folder.method;
        let mut stream = FolderStream {
            set,
            decoders,
            segments,
            segment: 0,
            method,
            decoder: None,
            blocks_left: 0,
            first: 0,
            at: 0,
            start: 0,
            end: 0,
            position: 0,
            short_block_read: false,
        };
        stream.enter(0)?;
        Ok(stream)
    }

    fn enter(&mut self, segment: usize) -> Result<()> {
        let Segment { cabinet, folder } = self.segments[segment];
        self.set.switch_to(cabinet)?;
        let at = u64::from(folder.data_offset);
        self.set.source.seek_to(at)?;
        self.segment = segment;
        self.blocks_left = folder.blocks;
        self.first = at;
        self.at = at;
        Ok(())
    }

    fn claim(&mut self) {
        if self.at > self.first {
            let cabinet = self.segments[self.segment].cabinet;
            self.set.cabinets[cabinet]
                .claimed
                .insert(self.first, self.at);
        }
    }

    fn next_block(&mut self) -> Result<bool> {
        if self.set.cabinets.len() == 1 {
            return self.next_block_in_set();
        }
        let cabinet = self.segments[self.segment].cabinet;
        self.next_block_in_set().with_context(|| {
            format!(
                "in a folder of cabinet {}",
                self.set.cabinets[cabinet].label()
            )
        })
    }

    fn next_block_in_set(&mut self) -> Result<bool> {
        let mut held = 0;
        let size = loop {
            if self.blocks_left == 0 {
                if self.segment + 1 == self.segments.len() {
                    return Ok(false);
                }
                self.enter(self.segment + 1)?;
                continue;
            }
            ensure!(
                !self.short_block_read,
                "CAB folder has a block shorter than 32 KiB before its last block"
            );
            if self.decoder.is_none() {
                self.decoder = Some(self.decoders.take(self.method)?);
            }
            let Set {
                cabinets, source, ..
            } = &mut *self.set;
            let cabinet = &cabinets[self.segments[self.segment].cabinet];
            let mut header_bytes = [0u8; DATA + u8::MAX as usize];
            let header_bytes = &mut header_bytes[..DATA + usize::from(cabinet.reserve.data)];
            let mut at = self.at;
            source.exact(header_bytes, &mut at, "data block header")?;
            let header = data_header(header_bytes[..DATA].try_into().unwrap())?;
            let range = joined_packed(held, header.packed)?;
            let packed = &mut self.decoders.block[range.clone()];
            source.exact(packed, &mut at, "data block")?;
            ensure_unclaimed(&cabinet.claimed, self.at, at)?;
            ensure!(
                checksum_matches(header_bytes, packed),
                "CAB data block fails its checksum"
            );
            self.at = at;
            self.blocks_left -= 1;
            held = range.end;
            if header.size != 0 {
                break usize::from(header.size);
            }
            ensure!(
                self.blocks_left == 0 && self.segment + 1 < self.segments.len(),
                "CAB data block continues in another cabinet, but its folder does not continue there"
            );
        };
        self.short_block_read = size < BLOCK;
        let packed = &self.decoders.block[..held];
        let output = &mut self.decoders.output;
        match self.decoder.as_mut().expect("taken above") {
            Decoder::Stored => {
                ensure!(
                    packed.len() == size,
                    "CAB stored block packs {} bytes for {size}",
                    packed.len()
                );
                output[..size].copy_from_slice(packed);
            }
            Decoder::Mszip(mszip) => mszip.block(packed, output, size)?,
            Decoder::Quantum(quantum) => {
                let decoded = quantum
                    .chunk(packed, size)
                    .map_err(|error| anyhow::anyhow!("damaged CAB Quantum block: {error}"))?;
                output[..size].copy_from_slice(decoded);
            }
        }
        self.start = 0;
        self.end = size;
        Ok(true)
    }

    fn fill(&mut self) -> Result<bool> {
        if self.start == self.end && !self.next_block()? {
            return Ok(false);
        }
        Ok(true)
    }

    fn skip_toward(&mut self, offset: u64) -> Result<()> {
        ensure!(
            self.fill()?,
            "truncated CAB folder: a file starts past its data"
        );
        let step = (self.end - self.start).min((offset - self.position) as usize);
        self.start += step;
        self.position += step as u64;
        Ok(())
    }

    // EXTERNAL(7-Zip CabHandler): a folder decodes only until its last file ends, so blocks
    // after it are neither decoded nor checked.
    fn finish(mut self) {
        self.claim();
        if let Some(decoder) = self.decoder.take() {
            self.decoders.give_back(decoder);
        }
    }
}

impl Read for FolderStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let more = self.fill().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                Box::<dyn std::error::Error + Send + Sync>::from(error),
            )
        })?;
        if !more {
            return Ok(0);
        }
        let count = buffer.len().min(self.end - self.start);
        buffer[..count].copy_from_slice(&self.decoders.output[self.start..self.start + count]);
        self.start += count;
        self.position += count as u64;
        Ok(count)
    }
}

fn file_mode(record: &FileRecord) -> u32 {
    if record.attributes & READ_ONLY != 0 {
        READ_ONLY_MODE
    } else {
        FILE_MODE
    }
}

fn parse(tree: &mut tree::Tree, set: &mut Set) -> Result<()> {
    let mut items = std::mem::take(&mut set.items).into_iter().peekable();
    while let Some(item) = items.next_if(Item::directory) {
        tree.check()?;
        ensure!(
            item.record.size == 0,
            "CAB folder entry {:?} carries data",
            item.name
        );
        let time = modified(item.record.date, item.record.time);
        tree.directory(&item.name, DIRECTORY_MODE, time)?;
    }
    let mut decoders = Decoders {
        mszip: None,
        quantum: None,
        block: Vec::new(),
        output: Vec::new(),
    };
    for index in 0..set.folders.len() {
        if items
            .peek()
            .is_none_or(|item| item.folder as usize != index)
        {
            continue;
        }
        let mut stream = FolderStream::open(set, index, &mut decoders)?;
        while let Some(item) = items.next_if(|item| item.folder as usize == index) {
            tree.check()?;
            let record = item.record;
            while stream.position < u64::from(record.offset) {
                stream.skip_toward(u64::from(record.offset))?;
                tree.check()?;
            }
            let time = modified(record.date, record.time);
            let mut data = (&mut stream).take(u64::from(record.size));
            tree.file(
                &item.name,
                u64::from(record.size),
                file_mode(&record),
                time,
                &mut data,
            )?;
        }
        stream.finish();
    }
    Ok(())
}

fn extract(path: &Path, destination: Option<&Path>, options: ReadOptions) -> Result<Vec<Entry>> {
    let mut set = Set::read(path)?;
    sort_items(&mut set.items);
    ensure_files_do_not_overlap(&set.items)?;
    tree::extract("CAB", path, destination, options, |tree| {
        parse(tree, &mut set)
    })
}

/// Verify and optionally extract a CAB file into a new directory. It reads
/// stored, MSZIP and Quantum folders, restores each file's DOS time
/// and its read-only attribute, and creates the folders its paths name. A
/// cabinet that starts a set is read with the later cabinets it names,
/// which must sit beside it; a later cabinet of a set is refused.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    extract(path, destination, options)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn reference_checksum(bytes: &[u8]) -> u32 {
        let mut sum = 0u32;
        let whole = bytes.len() / 4 * 4;
        for word in bytes[..whole].chunks(4) {
            sum ^= u32::from_le_bytes(word.try_into().unwrap());
        }
        let mut tail = 0u32;
        for &byte in &bytes[whole..] {
            tail = (tail << 8) | u32::from(byte);
        }
        sum ^ tail
    }

    #[test]
    fn checksums_match_a_word_at_a_time_reference_on_every_length() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for length in 0..300 {
            for _ in 0..50 {
                let bytes: Vec<u8> = (0..length)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
                assert_eq!(checksum(&bytes), reference_checksum(&bytes), "{length}");
            }
        }
        assert_eq!(checksum(&[1, 2, 3]), 0x0001_0203);
        assert_eq!(checksum(&[1, 2, 3, 4, 5]), 0x0403_0201 ^ 5);
    }

    #[test]
    fn decoders_hold_at_most_one_window_between_folders() {
        let mut decoders = Decoders {
            mszip: None,
            quantum: None,
            block: Vec::new(),
            output: Vec::new(),
        };
        let largest_quantum = Method::Quantum { window_bits: 21 };
        for (first, second) in [
            (largest_quantum, Method::Mszip),
            (Method::Mszip, largest_quantum),
            (largest_quantum, Method::Stored),
        ] {
            let decoder = decoders.take(first).unwrap();
            decoders.give_back(decoder);
            let decoder = decoders.take(second).unwrap();
            let held = [first, second]
                .iter()
                .filter(|method| match method {
                    Method::Quantum { .. } => decoders.quantum.is_some(),
                    _ => false,
                })
                .count();
            assert_eq!(held, 0, "{first:?} then {second:?}");
            let windows =
                u64::from(decoders.mszip.is_some()) * MSZIP_DECODER_MEMORY + decoder_memory(second);
            assert!(windows <= DECODER_MEMORY, "{first:?} then {second:?}");
            decoders.give_back(decoder);
        }
    }

    #[test]
    fn every_dos_date_and_time_has_fields_in_range_or_none() {
        for date in 0..=u16::MAX {
            for time in [0u16, 1, 0x7FFF, 0xBF7D, 0xBF7E, 0xC000, u16::MAX] {
                if let Some([year, month, day, hour, minute, second]) = dos_fields(date, time) {
                    assert!((1980..=2107).contains(&year));
                    assert!((1..=12).contains(&month) && (1..=31).contains(&day));
                    assert!(hour <= 23 && minute <= 59 && second <= 58);
                }
            }
        }
        assert_eq!(dos_fields(0x5A21, 0x6000), Some([2025, 1, 1, 12, 0, 0]));
        assert_eq!(dos_fields(0, 0), None);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_never_panic_and_keep_the_file_records_inside_the_cabinet() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(header) = header(&bytes) {
            assert!(header.cabinet_size >= HEADER as u32);
            assert!(header.files_offset >= HEADER as u32);
            assert!(header.files_offset <= header.cabinet_size);
            assert!(header.flags & !KNOWN_FLAGS == 0);
        }
    }

    #[kani::proof]
    fn folders_never_panic_refuse_lzx_and_give_windows_the_decoders_accept() {
        let bytes: [u8; FOLDER] = kani::any();
        if let Ok(folder) = folder(&bytes) {
            assert!(bytes[6] & METHOD_MASK != LZX);
            match folder.method {
                Method::Quantum { window_bits } => {
                    assert!(window_bits <= quantum::MAX_WINDOW_BITS);
                    assert!(decoder_memory(folder.method) <= DECODER_MEMORY);
                }
                Method::Stored | Method::Mszip => {
                    assert!(decoder_memory(folder.method) <= DECODER_MEMORY)
                }
            }
        }
    }

    #[kani::proof]
    fn file_records_never_panic_and_end_inside_33_bits() {
        let bytes: [u8; FILE] = kani::any();
        let record = file_record(&bytes);
        assert!(record.end() < 1 << 33);
        assert!(record.end() >= u64::from(record.offset));
    }

    #[kani::proof]
    fn data_headers_never_panic_and_fit_the_block_buffers() {
        let bytes: [u8; DATA] = kani::any();
        if let Ok(header) = data_header(&bytes) {
            assert!(usize::from(header.size) <= BLOCK);
            assert!((1..=LARGEST_PACKED).contains(&usize::from(header.packed)));
        }
    }

    fn any_header() -> Header {
        Header {
            cabinet_size: kani::any(),
            files_offset: kani::any(),
            folders: kani::any(),
            files: kani::any(),
            flags: kani::any(),
            set_id: kani::any(),
            number: kani::any(),
        }
    }

    #[kani::proof]
    fn a_cabinet_follows_only_with_its_set_id_and_the_next_number() {
        let (previous, next) = (any_header(), any_header());
        if follows_in_set(&previous, &next).is_ok() {
            assert!(next.set_id == previous.set_id);
            assert!(previous.number < u16::MAX && next.number == previous.number + 1);
            assert!(next.flags & PREVIOUS_CABINET != 0);
        }
    }

    #[kani::proof]
    fn every_record_folder_lies_inside_its_cabinet() {
        let (field, folders): (u16, u16) = kani::any();
        if let Ok((local, continued)) = record_folder(field, folders) {
            assert!(local < folders);
            match continued {
                Continued::No => assert!(local == field && field < CONTINUED_FROM_PREVIOUS),
                Continued::FromPrevious => assert!(field == CONTINUED_FROM_PREVIOUS && local == 0),
                Continued::ToNext => assert!(field == CONTINUED_TO_NEXT && local == folders - 1),
                Continued::BothWays => assert!(field == CONTINUED_BOTH_WAYS && folders == 1),
            }
            assert!(
                continued.continues_from_previous()
                    == (local == 0
                        && field >= CONTINUED_FROM_PREVIOUS
                        && field != CONTINUED_TO_NEXT)
            );
        } else {
            assert!(field >= folders || field >= CONTINUED_FROM_PREVIOUS);
        }
    }

    #[kani::proof]
    fn split_blocks_stay_inside_the_block_buffer() {
        let held: usize = kani::any();
        kani::assume(held <= LARGEST_PACKED);
        let part: u16 = kani::any();
        if let Ok(range) = joined_packed(held, part) {
            assert!(range.start == held && range.len() == usize::from(part));
            assert!(range.end <= LARGEST_PACKED);
        } else {
            assert!(held + usize::from(part) > LARGEST_PACKED);
        }
    }

    #[kani::proof]
    #[kani::unwind(4)]
    fn checksums_never_panic_on_any_short_block() {
        let header: [u8; DATA + 3] = kani::any();
        let reserve: usize = kani::any();
        kani::assume(reserve <= 3);
        let data: [u8; 7] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= data.len());
        let _ = checksum_matches(&header[..DATA + reserve], &data[..length]);
    }

    #[kani::proof]
    fn dos_fields_never_overflow_and_stay_in_range() {
        let date: u16 = kani::any();
        let time: u16 = kani::any();
        if let Some([year, month, day, hour, minute, second]) = dos_fields(date, time) {
            assert!((1980..=2107).contains(&year));
            assert!((1..=12).contains(&month) && (1..=31).contains(&day));
            assert!(hour <= 23 && minute <= 59 && second <= 58);
        }
    }
}

#[cfg(test)]
mod mszip_tests {
    use super::*;
    use crate::framing::oracle::{damaged, sample};

    struct Flate2Mszip {
        inflate: flate2::Decompress,
        history: Vec<u8>,
    }

    impl Flate2Mszip {
        fn block(&mut self, packed: &[u8], output: &mut [u8], size: usize) -> bool {
            let Some(deflate) = packed.strip_prefix(MSZIP_SIGNATURE) else {
                return false;
            };
            self.inflate.reset(false);
            if !self.history.is_empty() {
                self.inflate.set_dictionary(&self.history).unwrap();
            }
            let finish = flate2::FlushDecompress::Finish;
            let Ok(status) = self
                .inflate
                .decompress(deflate, &mut output[..size + 1], finish)
            else {
                return false;
            };
            if status != flate2::Status::StreamEnd
                || self.inflate.total_in() != deflate.len() as u64
                || self.inflate.total_out() != size as u64
            {
                return false;
            }
            if size >= DEFLATE_HISTORY {
                self.history.clear();
            }
            self.history
                .extend_from_slice(&output[size.saturating_sub(DEFLATE_HISTORY)..size]);
            let excess = self.history.len().saturating_sub(DEFLATE_HISTORY);
            self.history.drain(..excess);
            true
        }
    }

    fn mszip_blocks(data: &[u8], level: u32) -> Vec<(Vec<u8>, usize)> {
        let mut blocks = Vec::new();
        for (index, chunk) in data.chunks(BLOCK).enumerate() {
            let mut compress = flate2::Compress::new(flate2::Compression::new(level), false);
            if index > 0 {
                let start = index * BLOCK;
                compress
                    .set_dictionary(&data[start.saturating_sub(DEFLATE_HISTORY)..start])
                    .unwrap();
            }
            let mut packed = MSZIP_SIGNATURE.to_vec();
            packed.reserve(chunk.len() + 1024);
            compress
                .compress_vec(chunk, &mut packed, flate2::FlushCompress::Finish)
                .unwrap();
            blocks.push((packed, chunk.len()));
        }
        blocks
    }

    fn agree(blocks: &[(Vec<u8>, usize)], label: &str) {
        let mut ours = Mszip::new();
        let mut theirs = Flate2Mszip {
            inflate: flate2::Decompress::new(false),
            history: Vec::new(),
        };
        let mut ours_out = vec![0u8; BLOCK + 1];
        let mut theirs_out = vec![0u8; BLOCK + 1];
        for (index, (packed, size)) in blocks.iter().enumerate() {
            let accepted = ours.block(packed, &mut ours_out, *size).is_ok();
            let expected = theirs.block(packed, &mut theirs_out, *size);
            assert_eq!(accepted, expected, "{label}: block {index}");
            if !accepted {
                return;
            }
            assert_eq!(
                ours_out[..*size],
                theirs_out[..*size],
                "{label}: block {index}"
            );
            assert_eq!(ours.history, theirs.history, "{label}: block {index}");
        }
    }

    #[test]
    fn mszip_blocks_decode_as_flate2_decodes_them_whole_cut_and_flipped() {
        for (seed, size) in [(1, 10), (2, BLOCK), (3, 3 * BLOCK + 777)] {
            let data = sample(seed, size);
            for level in [0, 1, 6, 9] {
                let blocks = mszip_blocks(&data, level);
                agree(&blocks, &format!("seed {seed} level {level}"));
                for target in 0..blocks.len() {
                    for (variant, packed) in
                        damaged(&blocks[target].0, seed).into_iter().enumerate()
                    {
                        let mut hostile = blocks.clone();
                        hostile[target].0 = packed;
                        agree(
                            &hostile,
                            &format!("seed {seed} block {target} variant {variant}"),
                        );
                    }
                    let mut wrong_size = blocks.clone();
                    wrong_size[target].1 = wrong_size[target].1.saturating_sub(1);
                    agree(
                        &wrong_size,
                        &format!("seed {seed} block {target} one byte short"),
                    );
                }
            }
        }
    }
}
