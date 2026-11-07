//! In-process extraction of Microsoft Cabinet files with stored, MSZIP and
//! LZX folders. See `docs/reference/cab-reader.md` for what it accepts and
//! restores.
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT, MAX_NAME};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check, lzx};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufRead, BufReader, Read, Seek},
    path::Path,
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
const FIRST_CONTINUED_FOLDER: u16 = 0xFFFD;
const BLOCK: usize = 32 * 1024;
// SPEC(MS-CAB CFDATA): a compressed block may grow by up to 6,144 bytes.
const LARGEST_PACKED: usize = BLOCK + 6144;
const METHOD_MASK: u8 = 0x0F;
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
    Quantum { level: u8, window_bits: u8 },
    Lzx { window_bits: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Folder {
    data_offset: u32,
    blocks: u16,
    method: Method,
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
    })
}

fn reserve(bytes: &[u8; RESERVE_SIZES]) -> Reserve {
    Reserve {
        header: u16_at(bytes, 0),
        folder: bytes[2],
        data: bytes[3],
    }
}

// EXTERNAL(7-Zip CabItem): the method is the low 4 bits of typeCompress's first byte, and
// the LZX window is its whole second byte.
fn folder(bytes: &[u8; FOLDER]) -> Field<Folder> {
    let minor = bytes[7];
    let method = match bytes[6] & METHOD_MASK {
        0 => Method::Stored,
        1 => Method::Mszip,
        2 => Method::Quantum {
            level: bytes[6] >> 4,
            window_bits: minor,
        },
        3 => {
            field_check(
                (lzx::MIN_WINDOW_BITS..=lzx::MAX_WINDOW_BITS).contains(&minor),
                "CAB LZX window is not 2^15 to 2^21 bytes",
            )?;
            Method::Lzx { window_bits: minor }
        }
        _ => return Err(BadField("CAB folder uses an unknown compression method")),
    };
    Ok(Folder {
        data_offset: u32_at(bytes, 0),
        blocks: u16_at(bytes, 4),
        method,
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

fn data_header(bytes: &[u8; DATA]) -> Field<DataHeader> {
    let header = DataHeader {
        checksum: u32_at(bytes, 0),
        packed: u16_at(bytes, 4),
        size: u16_at(bytes, 6),
    };
    field_check(
        header.size != 0,
        "CAB data block continues in another cabinet",
    )?;
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

/// Returns whether the default engine hands this CAB to 7-Zip: it belongs
/// to a cabinet set or holds a Quantum folder, which this reader does not
/// decode. A CAB this reader cannot parse stays with it, so the error comes
/// from it.
pub fn needs_backend(path: &Path) -> Result<bool> {
    Ok(Tables::read(path, false).is_ok_and(|tables| {
        tables.header.flags & (PREVIOUS_CABINET | NEXT_CABINET) != 0
            || tables
                .folders
                .iter()
                .any(|folder| matches!(folder.method, Method::Quantum { .. }))
    }))
}

struct Item {
    name: String,
    record: FileRecord,
    index: usize,
}

impl Item {
    fn directory(&self) -> bool {
        self.record.attributes & DIRECTORY != 0
    }
}

struct Tables {
    reader: BufReader<ArchiveFile>,
    header: Header,
    reserve: Reserve,
    folders: Vec<Folder>,
    items: Vec<Item>,
    block_bytes: u64,
}

impl Tables {
    fn read(path: &Path, with_files: bool) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut reader = BufReader::with_capacity(BUFFER, file);
        let mut bytes = [0u8; HEADER];
        reader
            .read_exact(&mut bytes)
            .context("truncated CAB header")?;
        let header = header(&bytes)?;
        ensure!(
            u64::from(header.cabinet_size) <= length,
            "truncated CAB file: the header gives {} bytes and the file has {length}",
            header.cabinet_size
        );
        let mut tables = Tables {
            reader,
            header,
            reserve: Reserve::default(),
            folders: Vec::new(),
            items: Vec::new(),
            block_bytes: 0,
        };
        let mut at = HEADER as u64;
        if header.flags & RESERVE_PRESENT != 0 {
            let mut sizes = [0u8; RESERVE_SIZES];
            tables.exact(&mut sizes, &mut at, "reserve sizes")?;
            tables.reserve = reserve(&sizes);
            tables.skip(u64::from(tables.reserve.header), &mut at, "header reserve")?;
        }
        if header.flags & PREVIOUS_CABINET != 0 {
            tables.skip_names(&mut at)?;
        }
        if header.flags & NEXT_CABINET != 0 {
            tables.skip_names(&mut at)?;
        }
        let record = FOLDER as u64 + u64::from(tables.reserve.folder);
        ensure!(
            u64::from(header.folders) * record <= u64::from(header.cabinet_size) - at,
            "truncated CAB folder table"
        );
        tables.folders.reserve_exact(usize::from(header.folders));
        for _ in 0..header.folders {
            let mut bytes = [0u8; FOLDER];
            tables.exact(&mut bytes, &mut at, "folder record")?;
            tables.folders.push(folder(&bytes)?);
            tables.skip(u64::from(tables.reserve.folder), &mut at, "folder reserve")?;
        }
        if with_files {
            tables.read_files()?;
        }
        Ok(tables)
    }

    fn exact(&mut self, buffer: &mut [u8], at: &mut u64, what: &str) -> Result<()> {
        ensure!(
            *at + buffer.len() as u64 <= u64::from(self.header.cabinet_size),
            "truncated CAB {what}"
        );
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated CAB {what}"))?;
        *at += buffer.len() as u64;
        Ok(())
    }

    fn skip(&mut self, count: u64, at: &mut u64, what: &str) -> Result<()> {
        ensure!(
            *at + count <= u64::from(self.header.cabinet_size),
            "truncated CAB {what}"
        );
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

    fn skip_names(&mut self, at: &mut u64) -> Result<()> {
        for _ in 0..2 {
            self.name(at)?;
        }
        Ok(())
    }

    fn name(&mut self, at: &mut u64) -> Result<Vec<u8>> {
        let mut name = Vec::new();
        loop {
            let left = u64::from(self.header.cabinet_size) - *at;
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

    fn read_files(&mut self) -> Result<()> {
        let mut at = u64::from(self.header.files_offset);
        self.seek_to(at)?;
        ensure!(
            u64::from(self.header.files) * FILE as u64 <= u64::from(self.header.cabinet_size) - at,
            "truncated CAB file table"
        );
        self.items.reserve_exact(usize::from(self.header.files));
        let mut name_bytes = 0usize;
        for index in 0..usize::from(self.header.files) {
            let mut bytes = [0u8; FILE];
            self.exact(&mut bytes, &mut at, "file record")?;
            let record = file_record(&bytes);
            let raw = self.name(&mut at)?;
            name_bytes += raw.len();
            ensure!(
                name_bytes <= MAX_NAME_BYTES,
                "CAB names exceed 64 MiB in total"
            );
            ensure!(
                record.folder < FIRST_CONTINUED_FOLDER,
                "CAB file {index} continues across a cabinet set, which Zipper does not read in process; {BACKEND_HINT}"
            );
            ensure!(
                usize::from(record.folder) < self.folders.len(),
                "CAB file {index} names folder {}, and the cabinet has {}",
                record.folder,
                self.folders.len()
            );
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
            self.items.push(Item {
                name: name.replace('\\', "/"),
                record,
                index,
            });
        }
        Ok(())
    }
}

// EXTERNAL(7-Zip CabIn): records sort directories first, then by folder, offset and size, and
// one that repeats the folder, offset, size and slashed name of the one before it is dropped.
fn sort_items(items: &mut Vec<Item>) {
    items.sort_by_key(|item| {
        (
            !item.directory(),
            item.record.folder,
            item.record.offset,
            item.record.size,
            item.index,
        )
    });
    items.dedup_by(|later, earlier| {
        later.record.folder == earlier.record.folder
            && later.record.offset == earlier.record.offset
            && later.record.size == earlier.record.size
            && later.name == earlier.name
    });
}

fn ensure_files_do_not_overlap(items: &[Item]) -> Result<()> {
    let mut previous: Option<&Item> = None;
    for item in items.iter().filter(|item| !item.directory()) {
        if let Some(earlier) =
            previous.filter(|earlier| earlier.record.folder == item.record.folder)
        {
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
        Method::Lzx { window_bits } => lzx::decoder_memory(window_bits),
        Method::Quantum { window_bits, .. } => 1u64 << window_bits.min(63),
    }
}

struct Mszip {
    inflate: flate2::Decompress,
    history: Vec<u8>,
}

impl Mszip {
    fn block(&mut self, packed: &[u8], output: &mut [u8], size: usize) -> Result<()> {
        let deflate = packed
            .strip_prefix(MSZIP_SIGNATURE)
            .context("CAB MSZIP block does not start with CK")?;
        self.inflate.reset(false);
        if !self.history.is_empty() {
            self.inflate
                .set_dictionary(&self.history)
                .context("cannot give the MSZIP decoder its history")?;
        }
        let status = self
            .inflate
            .decompress(
                deflate,
                &mut output[..size + 1],
                flate2::FlushDecompress::Finish,
            )
            .map_err(|error| anyhow::anyhow!("damaged CAB MSZIP block: {error}"))?;
        ensure!(
            status == flate2::Status::StreamEnd
                && self.inflate.total_in() == deflate.len() as u64
                && self.inflate.total_out() == size as u64,
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
    Lzx(Box<lzx::Lzx>),
}

struct Decoders {
    mszip: Option<Box<Mszip>>,
    lzx: Option<(u8, Box<lzx::Lzx>)>,
}

impl Decoders {
    fn take(&mut self, method: Method) -> Result<Decoder> {
        if let Method::Quantum { .. } = method {
            bail!("CAB Quantum folders are not read in process yet; {BACKEND_HINT}");
        }
        let need = decoder_memory(method);
        ensure!(
            need <= DECODER_MEMORY,
            "CAB folder decoder needs {need} bytes, more than the {DECODER_MEMORY}-byte decoder budget"
        );
        Ok(match method {
            Method::Stored => Decoder::Stored,
            Method::Mszip => {
                let mut mszip = self.mszip.take().unwrap_or_else(|| {
                    Box::new(Mszip {
                        inflate: flate2::Decompress::new(false),
                        history: Vec::with_capacity(2 * DEFLATE_HISTORY),
                    })
                });
                mszip.history.clear();
                Decoder::Mszip(mszip)
            }
            Method::Lzx { window_bits } => Decoder::Lzx(match self.lzx.take() {
                Some((bits, mut lzx)) if bits == window_bits => {
                    lzx.reset();
                    lzx
                }
                _ => Box::new(lzx::Lzx::new(window_bits, None)?),
            }),
            Method::Quantum { .. } => unreachable!("refused above"),
        })
    }

    fn give_back(&mut self, decoder: Decoder, method: Method) {
        match (decoder, method) {
            (Decoder::Mszip(mszip), _) => self.mszip = Some(mszip),
            (Decoder::Lzx(lzx), Method::Lzx { window_bits }) => self.lzx = Some((window_bits, lzx)),
            _ => {}
        }
    }
}

struct FolderStream<'a> {
    tables: &'a mut Tables,
    decoder: Decoder,
    blocks_left: u16,
    at: u64,
    block: Vec<u8>,
    output: Vec<u8>,
    start: usize,
    end: usize,
    position: u64,
    short_block_read: bool,
}

impl<'a> FolderStream<'a> {
    fn open(tables: &'a mut Tables, folder: Folder, decoders: &mut Decoders) -> Result<Self> {
        let decoder = decoders.take(folder.method)?;
        let at = u64::from(folder.data_offset);
        tables.seek_to(at)?;
        Ok(FolderStream {
            tables,
            decoder,
            blocks_left: folder.blocks,
            at,
            block: vec![0; DATA + usize::from(u8::MAX) + LARGEST_PACKED],
            output: vec![0; BLOCK + 1],
            start: 0,
            end: 0,
            position: 0,
            short_block_read: false,
        })
    }

    fn next_block(&mut self) -> Result<bool> {
        if self.blocks_left == 0 {
            return Ok(false);
        }
        ensure!(
            !self.short_block_read,
            "CAB folder has a block shorter than 32 KiB before its last block"
        );
        let header_length = DATA + usize::from(self.tables.reserve.data);
        let mut at = self.at;
        let (header_bytes, rest) = self.block.split_at_mut(header_length);
        self.tables
            .exact(header_bytes, &mut at, "data block header")?;
        let header = data_header(header_bytes[..DATA].try_into().unwrap())?;
        let packed = &mut rest[..usize::from(header.packed)];
        self.tables.exact(packed, &mut at, "data block")?;
        self.tables.block_bytes += at - self.at;
        ensure!(
            self.tables.block_bytes <= u64::from(self.tables.header.cabinet_size),
            "CAB folders share data blocks: they read more block bytes than the cabinet holds"
        );
        ensure!(
            checksum_matches(header_bytes, packed),
            "CAB data block fails its checksum"
        );
        self.at = at;
        self.blocks_left -= 1;
        let size = usize::from(header.size);
        self.short_block_read = size < BLOCK;
        match &mut self.decoder {
            Decoder::Stored => {
                ensure!(
                    packed.len() == size,
                    "CAB stored block packs {} bytes for {size}",
                    packed.len()
                );
                self.output[..size].copy_from_slice(packed);
            }
            Decoder::Mszip(mszip) => mszip.block(packed, &mut self.output, size)?,
            Decoder::Lzx(lzx) => {
                let decoded = lzx
                    .chunk(packed, size)
                    .map_err(|error| anyhow::anyhow!("damaged CAB LZX block: {error}"))?;
                self.output[..size].copy_from_slice(decoded);
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

    fn finish(mut self, tree: &tree::Tree, decoders: &mut Decoders, method: Method) -> Result<()> {
        while self.next_block()? {
            tree.check()?;
        }
        decoders.give_back(self.decoder, method);
        Ok(())
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
        buffer[..count].copy_from_slice(&self.output[self.start..self.start + count]);
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

fn parse(tree: &mut tree::Tree, tables: &mut Tables) -> Result<()> {
    let mut items = std::mem::take(&mut tables.items).into_iter().peekable();
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
        lzx: None,
    };
    for index in 0..tables.folders.len() {
        let folder = tables.folders[index];
        let mut stream = FolderStream::open(tables, folder, &mut decoders)?;
        while let Some(item) = items.next_if(|item| usize::from(item.record.folder) == index) {
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
        tree.check()?;
        stream.finish(tree, &mut decoders, folder.method)?;
    }
    Ok(())
}

/// Verify and optionally extract a CAB file into a new directory. It reads
/// stored, MSZIP and LZX folders, restores each file's DOS time and its
/// read-only attribute, and creates the folders its paths name. It refuses
/// cabinet sets and Quantum folders, which the default engine hands to
/// 7-Zip.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut tables = Tables::read(path, true)?;
    if tables.header.flags & (PREVIOUS_CABINET | NEXT_CABINET) != 0 {
        bail!(
            "CAB file is part of a cabinet set, which Zipper does not read in process; {BACKEND_HINT}"
        );
    }
    sort_items(&mut tables.items);
    ensure_files_do_not_overlap(&tables.items)?;
    tree::extract("CAB", path, destination, options, |tree| {
        parse(tree, &mut tables)
    })
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
    fn folders_never_panic_and_give_lzx_windows_the_decoder_accepts() {
        let bytes: [u8; FOLDER] = kani::any();
        if let Ok(folder) = folder(&bytes) {
            match folder.method {
                Method::Lzx { window_bits } => {
                    assert!((lzx::MIN_WINDOW_BITS..=lzx::MAX_WINDOW_BITS).contains(&window_bits));
                    assert!(decoder_memory(folder.method) <= DECODER_MEMORY);
                }
                Method::Quantum { level, .. } => assert!(level < 16),
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
            assert!((1..=BLOCK).contains(&usize::from(header.size)));
            assert!((1..=LARGEST_PACKED).contains(&usize::from(header.packed)));
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
