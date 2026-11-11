//! In-process extraction of xar archives and flat macOS installer packages,
//! published only after the table of contents and every file's checksums
//! check out. See `docs/reference/xar-reader.md` for what it accepts and
//! restores.
pub use crate::tree::ReadOptions;
use crate::tree::{self, BACKEND_HINT, Tree};
use crate::{BadField, Entry, Field, field_check, volumes::ArchiveFile};
use anyhow::{Context, Result, bail, ensure};
use quick_xml::events::{BytesStart, Event};
use sha2::Digest;
use std::{
    cell::{Cell, RefCell},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAGIC: [u8; 4] = *b"xar!";
const HEADER: usize = 28;
const VERSION: u16 = 1;
const MAX_TOC: u64 = 256 * 1024 * 1024;
const MAX_TOKEN: usize = 1024 * 1024;
const MAX_DEPTH: usize = 256;
const MAX_FIELD: usize = 8192;
const RECORD_MEMORY: usize = 128 * 1024 * 1024;
const HEAP_BLOCK: usize = 16;
const RECORD_COST: usize = 2 * size_of::<Record>() + size_of::<Option<Box<str>>>() + 32;
const DATA_COST: usize = size_of::<Data>() + HEAP_BLOCK;
const COPY_BUFFER: usize = 1024 * 1024;
const LARGEST_DICTIONARY: u64 = 256 * 1024 * 1024;
const DECODER_OVERHEAD: u64 = 16 * 1024 * 1024;
const LZMA_ALONE_HEADER: usize = 13;
const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0];
const TOC_FILE: &str = "[TOC].xml";
const LONGEST_DIGEST: usize = 64;
const FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    None,
    Sha1,
    Md5,
    Sha256,
    Sha512,
}

impl Algorithm {
    fn length(self) -> usize {
        match self {
            Algorithm::None => 0,
            Algorithm::Md5 => 16,
            Algorithm::Sha1 => 20,
            Algorithm::Sha256 => 32,
            Algorithm::Sha512 => 64,
        }
    }

    fn named(name: &[u8]) -> Option<Algorithm> {
        [
            (&b"none"[..], Algorithm::None),
            (b"sha1", Algorithm::Sha1),
            (b"md5", Algorithm::Md5),
            (b"sha256", Algorithm::Sha256),
            (b"sha512", Algorithm::Sha512),
        ]
        .into_iter()
        .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
        .map(|(_, algorithm)| algorithm)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    size: u16,
    toc_compressed: u64,
    toc_uncompressed: u64,
    algorithm: u32,
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("eight bytes"))
}

// SPEC(xar include/xar.h.in, struct xar_header): magic, header size, version, compressed and
// uncompressed TOC length, checksum algorithm.
fn header(bytes: &[u8; HEADER]) -> Field<Header> {
    field_check(bytes[..4] == MAGIC, "not a xar archive")?;
    let header = Header {
        size: u16::from_be_bytes([bytes[4], bytes[5]]),
        toc_compressed: be_u64(&bytes[8..16]),
        toc_uncompressed: be_u64(&bytes[16..24]),
        algorithm: u32::from_be_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
    };
    field_check(
        usize::from(header.size) >= HEADER,
        "xar header is shorter than 28 bytes",
    )?;
    field_check(
        u16::from_be_bytes([bytes[6], bytes[7]]) == VERSION,
        "xar header version is not 1",
    )?;
    field_check(
        header.toc_compressed > 0,
        "xar has an empty table of contents",
    )?;
    field_check(
        header.toc_uncompressed <= MAX_TOC,
        "xar table of contents is larger than 256 MiB",
    )?;
    Ok(header)
}

// EXTERNAL(Apple xar and mackyle xar): 0 none, 1 SHA-1, 2 MD5. Code 3 is SHA-256 in Apple's xar
// and, in mackyle's, the algorithm named after the 28 bytes. Code 4 is Apple's SHA-512.
fn algorithm(code: u32, extra: &[u8]) -> Field<Algorithm> {
    Ok(match code {
        0 => Algorithm::None,
        1 => Algorithm::Sha1,
        2 => Algorithm::Md5,
        3 => {
            let name = &extra[..extra
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(extra.len())];
            if name.is_empty() {
                Algorithm::Sha256
            } else {
                Algorithm::named(name)
                    .filter(|algorithm| *algorithm != Algorithm::None)
                    .ok_or(BadField("xar header names a checksum Zipper does not know"))?
            }
        }
        4 => Algorithm::Sha512,
        _ => return Err(BadField("xar header has an unknown checksum algorithm")),
    })
}

fn heap_start(header: &Header, file_length: u64) -> Field<u64> {
    let start = u64::from(header.size)
        .checked_add(header.toc_compressed)
        .ok_or(BadField(
            "xar table of contents runs past the end of the file",
        ))?;
    field_check(
        start <= file_length,
        "xar table of contents runs past the end of the file",
    )?;
    Ok(start)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Extent {
    start: u64,
    end: u64,
}

fn extent(heap: u64, offset: u64, length: u64, file_length: u64) -> Field<Extent> {
    let start = heap
        .checked_add(offset)
        .ok_or(BadField("xar data lies outside the heap"))?;
    let end = start
        .checked_add(length)
        .ok_or(BadField("xar data lies outside the heap"))?;
    field_check(end <= file_length, "xar data lies outside the heap")?;
    Ok(Extent { start, end })
}

fn overlap_only_where_equal(extents: &mut [Extent]) -> Field<()> {
    extents.sort_unstable();
    let mut reached = 0u64;
    let mut last: Option<Extent> = None;
    for &extent in extents.iter().filter(|extent| extent.end > extent.start) {
        if last == Some(extent) {
            continue;
        }
        field_check(
            extent.start >= reached,
            "xar data ranges overlap without being the same range",
        )?;
        reached = extent.end;
        last = Some(extent);
    }
    Ok(())
}

/// Returns whether the file starts with the xar signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(MAGIC.len());
    crate::volumes::open(path)?
        .take(MAGIC.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == MAGIC)
}

enum Digester {
    Sha1(sha1::Sha1),
    Md5(md5::Md5),
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Digester {
    fn new(algorithm: Algorithm) -> Option<Digester> {
        Some(match algorithm {
            Algorithm::None => return None,
            Algorithm::Sha1 => Digester::Sha1(sha1::Sha1::new()),
            Algorithm::Md5 => Digester::Md5(md5::Md5::new()),
            Algorithm::Sha256 => Digester::Sha256(sha2::Sha256::new()),
            Algorithm::Sha512 => Digester::Sha512(sha2::Sha512::new()),
        })
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Digester::Sha1(digest) => digest.update(bytes),
            Digester::Md5(digest) => digest.update(bytes),
            Digester::Sha256(digest) => digest.update(bytes),
            Digester::Sha512(digest) => digest.update(bytes),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            Digester::Sha1(digest) => digest.finalize().to_vec(),
            Digester::Md5(digest) => digest.finalize().to_vec(),
            Digester::Sha256(digest) => digest.finalize().to_vec(),
            Digester::Sha512(digest) => digest.finalize().to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Checksum {
    algorithm: Algorithm,
    digest: [u8; LONGEST_DIGEST],
}

impl Checksum {
    fn bytes(&self) -> &[u8] {
        &self.digest[..self.algorithm.length()]
    }
}

fn hex_digest(algorithm: Algorithm, text: &str) -> Result<Checksum> {
    let text = text.trim_ascii();
    ensure!(
        text.len() == 2 * algorithm.length(),
        "xar checksum does not have the length of its {algorithm:?} digest"
    );
    let mut digest = [0u8; LONGEST_DIGEST];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(pair).context("xar checksum is not hexadecimal")?;
        digest[index] = u8::from_str_radix(pair, 16).context("xar checksum is not hexadecimal")?;
    }
    Ok(Checksum { algorithm, digest })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Stored,
    Zlib,
    Bzip2,
    Xz,
    Lzma,
}

impl Encoding {
    fn of(style: &str) -> Result<Encoding> {
        Ok(match style {
            "application/octet-stream" => Encoding::Stored,
            "application/x-gzip" => Encoding::Zlib,
            "application/x-bzip2" => Encoding::Bzip2,
            "application/x-xz" => Encoding::Xz,
            "application/x-lzma" => Encoding::Lzma,
            _ => bail!("xar data encoding {style:?} is not one Zipper reads; {BACKEND_HINT}"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Symlink,
    LinkedOriginal,
    LinkTo(u64),
}

#[derive(Default)]
struct Data {
    offset: Option<u64>,
    length: Option<u64>,
    size: Option<u64>,
    encoding: Option<Encoding>,
    archived: Option<Checksum>,
    extracted: Option<Checksum>,
}

struct Record {
    id: Option<u64>,
    parent: Option<u32>,
    name: Option<Box<str>>,
    kind: Option<Kind>,
    link: Option<Box<str>>,
    mode: Option<u32>,
    modified: Option<SystemTime>,
    data: Option<Box<Data>>,
}

#[derive(Clone, Copy)]
enum HardLinkAttribute {
    Absent,
    Original,
    To(u64),
}

#[derive(Clone, Copy)]
enum Target {
    ChecksumOffset,
    ChecksumSize,
    Name(u32),
    Type(u32, HardLinkAttribute),
    Link(u32),
    Mode(u32),
    Modified(u32),
    Offset(u32),
    Length(u32),
    Size(u32),
    Archived(u32, Algorithm),
    Extracted(u32, Algorithm),
}

#[derive(Clone, Copy)]
enum Frame {
    Root,
    Toc,
    TocChecksum,
    File(u32),
    Data(u32),
    Field(Target),
    Ignored,
}

#[derive(Default)]
struct TocChecksum {
    algorithm: Option<Algorithm>,
    offset: Option<u64>,
    size: Option<u64>,
}

#[derive(Default)]
struct Toc {
    checksum: Option<TocChecksum>,
    records: Vec<Record>,
    memory: usize,
    seen_toc: bool,
}

fn set_once<T>(slot: &mut Option<T>, value: T, what: &str) -> Result<()> {
    ensure!(slot.is_none(), "xar table of contents gives {what} twice");
    *slot = Some(value);
    Ok(())
}

fn set_agreeing<T: PartialEq>(slot: &mut Option<T>, value: T, what: &str) -> Result<()> {
    match slot {
        Some(earlier) => ensure!(
            *earlier == value,
            "xar table of contents gives two different values for {what}"
        ),
        None => *slot = Some(value),
    }
    Ok(())
}

fn decimal(text: &str, what: &str) -> Result<u64> {
    let text = text.trim_ascii();
    ensure!(
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        "xar {what} is not a decimal number"
    );
    text.parse()
        .with_context(|| format!("xar {what} does not fit 64 bits"))
}

fn octal_mode(text: &str) -> Result<u32> {
    let text = text.trim_ascii();
    ensure!(
        !text.is_empty() && text.bytes().all(|byte| (b'0'..=b'7').contains(&byte)),
        "xar mode is not octal"
    );
    u32::from_str_radix(text, 8).context("xar mode does not fit 32 bits")
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn utc_time(text: &str) -> Option<SystemTime> {
    let text = text.trim_ascii();
    let text = text.strip_suffix('Z').unwrap_or(text);
    let bytes = text.as_bytes();
    if bytes.len() != 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = &text[range];
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
}

fn attribute(element: &BytesStart, name: &str) -> Result<Option<String>> {
    element
        .try_get_attribute(name)
        .context("xar table of contents has a malformed attribute")?
        .map(|value| {
            value
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|value| value.into_owned())
        })
        .transpose()
        .context("xar table of contents has a malformed attribute")
}

fn checksum_style(element: &BytesStart) -> Result<Algorithm> {
    let style = attribute(element, "style")?.context("xar checksum has no style")?;
    Algorithm::named(style.as_bytes())
        .with_context(|| format!("xar checksum {style:?} is not one Zipper knows; {BACKEND_HINT}"))
}

fn hard_link_attribute(element: &BytesStart) -> Result<HardLinkAttribute> {
    Ok(match attribute(element, "link")?.as_deref() {
        None => HardLinkAttribute::Absent,
        Some("original") => HardLinkAttribute::Original,
        Some(id) => HardLinkAttribute::To(decimal(id, "hard link id")?),
    })
}

impl Toc {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.memory += bytes;
        ensure!(
            self.memory <= RECORD_MEMORY,
            "xar table of contents takes more than {} MiB to keep",
            RECORD_MEMORY >> 20
        );
        Ok(())
    }

    fn record(&mut self, index: u32) -> &mut Record {
        &mut self.records[index as usize]
    }

    fn data(&mut self, index: u32) -> &mut Data {
        self.records[index as usize]
            .data
            .as_deref_mut()
            .expect("a data element opened its record's data")
    }

    fn open(&mut self, parent: Frame, element: &BytesStart) -> Result<Frame> {
        for attribute in element.attributes() {
            attribute.context("xar table of contents has a malformed or repeated attribute")?;
        }
        let name = element.name();
        let name: &str = name.as_ref();
        Ok(match (parent, name) {
            (Frame::Root, "toc") => {
                ensure!(!self.seen_toc, "xar has two tables of contents");
                self.seen_toc = true;
                Frame::Toc
            }
            (Frame::Toc, "checksum") => {
                let algorithm = checksum_style(element)?;
                set_once(
                    &mut self.checksum,
                    TocChecksum {
                        algorithm: Some(algorithm),
                        ..TocChecksum::default()
                    },
                    "the table of contents checksum",
                )?;
                Frame::TocChecksum
            }
            (Frame::TocChecksum, "offset") => Frame::Field(Target::ChecksumOffset),
            (Frame::TocChecksum, "size") => Frame::Field(Target::ChecksumSize),
            (Frame::Toc | Frame::File(_), "file") => {
                ensure!(
                    self.records.len() < tree::MAX_ENTRIES,
                    "xar has more than {} entries",
                    tree::MAX_ENTRIES
                );
                self.charge(RECORD_COST)?;
                let id = attribute(element, "id")?
                    .map(|id| decimal(&id, "file id"))
                    .transpose()?;
                let parent = match parent {
                    Frame::File(index) => Some(index),
                    _ => None,
                };
                self.records.push(Record {
                    id,
                    parent,
                    name: None,
                    kind: None,
                    link: None,
                    mode: None,
                    modified: None,
                    data: None,
                });
                Frame::File((self.records.len() - 1) as u32)
            }
            (Frame::File(index), "name") => {
                ensure!(
                    attribute(element, "enctype")?.is_none(),
                    "xar name is encoded; {BACKEND_HINT}"
                );
                Frame::Field(Target::Name(index))
            }
            (Frame::File(index), "type") => {
                Frame::Field(Target::Type(index, hard_link_attribute(element)?))
            }
            (Frame::File(index), "link") => Frame::Field(Target::Link(index)),
            (Frame::File(index), "mode") => Frame::Field(Target::Mode(index)),
            (Frame::File(index), "mtime") => Frame::Field(Target::Modified(index)),
            (Frame::File(index), "data") => {
                self.charge(DATA_COST)?;
                let record = self.record(index);
                set_once(&mut record.data, Box::default(), "a file's data")?;
                Frame::Data(index)
            }
            (Frame::Data(index), "offset") => Frame::Field(Target::Offset(index)),
            (Frame::Data(index), "length") => Frame::Field(Target::Length(index)),
            (Frame::Data(index), "size") => Frame::Field(Target::Size(index)),
            (Frame::Data(index), "encoding") => {
                let style = attribute(element, "style")?.context("xar encoding has no style")?;
                let encoding = Encoding::of(&style)?;
                set_agreeing(&mut self.data(index).encoding, encoding, "a data encoding")?;
                Frame::Ignored
            }
            (Frame::Data(index), "archived-checksum") => {
                Frame::Field(Target::Archived(index, checksum_style(element)?))
            }
            (Frame::Data(index), "extracted-checksum") => {
                Frame::Field(Target::Extracted(index, checksum_style(element)?))
            }
            (Frame::Field(_), _) => bail!("xar table of contents has an element inside a field"),
            _ => Frame::Ignored,
        })
    }

    fn close(&mut self, frame: Frame, text: &str) -> Result<()> {
        match frame {
            Frame::Field(target) => self.apply(target, text),
            Frame::File(index) => {
                let record = self.record(index);
                ensure!(record.name.is_some(), "xar file has no name");
                if record.kind.is_none() {
                    record.kind = Some(Kind::File);
                }
                if record.data.is_some() {
                    ensure!(
                        matches!(record.kind, Some(Kind::File | Kind::LinkedOriginal)),
                        "xar entry {:?} carries data but is not a file",
                        record.name.as_deref().unwrap_or_default()
                    );
                }
                if record.kind == Some(Kind::Symlink) {
                    ensure!(
                        record.link.is_some(),
                        "xar symlink {:?} has no target",
                        record.name.as_deref().unwrap_or_default()
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn apply(&mut self, target: Target, text: &str) -> Result<()> {
        match target {
            Target::ChecksumOffset => {
                let offset = decimal(text, "checksum offset")?;
                let checksum = self.checksum.as_mut().expect("an open checksum");
                set_agreeing(&mut checksum.offset, offset, "the checksum offset")
            }
            Target::ChecksumSize => {
                let size = decimal(text, "checksum size")?;
                let checksum = self.checksum.as_mut().expect("an open checksum");
                set_agreeing(&mut checksum.size, size, "the checksum size")
            }
            Target::Name(index) => {
                ensure!(
                    !text.is_empty() && text != "." && text != ".." && !text.contains(['/', '\0']),
                    "xar file name {text:?} is not a plain name"
                );
                self.charge(text.len() + HEAP_BLOCK)?;
                set_agreeing(&mut self.record(index).name, text.into(), "a file name")
            }
            Target::Type(index, link) => {
                let kind = match (text.trim_ascii(), link) {
                    ("file", HardLinkAttribute::Absent) => Kind::File,
                    ("directory", HardLinkAttribute::Absent) => Kind::Directory,
                    ("symlink", HardLinkAttribute::Absent) => Kind::Symlink,
                    ("hardlink", HardLinkAttribute::Original) => Kind::LinkedOriginal,
                    ("hardlink", HardLinkAttribute::To(id)) => Kind::LinkTo(id),
                    ("hardlink", HardLinkAttribute::Absent) => {
                        bail!("xar hard link has no link attribute")
                    }
                    ("fifo" | "character special" | "block special" | "socket", _) => {
                        bail!("xar entry is a device, FIFO or socket, which is not extracted")
                    }
                    (other, _) => bail!("xar entry has the unknown type {other:?}"),
                };
                set_agreeing(&mut self.record(index).kind, kind, "a file type")
            }
            Target::Link(index) => {
                ensure!(
                    text.len() as u64 <= crate::MAX_LINK_TARGET,
                    "xar symlink has a target longer than {} bytes",
                    crate::MAX_LINK_TARGET
                );
                self.charge(text.len() + HEAP_BLOCK)?;
                set_agreeing(&mut self.record(index).link, text.into(), "a link target")
            }
            Target::Mode(index) => {
                let mode = octal_mode(text)?;
                set_agreeing(&mut self.record(index).mode, mode, "a mode")
            }
            Target::Modified(index) => {
                let record = self.record(index);
                if let Some(time) = utc_time(text) {
                    set_agreeing(&mut record.modified, time, "a modification time")?;
                }
                Ok(())
            }
            Target::Offset(index) => {
                let offset = decimal(text, "data offset")?;
                set_agreeing(&mut self.data(index).offset, offset, "a data offset")
            }
            Target::Length(index) => {
                let length = decimal(text, "data length")?;
                set_agreeing(&mut self.data(index).length, length, "a data length")
            }
            Target::Size(index) => {
                let size = decimal(text, "data size")?;
                set_agreeing(&mut self.data(index).size, size, "a data size")
            }
            Target::Archived(index, algorithm) => {
                if algorithm == Algorithm::None {
                    return Ok(());
                }
                let checksum = hex_digest(algorithm, text)?;
                set_agreeing(
                    &mut self.data(index).archived,
                    checksum,
                    "an archived checksum",
                )
            }
            Target::Extracted(index, algorithm) => {
                if algorithm == Algorithm::None {
                    return Ok(());
                }
                let checksum = hex_digest(algorithm, text)?;
                set_agreeing(
                    &mut self.data(index).extracted,
                    checksum,
                    "an extracted checksum",
                )
            }
        }
    }
}

struct TokenLimit<R> {
    inner: R,
    allowance: usize,
}

impl<R: BufRead> Read for TokenLimit<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let read = available.len().min(buffer.len());
        buffer[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for TokenLimit<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.allowance == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "xar table of contents has an XML token longer than 1 MiB",
            ));
        }
        let available = self.inner.fill_buf()?;
        Ok(&available[..available.len().min(self.allowance)])
    }

    fn consume(&mut self, amount: usize) {
        self.allowance -= amount;
        self.inner.consume(amount);
    }
}

struct Inflated<R> {
    inner: R,
    produced: u64,
    declared: u64,
}

impl<R: Read> Read for Inflated<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let room = (self.declared - self.produced).saturating_add(1);
        let want = buffer
            .len()
            .min(usize::try_from(room).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..want])?;
        self.produced += read as u64;
        let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
        if self.produced > self.declared {
            return Err(invalid(
                "xar table of contents inflates past the length its header gives",
            ));
        }
        if read == 0 && want > 0 && self.produced != self.declared {
            return Err(invalid(
                "xar table of contents inflates short of the length its header gives",
            ));
        }
        Ok(read)
    }
}

struct Raw<'a, R> {
    inner: std::io::Take<R>,
    digest: &'a RefCell<Option<Digester>>,
    consumed: &'a Cell<u64>,
}

impl<R: Read> Read for Raw<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if let Some(digest) = self.digest.borrow_mut().as_mut() {
            digest.update(&buffer[..read]);
        }
        self.consumed.set(self.consumed.get() + read as u64);
        Ok(read)
    }
}

struct Archive {
    file: ArchiveFile,
    length: u64,
    header: Header,
    algorithm: Algorithm,
    heap: u64,
}

fn read_exactly(reader: &mut impl Read, buffer: &mut [u8], what: &str) -> Result<()> {
    reader
        .read_exact(buffer)
        .with_context(|| format!("truncated xar {what}"))
}

impl Archive {
    fn open(path: &Path) -> Result<Archive> {
        let mut file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut bytes = [0u8; HEADER];
        read_exactly(&mut file, &mut bytes, "header")?;
        let header = header(&bytes)?;
        let mut extra = vec![0u8; usize::from(header.size) - HEADER];
        read_exactly(&mut file, &mut extra, "header")?;
        let algorithm = algorithm(header.algorithm, &extra)?;
        let heap = heap_start(&header, length)?;
        Ok(Archive {
            file,
            length,
            header,
            algorithm,
            heap,
        })
    }

    fn hash_rest(&mut self, from: u64, length: u64, digest: &mut Digester) -> Result<()> {
        self.file.seek(SeekFrom::Start(from))?;
        let mut rest = (&mut self.file).take(length);
        let mut buffer = vec![0u8; COPY_BUFFER.min(length as usize)];
        loop {
            let read = rest
                .read(&mut buffer)
                .context("cannot read the xar archive")?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        ensure!(rest.limit() == 0, "truncated xar archive");
        Ok(())
    }

    fn inflate_toc<'a>(
        &'a mut self,
        digest: &'a RefCell<Option<Digester>>,
        consumed: &'a Cell<u64>,
    ) -> Result<Inflated<impl Read + 'a>> {
        let start = u64::from(self.header.size);
        self.file.seek(SeekFrom::Start(start))?;
        let raw = Raw {
            inner: (&mut self.file).take(self.header.toc_compressed),
            digest,
            consumed,
        };
        Ok(Inflated {
            inner: crate::framing::Reader::zlib(BufReader::new(raw)),
            produced: 0,
            declared: self.header.toc_uncompressed,
        })
    }

    fn read_toc(&mut self) -> Result<(Toc, Option<Vec<u8>>)> {
        let digest = RefCell::new(Digester::new(self.algorithm));
        let consumed = Cell::new(0);
        let toc = {
            let inflated = self.inflate_toc(&digest, &consumed)?;
            parse_toc(BufReader::new(inflated))?
        };
        let mut digest = digest.into_inner();
        if let Some(digest) = &mut digest {
            let start = u64::from(self.header.size) + consumed.get();
            let rest = self.header.toc_compressed - consumed.get();
            self.hash_rest(start, rest, digest)?;
        }
        Ok((toc, digest.map(Digester::finish)))
    }

    fn check_toc_checksum(&mut self, toc: &Toc, computed: Option<Vec<u8>>) -> Result<Extent> {
        let Some(computed) = computed else {
            ensure!(
                toc.checksum.is_none(),
                "xar table of contents has a checksum its header does not name"
            );
            return Ok(Extent { start: 0, end: 0 });
        };
        let checksum = toc
            .checksum
            .as_ref()
            .context("xar table of contents has no checksum though its header names one")?;
        ensure!(
            checksum.algorithm == Some(self.algorithm),
            "xar table of contents checksum is not the one its header names"
        );
        let (Some(offset), Some(size)) = (checksum.offset, checksum.size) else {
            bail!("xar table of contents checksum has no offset or size");
        };
        ensure!(
            size == computed.len() as u64,
            "xar table of contents checksum has the wrong size"
        );
        let extent = extent(self.heap, offset, size, self.length)?;
        self.file.seek(SeekFrom::Start(extent.start))?;
        let mut stored = vec![0u8; computed.len()];
        read_exactly(&mut self.file, &mut stored, "checksum")?;
        ensure!(
            stored == computed,
            "xar table of contents fails its checksum"
        );
        Ok(extent)
    }
}

fn general_reference(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

fn push_text(text: &mut String, more: &str) -> Result<()> {
    ensure!(
        text.len() + more.len() <= MAX_FIELD,
        "xar table of contents has a field longer than {MAX_FIELD} bytes"
    );
    text.push_str(more);
    Ok(())
}

fn parse_toc(input: impl BufRead) -> Result<Toc> {
    let mut reader = quick_xml::Reader::from_reader(TokenLimit {
        inner: input,
        allowance: MAX_TOKEN,
    });
    let mut toc = Toc::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut text = String::new();
    let mut seen_root = false;
    let mut buffer = Vec::new();
    loop {
        reader.get_mut().allowance = MAX_TOKEN;
        buffer.clear();
        let event = reader
            .read_event_into(&mut buffer)
            .context("xar table of contents is not well-formed XML")?;
        let collecting = matches!(stack.last(), Some(Frame::Field(_)));
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                ensure!(
                    stack.len() < MAX_DEPTH,
                    "xar table of contents nests deeper than {MAX_DEPTH} elements"
                );
                let frame = match stack.last() {
                    None => {
                        ensure!(
                            !seen_root && element.name().as_ref() == "xar",
                            "xar table of contents does not have one <xar> root"
                        );
                        seen_root = true;
                        Frame::Root
                    }
                    Some(&parent) => toc.open(parent, element)?,
                };
                text.clear();
                if matches!(event, Event::Empty(_)) {
                    toc.close(frame, "")?;
                } else {
                    stack.push(frame);
                }
            }
            Event::End(_) => {
                let frame = stack
                    .pop()
                    .context("xar table of contents closes too much")?;
                toc.close(frame, &text)?;
                text.clear();
            }
            Event::Text(content) if collecting => push_text(&mut text, &content.xml10_content())?,
            Event::CData(content) if collecting => push_text(&mut text, &content.xml10_content())?,
            Event::GeneralRef(reference) => {
                let character = match reference.resolve_char_ref() {
                    Ok(Some(character)) => character,
                    Ok(None) => general_reference(&reference)
                        .context("xar table of contents uses an entity XML does not define")?,
                    Err(error) => {
                        return Err(error)
                            .context("xar table of contents has a bad character reference");
                    }
                };
                if collecting {
                    push_text(&mut text, character.encode_utf8(&mut [0; 4]))?;
                }
            }
            Event::DocType(_) => bail!("xar table of contents has a document type declaration"),
            Event::Text(content) => {
                ensure!(
                    !stack.is_empty() || content.trim_ascii().is_empty(),
                    "xar table of contents has text outside its root"
                );
            }
            Event::Eof => break,
            Event::CData(_) | Event::Comment(_) | Event::Decl(_) | Event::PI(_) => {}
        }
    }
    ensure!(
        seen_root && stack.is_empty(),
        "xar table of contents ends before its root closes"
    );
    ensure!(toc.seen_toc, "xar has no <toc> element");
    Ok(toc)
}

struct Checked<R> {
    inner: R,
    digest: Option<Digester>,
}

impl<R: Read> Read for Checked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if let Some(digest) = &mut self.digest {
            digest.update(&buffer[..read]);
        }
        Ok(read)
    }
}

fn decoder<'a>(
    encoding: Encoding,
    mut input: BufReader<impl Read + 'a>,
    size: u64,
) -> Result<Box<dyn Read + 'a>> {
    Ok(match encoding {
        Encoding::Stored => Box::new(input),
        Encoding::Zlib => Box::new(crate::framing::Reader::zlib(input)),
        Encoding::Bzip2 => Box::new(crate::bzip2_streams::ParallelBzip2::new(
            input,
            0,
            crate::bzip2_streams::AfterAStream::TheRestIsLeft,
            1,
        )),
        Encoding::Lzma if !input.fill_buf()?.starts_with(&XZ_MAGIC) => {
            let mut head = [0u8; LZMA_ALONE_HEADER];
            read_exactly(&mut input, &mut head, "LZMA header")?;
            let properties = crate::lzma::Properties::from_byte(head[0])
                .context("xar LZMA data has invalid properties")?;
            let dictionary = u32::from_le_bytes(head[1..5].try_into().expect("four bytes"));
            let declared = u64::from_le_bytes(head[5..13].try_into().expect("eight bytes"));
            ensure!(
                declared == u64::MAX || declared == size,
                "xar LZMA data declares a size other than its file's"
            );
            let memory = crate::lzma::lzma_memory(properties, dictionary, size);
            ensure!(
                memory <= LARGEST_DICTIONARY + DECODER_OVERHEAD,
                "xar LZMA data needs {memory} bytes of decoder memory, more than {} MiB; {BACKEND_HINT}",
                (LARGEST_DICTIONARY + DECODER_OVERHEAD) >> 20
            );
            Box::new(crate::lzma::RawLzmaReader::new(
                input, properties, dictionary, size,
            ))
        }
        Encoding::Xz | Encoding::Lzma => Box::new(crate::dictionary::DecoderMemory(
            crate::lzma::XzReader::new(input).with_dictionary_limit(LARGEST_DICTIONARY),
        )),
    })
}

fn file_data(
    tree: &mut Tree,
    archive: &mut Archive,
    path: &str,
    mode: u32,
    modified: Option<SystemTime>,
    data: Option<&Data>,
) -> Result<()> {
    let Some(data) = data else {
        return tree.file(path, 0, mode, modified, &mut std::io::empty());
    };
    let (Some(offset), Some(length), Some(size)) = (data.offset, data.length, data.size) else {
        bail!("xar file {path:?} has data without an offset, length and size");
    };
    let encoding = data.encoding.unwrap_or(Encoding::Stored);
    ensure!(
        encoding != Encoding::Stored || length == size,
        "xar stored file {path:?} has a length other than its size"
    );
    let extent = extent(archive.heap, offset, length, archive.length)?;
    archive.file.seek(SeekFrom::Start(extent.start))?;
    let archived = RefCell::new(data.archived.and_then(|sum| Digester::new(sum.algorithm)));
    let consumed = Cell::new(0);
    let extracted = {
        let raw = Raw {
            inner: (&mut archive.file).take(length),
            digest: &archived,
            consumed: &consumed,
        };
        let input = BufReader::with_capacity(COPY_BUFFER.min(length.max(1) as usize), raw);
        let mut decoded = Checked {
            inner: decoder(encoding, input, size)
                .with_context(|| format!("cannot decode xar file {path:?}"))?,
            digest: data.extracted.and_then(|sum| Digester::new(sum.algorithm)),
        };
        tree.file(path, size, mode, modified, &mut decoded)
            .with_context(|| format!("cannot decode xar file {path:?}"))?;
        let mut more = [0u8; 1];
        let extra = decoded
            .read(&mut more)
            .with_context(|| format!("cannot decode xar file {path:?}"))?;
        ensure!(extra == 0, "xar file {path:?} decodes past its size");
        decoded.digest.map(Digester::finish)
    };
    if let (Some(expected), Some(computed)) = (data.extracted, extracted) {
        ensure!(
            expected.bytes() == computed.as_slice(),
            "xar file {path:?} fails its extracted checksum"
        );
    }
    let mut archived = archived.into_inner();
    if let Some(digest) = &mut archived {
        archive.hash_rest(
            extent.start + consumed.get(),
            length - consumed.get(),
            digest,
        )?;
    }
    if let (Some(expected), Some(computed)) = (data.archived, archived.map(Digester::finish)) {
        ensure!(
            expected.bytes() == computed.as_slice(),
            "xar file {path:?} fails its archived checksum"
        );
    }
    Ok(())
}

fn data_extents(toc: &Toc, archive: &Archive, checksum: Extent) -> Result<Vec<Extent>> {
    let mut extents = Vec::new();
    for record in &toc.records {
        let Some(data) = record.data.as_deref() else {
            continue;
        };
        if let (Some(offset), Some(length)) = (data.offset, data.length) {
            let extent = extent(archive.heap, offset, length, archive.length)?;
            let apart = extent.end <= checksum.start || checksum.end <= extent.start;
            ensure!(
                apart || extent.start == extent.end,
                "xar data overlaps the table of contents checksum"
            );
            extents.push(extent);
        }
    }
    Ok(extents)
}

fn ids_by_record(toc: &Toc) -> Result<Vec<(u64, u32)>> {
    let mut ids: Vec<(u64, u32)> = toc
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.id.map(|id| (id, index as u32)))
        .collect();
    ids.sort_unstable();
    ensure!(
        ids.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "xar has two files with the same id"
    );
    Ok(ids)
}

fn link_target(ids: &[(u64, u32)], toc: &Toc, id: u64) -> Result<u32> {
    let found = ids
        .binary_search_by_key(&id, |&(id, _)| id)
        .map(|at| ids[at].1)
        .ok()
        .with_context(|| format!("xar hard link names the id {id}, which no file has"))?;
    ensure!(
        matches!(
            toc.records[found as usize].kind,
            Some(Kind::File | Kind::LinkedOriginal)
        ),
        "xar hard link names the id {id}, which is not a file"
    );
    Ok(found)
}

fn extract(tree: &mut Tree, archive: &mut Archive) -> Result<()> {
    let (toc, computed) = archive.read_toc()?;
    let checksum = archive.check_toc_checksum(&toc, computed)?;
    overlap_only_where_equal(&mut data_extents(&toc, archive, checksum)?)?;
    let ids = ids_by_record(&toc)?;
    let mut linked = vec![false; toc.records.len()];
    for record in &toc.records {
        if let Some(Kind::LinkTo(id)) = record.kind {
            linked[link_target(&ids, &toc, id)? as usize] = true;
        }
    }
    let mut paths: Vec<Option<Box<str>>> = Vec::with_capacity(toc.records.len());
    let mut kept = 0usize;
    for (index, record) in toc.records.iter().enumerate() {
        tree.check()?;
        let name = record.name.as_deref().expect("a closed file has a name");
        let path = match record.parent {
            None => name.to_owned(),
            Some(parent) => {
                ensure!(
                    toc.records[parent as usize].kind == Some(Kind::Directory),
                    "xar entry {name:?} is inside a file or link"
                );
                let parent = paths[parent as usize]
                    .as_deref()
                    .expect("a directory keeps its path");
                format!("{parent}/{name}")
            }
        };
        let mode = record.mode.map(|mode| mode & 0o777);
        match record.kind.expect("a closed file has a kind") {
            Kind::Directory => {
                tree.directory(&path, mode.unwrap_or(DIRECTORY_MODE), record.modified)?
            }
            Kind::File | Kind::LinkedOriginal => {
                file_data(
                    tree,
                    archive,
                    &path,
                    mode.unwrap_or(FILE_MODE),
                    record.modified,
                    record.data.as_deref(),
                )?;
                if linked[index] {
                    tree.keep_for_links(&path)?;
                }
            }
            Kind::Symlink => tree.symlink(
                &path,
                record
                    .link
                    .as_deref()
                    .expect("a symlink has a target")
                    .to_owned(),
            )?,
            Kind::LinkTo(_) => {}
        }
        let keep = record.kind == Some(Kind::Directory) || linked[index];
        if keep {
            kept += path.len() + HEAP_BLOCK;
            ensure!(
                toc.memory + kept <= RECORD_MEMORY,
                "xar table of contents takes more than {} MiB to keep",
                RECORD_MEMORY >> 20
            );
        }
        paths.push(keep.then(|| path.into()));
    }
    for record in &toc.records {
        tree.check()?;
        let Some(Kind::LinkTo(id)) = record.kind else {
            continue;
        };
        let target = link_target(&ids, &toc, id)?;
        let name = record.name.as_deref().expect("a closed file has a name");
        let path = match record.parent {
            None => name.to_owned(),
            Some(parent) => format!(
                "{}/{name}",
                paths[parent as usize]
                    .as_deref()
                    .expect("a directory keeps its path")
            ),
        };
        tree.hard_link(
            &path,
            paths[target as usize]
                .as_deref()
                .expect("a link target keeps its path"),
        )?;
    }
    let toc_file_named = toc.records.iter().any(|record| {
        record.parent.is_none()
            && record
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(TOC_FILE))
    });
    if !toc_file_named {
        let declared = archive.header.toc_uncompressed;
        let digest = RefCell::new(None);
        let consumed = Cell::new(0);
        let mut inflated = archive.inflate_toc(&digest, &consumed)?;
        tree.file(TOC_FILE, declared, FILE_MODE, None, &mut inflated)?;
    }
    Ok(())
}

/// Verify and optionally extract a xar archive or flat macOS installer
/// package into a new directory. It checks the table of contents checksum
/// and every file's archived and extracted checksums, and restores files,
/// directories, hard links, Unix modes, modification times and symlinks
/// that stay inside the destination. Like 7-Zip, it also writes the table
/// of contents as `[TOC].xml`. Payloads inside a package are written as
/// they are stored, not unpacked. Signatures are not checked.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut archive = Archive::open(path)?;
    tree::extract("xar", path, destination, options, |tree| {
        extract(tree, &mut archive)
    })
}

#[cfg(test)]
mod bound_tests {
    use super::*;

    fn header_bytes(
        size: u16,
        version: u16,
        compressed: u64,
        uncompressed: u64,
        code: u32,
    ) -> [u8; HEADER] {
        let mut bytes = [0u8; HEADER];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4..6].copy_from_slice(&size.to_be_bytes());
        bytes[6..8].copy_from_slice(&version.to_be_bytes());
        bytes[8..16].copy_from_slice(&compressed.to_be_bytes());
        bytes[16..24].copy_from_slice(&uncompressed.to_be_bytes());
        bytes[24..28].copy_from_slice(&code.to_be_bytes());
        bytes
    }

    #[test]
    fn headers_accept_exactly_the_documented_bounds() {
        assert!(header(&header_bytes(28, 1, 1, MAX_TOC, 1)).is_ok());
        assert!(header(&header_bytes(28, 1, 1, MAX_TOC + 1, 1)).is_err());
        assert!(header(&header_bytes(27, 1, 1, 0, 1)).is_err());
        assert!(header(&header_bytes(28, 2, 1, 0, 1)).is_err());
        assert!(header(&header_bytes(28, 1, 0, 0, 1)).is_err());
        let mut bad = header_bytes(28, 1, 1, 0, 1);
        bad[0] = b'X';
        assert!(header(&bad).is_err());
    }

    #[test]
    fn algorithms_follow_the_header_code_and_name() {
        assert_eq!(algorithm(0, b"").unwrap(), Algorithm::None);
        assert_eq!(algorithm(1, b"").unwrap(), Algorithm::Sha1);
        assert_eq!(algorithm(2, b"").unwrap(), Algorithm::Md5);
        assert_eq!(algorithm(3, b"").unwrap(), Algorithm::Sha256);
        assert_eq!(algorithm(3, b"\0\0\0\0").unwrap(), Algorithm::Sha256);
        assert_eq!(algorithm(3, b"sha512\0\0").unwrap(), Algorithm::Sha512);
        assert_eq!(algorithm(3, b"sha1").unwrap(), Algorithm::Sha1);
        assert!(algorithm(3, b"none\0\0\0\0").is_err());
        assert!(algorithm(3, b"crc32\0\0\0").is_err());
        assert_eq!(algorithm(4, b"").unwrap(), Algorithm::Sha512);
        assert!(algorithm(5, b"").is_err());
    }

    fn reference_extent(heap: u64, offset: u64, length: u64, file: u64) -> Option<(u64, u64)> {
        let start = u128::from(heap) + u128::from(offset);
        let end = start + u128::from(length);
        (end <= u128::from(file)).then_some((start as u64, end as u64))
    }

    #[test]
    fn extents_lie_inside_the_file_for_every_edge_value() {
        let edges = [
            0,
            1,
            2,
            27,
            28,
            29,
            u64::MAX / 2,
            u64::MAX - 2,
            u64::MAX - 1,
            u64::MAX,
        ];
        for heap in edges {
            for offset in edges {
                for length in edges {
                    for file in edges {
                        let got = extent(heap, offset, length, file)
                            .ok()
                            .map(|extent| (extent.start, extent.end));
                        assert_eq!(
                            got,
                            reference_extent(heap, offset, length, file),
                            "{heap} {offset} {length} {file}"
                        );
                    }
                }
            }
        }
        for size in [28u16, 29, u16::MAX] {
            for compressed in edges {
                for file in edges {
                    let header = Header {
                        size,
                        toc_compressed: compressed,
                        toc_uncompressed: 0,
                        algorithm: 0,
                    };
                    let want = u128::from(size) + u128::from(compressed);
                    let got = heap_start(&header, file).ok();
                    let expected = (want <= u128::from(file)).then_some(want as u64);
                    assert_eq!(got, expected, "{size} {compressed} {file}");
                }
            }
        }
    }

    fn reference_overlap(extents: &[Extent]) -> bool {
        extents.iter().enumerate().all(|(index, first)| {
            extents[index + 1..].iter().all(|second| {
                let empty = first.start == first.end || second.start == second.end;
                let apart = first.end <= second.start || second.end <= first.start;
                empty || apart || first == second
            })
        })
    }

    #[test]
    fn ranges_may_overlap_only_when_equal_for_every_small_layout() {
        let mut pieces = Vec::new();
        for start in 0..4u64 {
            for end in start..5u64 {
                pieces.push(Extent { start, end });
            }
        }
        let mut cases = 0;
        let mut layout = Vec::new();
        fn walk(pieces: &[Extent], layout: &mut Vec<Extent>, cases: &mut usize, depth: usize) {
            let mut sorted = layout.clone();
            let got = overlap_only_where_equal(&mut sorted).is_ok();
            assert_eq!(got, reference_overlap(layout), "{layout:?}");
            *cases += 1;
            if depth == 4 {
                return;
            }
            for &piece in pieces {
                layout.push(piece);
                walk(pieces, layout, cases, depth + 1);
                layout.pop();
            }
        }
        walk(&pieces, &mut layout, &mut cases, 0);
        assert!(cases > 40_000, "{cases}");
    }

    #[test]
    fn times_read_as_utc_seconds() {
        let at = |text| {
            utc_time(text).map(|time| match time.duration_since(UNIX_EPOCH) {
                Ok(after) => after.as_secs() as i64,
                Err(before) => -(before.duration().as_secs() as i64),
            })
        };
        assert_eq!(at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(at("2026-11-07T14:12:25Z"), Some(1_794_060_745));
        assert_eq!(at("2000-02-29T00:00:00"), Some(951_782_400));
        assert_eq!(at("1969-12-31T23:59:59Z"), Some(-1));
        assert_eq!(at("2026-13-07T14:12:25Z"), None);
        assert_eq!(at("2026-11-07 14:12:25Z"), None);
        assert_eq!(at("+026-11-07T14:12:25Z"), None);
    }

    #[test]
    fn a_record_costs_what_it_holds() {
        assert!(RECORD_COST >= 2 * size_of::<Record>());
        assert!(DATA_COST >= size_of::<Data>());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_never_panic_and_bound_the_table_of_contents() {
        let bytes: [u8; HEADER] = kani::any();
        let file_length: u64 = kani::any();
        if let Ok(header) = header(&bytes) {
            assert!(usize::from(header.size) >= HEADER);
            assert!(header.toc_compressed > 0);
            assert!(header.toc_uncompressed <= MAX_TOC);
            if let Ok(heap) = heap_start(&header, file_length) {
                assert!(heap <= file_length);
                assert!(heap >= HEADER as u64 + header.toc_compressed);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn algorithm_names_never_panic_and_name_a_digest() {
        let code: u32 = kani::any();
        let extra: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= extra.len());
        if let Ok(algorithm) = algorithm(code, &extra[..length]) {
            assert!(code <= 4);
            assert!(algorithm.length() <= LONGEST_DIGEST);
            assert!((algorithm == Algorithm::None) == (code == 0));
        }
    }

    #[kani::proof]
    fn data_extents_stay_inside_the_file() {
        let heap: u64 = kani::any();
        let offset: u64 = kani::any();
        let length: u64 = kani::any();
        let file_length: u64 = kani::any();
        if let Ok(extent) = extent(heap, offset, length, file_length) {
            assert!(extent.start >= heap);
            assert!(extent.end - extent.start == length);
            assert!(extent.end <= file_length);
        }
    }
}
