//! In-process extraction of Apple disk images: UDIF (`.dmg`) images with
//! zero, raw, ADC, zlib, bzip2, LZFSE and xz chunks, raw HFS+ and HFSX
//! volumes, Apple partition maps, and GPT and MBR disks inside a DMG. See
//! `docs/reference/dmg-reader.md` for what it accepts and restores.
use crate::hfs::{self, Volume};
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{io::Read, path::Path};

const TRAILER: usize = 512;
const KOLY: &[u8; 12] = b"koly\0\0\0\x04\0\0\x02\0";
const CHECKSUM: usize = 136;
const CHECKSUM_BYTES: usize = 128;
const CRC32: u32 = 2;
const MISH_HEADER: usize = 0xCC;
const MISH_BLOCK: usize = 40;
const MISH_MAGIC: &[u8; 4] = b"mish";
const SECTOR_LOG: u32 = 9;
const SECTOR_LIMIT: u64 = 1 << (63 - SECTOR_LOG);
const POSITION_LIMIT: u64 = 1 << 63;
const ZERO: u32 = 0;
const RAW: u32 = 1;
const IGNORE: u32 = 2;
const ADC: u32 = 0x8000_0004;
const ZLIB: u32 = 0x8000_0005;
const BZIP2: u32 = 0x8000_0006;
const LZFSE: u32 = 0x8000_0007;
const XZ: u32 = 0x8000_0008;
const COMMENT: u32 = 0x7FFF_FFFE;
const END: u32 = 0xFFFF_FFFF;
const LARGEST_CHUNK: u64 = 1 << 28;
const MAX_PLIST: u64 = 64 * 1024 * 1024;
const MAX_BLOCKS: usize = 4 * 1024 * 1024;
const MAX_PARTITIONS: usize = 4096;
const MAX_XML_DEPTH: usize = 1000;
const RSRC_HEADER: u32 = 0x100;
const RSRC_LIMIT: u64 = 1 << 24;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_BLOCKS: usize = 64;
const XZ_DICTIONARY: u64 = 256 * 1024 * 1024;
const COPY_PIECE: usize = 1 << 20;
const PROBE: usize = 8192;
const MAX_NESTING: usize = 4;
const XML_HEADER: &[u8] = b"<?xml version";

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

impl Span {
    fn at(bytes: &[u8], at: usize) -> Span {
        Span {
            offset: u64_at(bytes, at),
            length: u64_at(bytes, at + 8),
        }
    }

    fn end_within(&self, limit: u64) -> Option<u64> {
        (self.offset <= limit && self.length <= limit - self.offset)
            .then_some(self.offset + self.length)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Checksum {
    pub(crate) kind: u32,
    pub(crate) bits: u32,
    pub(crate) data: [u8; CHECKSUM_BYTES],
}

impl Checksum {
    fn at(bytes: &[u8], at: usize) -> Checksum {
        Checksum {
            kind: u32_at(bytes, at),
            bits: u32_at(bytes, at + 4),
            data: bytes[at + 8..at + CHECKSUM].try_into().unwrap(),
        }
    }

    fn crc32(&self) -> Option<u32> {
        (self.kind == CRC32 && self.bits == 32).then(|| u32_at(&self.data, 0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Koly {
    pub(crate) data: Span,
    pub(crate) resource: Span,
    pub(crate) data_checksum: Checksum,
    pub(crate) xml: Span,
    pub(crate) blob: Span,
    pub(crate) master_checksum: Checksum,
    pub(crate) sectors: u64,
}

pub(crate) fn koly(bytes: &[u8; TRAILER]) -> Field<Koly> {
    field_check(
        &bytes[..KOLY.len()] == KOLY,
        "DMG trailer has no koly signature",
    )?;
    Ok(Koly {
        data: Span::at(bytes, 0x18),
        resource: Span::at(bytes, 0x28),
        data_checksum: Checksum::at(bytes, 0x50),
        xml: Span::at(bytes, 0xD8),
        blob: Span::at(bytes, 0x128),
        master_checksum: Checksum::at(bytes, 0x160),
        sectors: u64_at(bytes, 0x1EC),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MishHeader {
    pub(crate) start_sector: u64,
    pub(crate) sectors: u64,
    pub(crate) data_offset: u64,
    pub(crate) checksum: Checksum,
    pub(crate) blocks: usize,
}

pub(crate) fn mish_header(bytes: &[u8; MISH_HEADER], length: usize) -> Field<MishHeader> {
    field_check(
        &bytes[..4] == MISH_MAGIC && u32_at(bytes, 4) == 1,
        "DMG block table has no mish signature or version 1",
    )?;
    let blocks = u32_at(bytes, 0xC8) as usize;
    field_check(
        blocks
            .checked_mul(MISH_BLOCK)
            .and_then(|size| size.checked_add(MISH_HEADER))
            == Some(length),
        "DMG block table's count does not match its size",
    )?;
    Ok(MishHeader {
        start_sector: u64_at(bytes, 8),
        sectors: u64_at(bytes, 0x10),
        data_offset: u64_at(bytes, 0x18),
        checksum: Checksum::at(bytes, 0x40),
        blocks,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockRecord {
    pub(crate) kind: u32,
    pub(crate) start: u64,
    pub(crate) length: u64,
    pub(crate) packed: u64,
    pub(crate) packed_length: u64,
}

pub(crate) fn block_record(bytes: &[u8; MISH_BLOCK]) -> Field<BlockRecord> {
    let start = u64_at(bytes, 8);
    let sectors = u64_at(bytes, 0x10);
    field_check(
        start < SECTOR_LIMIT && sectors < SECTOR_LIMIT,
        "DMG block lies past the largest sector 7-Zip reads",
    )?;
    let (start, length) = (start << SECTOR_LOG, sectors << SECTOR_LOG);
    field_check(
        start + length < POSITION_LIMIT,
        "DMG block ends past the largest position 7-Zip reads",
    )?;
    let packed = u64_at(bytes, 0x18);
    let packed_length = u64_at(bytes, 0x20);
    Ok(BlockRecord {
        kind: u32_at(bytes, 0),
        start,
        length,
        packed,
        packed_length,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Zero,
    Ignore,
    Raw,
    Adc,
    Zlib,
    Bzip2,
    Lzfse,
    Xz,
    Unknown(u32),
}

impl Method {
    fn of(kind: u32) -> Method {
        match kind {
            ZERO => Method::Zero,
            IGNORE => Method::Ignore,
            RAW => Method::Raw,
            ADC => Method::Adc,
            ZLIB => Method::Zlib,
            BZIP2 => Method::Bzip2,
            LZFSE => Method::Lzfse,
            XZ => Method::Xz,
            other => Method::Unknown(other),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Block {
    method: Method,
    start: u64,
    length: u64,
    packed: u64,
    packed_length: u64,
}

struct Partition {
    name: String,
    header: MishHeader,
    layout: Result<Layout>,
}

struct Layout {
    blocks: Vec<Block>,
    size: u64,
}

impl Partition {
    fn layout(&self) -> Result<&Layout> {
        self.layout
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))
    }
}

// EXTERNAL(7-Zip DmgHandler Open2): a table without the mish signature, version 1 or a size
// that fits its count stops 7-Zip opening the image; other faults spoil only that partition.
fn partition(table: &[u8], name: String) -> Result<Partition> {
    ensure!(
        table.len() >= MISH_HEADER,
        "DMG block table of {name:?} is too short"
    );
    let header = mish_header(table[..MISH_HEADER].try_into().unwrap(), table.len())?;
    ensure!(
        header.blocks <= MAX_BLOCKS,
        "DMG partition {name:?} has more than {MAX_BLOCKS} blocks"
    );
    let layout = blocks(table, &header, &name);
    Ok(Partition {
        name,
        header,
        layout,
    })
}

// EXTERNAL(7-Zip DmgHandler CFile::Parse): blocks follow one another from byte 0, comment
// records and empty blocks are skipped, and the end record comes last.
fn blocks(table: &[u8], header: &MishHeader, name: &str) -> Result<Layout> {
    let mut blocks = Vec::new();
    let mut size = 0u64;
    let mut ended = false;
    for (index, bytes) in table[MISH_HEADER..].chunks_exact(MISH_BLOCK).enumerate() {
        let record = block_record(bytes.try_into().unwrap())?;
        ensure!(
            record.start == size,
            "DMG partition {name:?} has a block that does not follow the one before"
        );
        match record.kind {
            COMMENT => continue,
            END => {
                ensure!(
                    index + 1 == header.blocks,
                    "DMG partition {name:?} has records after its end record"
                );
                ended = true;
                break;
            }
            _ => {}
        }
        if record.length == 0 {
            continue;
        }
        ensure!(
            record.packed < POSITION_LIMIT && record.packed_length < POSITION_LIMIT - record.packed,
            "DMG partition {name:?} has a block whose data lies past 7-Zip's limit"
        );
        blocks.push(Block {
            method: Method::of(record.kind),
            start: record.start,
            length: record.length,
            packed: record.packed,
            packed_length: record.packed_length,
        });
        size = record.start + record.length;
    }
    ensure!(ended, "DMG partition {name:?} has no end record");
    ensure!(
        size >> SECTOR_LOG == header.sectors,
        "DMG partition {name:?} counts {} sectors, and its blocks hold {}",
        header.sectors,
        size >> SECTOR_LOG
    );
    Ok(Layout { blocks, size })
}

const SPACE: u8 = 65;
const EQUALS: u8 = 64;
const INVALID: u8 = 77;

fn base64_value(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        b'=' => EQUALS,
        b'\t' | b'\n' | b'\r' | b' ' | 0xA0 => SPACE,
        _ => INVALID,
    }
}

// EXTERNAL(7-Zip Base64Handler Base64ToBin): white space anywhere, at most two '=' that end the
// last group, and nothing but white space after them.
fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(text.len() / 4 * 3 + 3);
    let mut value = 1u32;
    let mut rest = text.iter();
    loop {
        let Some(&byte) = rest.next() else {
            return (value == 1).then_some(output);
        };
        match base64_value(byte) {
            SPACE => continue,
            EQUALS => break,
            INVALID => return None,
            digit => {
                value = value << 6 | u32::from(digit);
                if value & 1 << 24 != 0 {
                    output.extend_from_slice(&value.to_be_bytes()[1..]);
                    value = 1;
                }
            }
        }
    }
    if value < 1 << 12 {
        return None;
    }
    if value & 1 << 18 != 0 {
        output.push((value >> 10) as u8);
        value <<= 2;
    } else if rest.next().map(|&byte| base64_value(byte)) != Some(EQUALS) {
        return None;
    }
    output.push((value >> 4) as u8);
    rest.all(|&byte| base64_value(byte) == SPACE)
        .then_some(output)
}

#[derive(Debug)]
struct XmlItem {
    tag: bool,
    name: std::ops::Range<usize>,
    children: Vec<XmlItem>,
}

fn is_xml_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

struct XmlText<'a> {
    bytes: &'a [u8],
}

impl XmlText<'_> {
    fn at(&self, at: usize) -> u8 {
        self.bytes.get(at).copied().unwrap_or(0)
    }

    fn skip_spaces(&self, mut at: usize) -> usize {
        while is_xml_space(self.at(at)) {
            at += 1;
        }
        at
    }

    // EXTERNAL(7-Zip MyXml CXmlItem::ParseItem): text up to the next '<', or a tag of letters,
    // digits and '-' with quoted properties, closed by '/>' or by children and '</name>'.
    fn item(&self, start: usize, levels: usize) -> Option<(XmlItem, usize)> {
        let mut at = self.skip_spaces(start);
        let text = at;
        while self.at(at) != 0 && self.at(at) != b'<' {
            at += 1;
        }
        if self.at(at) == 0 {
            return None;
        }
        if at != text {
            let item = XmlItem {
                tag: false,
                name: text..at,
                children: Vec::new(),
            };
            return Some((item, at));
        }
        at = self.skip_spaces(at + 1);
        let name = at;
        while is_name_byte(self.at(at)) {
            at += 1;
        }
        if at == name || self.at(at) == 0 {
            return None;
        }
        let mut item = XmlItem {
            tag: true,
            name: name..at,
            children: Vec::new(),
        };
        loop {
            let before = at;
            at = self.skip_spaces(at);
            match self.at(at) {
                b'/' => return (self.at(at + 1) == b'>').then_some((item, at + 2)),
                b'>' => {
                    at += 1;
                    if levels == 0 {
                        return None;
                    }
                    loop {
                        at = self.skip_spaces(at);
                        if self.at(at) == b'<' && self.at(at + 1) == b'/' {
                            break;
                        }
                        let (child, next) = self.item(at, levels - 1)?;
                        item.children.push(child);
                        at = next;
                    }
                    at += 2;
                    let name = &self.bytes[item.name.clone()];
                    if self.bytes.get(at..at + name.len()) != Some(name) {
                        return None;
                    }
                    at += name.len();
                    return (self.at(at) == b'>').then_some((item, at + 1));
                }
                _ if before == at => return None,
                _ => {
                    let property = at;
                    while is_name_byte(self.at(at)) {
                        at += 1;
                    }
                    if at == property {
                        return None;
                    }
                    at = self.skip_spaces(at);
                    if self.at(at) != b'=' {
                        return None;
                    }
                    at = self.skip_spaces(at + 1);
                    if self.at(at) != b'"' {
                        return None;
                    }
                    at += 1;
                    while self.at(at) != b'"' {
                        if self.at(at) == 0 {
                            return None;
                        }
                        at += 1;
                    }
                    at += 1;
                }
            }
        }
    }

    fn skip_header(&self, at: usize, start: &[u8], end: &[u8]) -> Option<usize> {
        let at = self.skip_spaces(at);
        if !self.bytes[at.min(self.bytes.len())..].starts_with(start) {
            return Some(at);
        }
        let found = self.bytes[at..]
            .windows(end.len())
            .position(|window| window == end)?;
        Some(at + found + end.len())
    }

    fn document(&self) -> Option<XmlItem> {
        let at = self.skip_header(0, b"<?xml", b"?>")?;
        let at = self.skip_header(at, b"<!DOCTYPE", b">")?;
        let (root, at) = self.item(at, MAX_XML_DEPTH)?;
        (root.tag && self.skip_spaces(at) == self.bytes.len()).then_some(root)
    }

    fn is_tag(&self, item: &XmlItem, name: &str) -> bool {
        item.tag && &self.bytes[item.name.clone()] == name.as_bytes()
    }

    fn text<'i>(&self, item: &'i XmlItem) -> Option<&'i std::ops::Range<usize>> {
        match &item.children[..] {
            [only] if !only.tag => Some(&only.name),
            _ => None,
        }
    }

    // EXTERNAL(7-Zip DmgHandler FindKeyPair): the value is the element right after the first
    // <key> with this text whose own tag matches.
    fn value<'i>(&self, dict: &'i XmlItem, key: &str, tag: &str) -> Option<&'i XmlItem> {
        dict.children.windows(2).find_map(|pair| {
            let named = self.is_tag(&pair[0], "key")
                && self
                    .text(&pair[0])
                    .is_some_and(|text| &self.bytes[text.clone()] == key.as_bytes());
            (named && self.is_tag(&pair[1], tag)).then_some(&pair[1])
        })
    }
}

fn plist_partitions(xml: &[u8]) -> Result<Vec<Partition>> {
    let xml = &xml[..xml.iter().position(|&byte| byte == 0).unwrap_or(xml.len())];
    let text = XmlText { bytes: xml };
    let root = text
        .document()
        .context("DMG property list is not XML that 7-Zip reads")?;
    ensure!(
        text.is_tag(&root, "plist"),
        "DMG property list has no plist element"
    );
    let dict = root
        .children
        .iter()
        .find(|item| text.is_tag(item, "dict"))
        .context("DMG property list has no dict")?;
    let forks = text
        .value(dict, "resource-fork", "dict")
        .context("DMG property list has no resource-fork dict")?;
    let blkx = text
        .value(forks, "blkx", "array")
        .context("DMG property list has no blkx array")?;
    let mut partitions = Vec::new();
    for item in blkx
        .children
        .iter()
        .filter(|item| text.is_tag(item, "dict"))
    {
        ensure!(
            partitions.len() < MAX_PARTITIONS,
            "DMG has more than {MAX_PARTITIONS} partitions"
        );
        let data = text
            .value(item, "Data", "data")
            .and_then(|data| text.text(data))
            .context("DMG blkx entry has no Data")?;
        let table = base64(&xml[data.clone()]).context("DMG blkx Data is not base64")?;
        let string = |key: &str| {
            text.value(item, key, "string")
                .and_then(|value| text.text(value))
                .map(|range| String::from_utf8_lossy(&xml[range.clone()]).into_owned())
        };
        let name = string("Name")
            .filter(|name| !name.is_empty())
            .or_else(|| string("CFName"))
            .unwrap_or_default();
        partitions.push(partition(&table, name)?);
    }
    Ok(partitions)
}

// EXTERNAL(7-Zip DmgHandler Open2): the resource fork form keeps one 'blkx' resource per
// partition, each a 4-byte length and a mish table, with 7-Zip reading 24 bits of each offset.
fn resource_partitions(fork: &[u8]) -> Result<Vec<Partition>> {
    let length = fork.len() as u64;
    let head = u32_at(fork, 0);
    let footer = u32_at(fork, 4);
    let data = u32_at(fork, 8);
    let footer_size = u32_at(fork, 12);
    ensure!(
        head == RSRC_HEADER
            && u64::from(footer) < length
            && u64::from(data) < length
            && footer >= data
            && u64::from(footer) == u64::from(head) + u64::from(data),
        "DMG resource fork has a bad header"
    );
    let footer_end = u64::from(footer) + u64::from(footer_size);
    if footer_end != length {
        let rest = length - u64::from(footer);
        ensure!(
            rest >= u64::from(footer_size)
                && rest - u64::from(footer_size) == 4
                && u32_at(fork, footer_end as usize) == 0,
            "DMG resource fork's map does not end the fork"
        );
    }
    ensure!(footer_size >= 0x1E, "DMG resource map is too short");
    let map = &fork[footer as usize..];
    ensure!(
        fork[..16] == map[..16],
        "DMG resource map does not repeat the fork header"
    );
    ensure!(
        u16_at(map, 0x18) == 0x1C,
        "DMG resource map has a bad type list offset"
    );
    let names = u32::from(u16_at(map, 0x1A));
    ensure!(
        names <= footer_size,
        "DMG resource map's names lie past its end"
    );
    let types = u32::from(u16_at(map, 0x1C)) + 1;
    ensure!(
        types * 8 + 0x1E <= names,
        "DMG resource map's types overlap its names"
    );
    let mut partitions = Vec::new();
    for kind in 0..types as usize {
        let entry = &map[0x1E + kind * 8..];
        if &entry[..4] != b"blkx" {
            continue;
        }
        let count = u32::from(u16_at(entry, 4)) + 1;
        let list = u32::from(u16_at(entry, 6));
        ensure!(
            0x1C + list + 12 * count <= names,
            "DMG resource list lies past the resource names"
        );
        for index in 0..count as usize {
            let reference = &map[0x1C + list as usize + index * 12..];
            let name_at = u16_at(reference, 2);
            let offset = u32_at(reference, 4) & 0xFF_FFFF;
            ensure!(
                offset + 4 < data,
                "DMG resource lies past the resource data"
            );
            let at = (head + offset) as usize;
            let size = u32_at(fork, at);
            ensure!(
                data - (offset + 4) >= size,
                "DMG resource is longer than the resource data"
            );
            let mut name = String::new();
            if name_at != 0xFFFF {
                let names_size = footer_size - names;
                ensure!(
                    u32::from(name_at) < names_size,
                    "DMG resource name lies past the names"
                );
                let start = names as usize + usize::from(name_at);
                let letters = usize::from(map[start]);
                ensure!(
                    (names_size - u32::from(name_at)) as usize > letters,
                    "DMG resource name runs past the names"
                );
                name = map[start + 1..start + 1 + letters]
                    .iter()
                    .take_while(|&&byte| (0x20..0x80).contains(&byte))
                    .map(|&byte| byte as char)
                    .collect();
            }
            ensure!(
                partitions.len() < MAX_PARTITIONS,
                "DMG has more than {MAX_PARTITIONS} partitions"
            );
            let table = &fork[at + 4..at + 4 + size as usize];
            partitions.push(partition(table, name)?);
        }
    }
    Ok(partitions)
}

fn read_exact_at(file: &ArchiveFile, mut buffer: &mut [u8], mut offset: u64) -> Result<()> {
    while !buffer.is_empty() {
        let read = file
            .read_at(buffer, offset)
            .context("cannot read the DMG image")?;
        ensure!(read != 0, "truncated DMG image");
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

fn read_span(file: &ArchiveFile, start: u64, span: Span, what: &str) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; span.length as usize];
    read_exact_at(file, &mut bytes, start + span.offset)
        .with_context(|| format!("cannot read the DMG {what}"))?;
    Ok(bytes)
}

struct Udif {
    file: ArchiveFile,
    start: u64,
    koly: Koly,
    partitions: Vec<Partition>,
}

fn file_crc(file: &ArchiveFile, from: u64, length: u64) -> Result<u32> {
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0u8; COPY_PIECE];
    let mut done = 0u64;
    while done < length {
        let take = (length - done).min(COPY_PIECE as u64) as usize;
        read_exact_at(file, &mut buffer[..take], from + done)?;
        hasher.update(&buffer[..take]);
        done += take as u64;
    }
    Ok(hasher.finalize())
}

// EXTERNAL(7-Zip DmgHandler Open2): when the forks end short of the trailer, an XML header at
// the property list's offset from the file's start means the offsets count from the file.
fn xml_offsets_count_from_the_file(file: &ArchiveFile, xml: Span) -> bool {
    let probe = Span {
        offset: xml.offset,
        length: xml.length.min(XML_HEADER.len() as u64),
    };
    probe.length == XML_HEADER.len() as u64
        && read_span(file, 0, probe, "property list").is_ok_and(|bytes| bytes == XML_HEADER)
}

impl Udif {
    // EXTERNAL(7-Zip DmgHandler Open2): the trailer ends the file or, rarely, starts it.
    fn open(path: &Path) -> Result<Option<Udif>> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        if length < TRAILER as u64 {
            return Ok(None);
        }
        let mut trailer = [0u8; TRAILER];
        read_exact_at(&file, &mut trailer, 0)?;
        let mut at = 0u64;
        if trailer[..KOLY.len()] != *KOLY {
            at = length - TRAILER as u64;
            read_exact_at(&file, &mut trailer, at)?;
            if trailer[..KOLY.len()] != *KOLY {
                return Ok(None);
            }
        }
        let koly = koly(&trailer)?;
        let front = koly.data.offset == TRAILER as u64 && at + (TRAILER as u64) < length;
        let limit = if front { length } else { at };
        let mut top = 0u64;
        for (span, what) in [
            (koly.data, "data fork"),
            (koly.xml, "property list"),
            (koly.resource, "resource fork"),
        ] {
            top = top.max(
                span.end_within(limit)
                    .with_context(|| format!("DMG {what} lies past its trailer"))?,
            );
        }
        let blob_end = koly.blob.end_within(limit);
        top = top.max(blob_end.unwrap_or(0));
        let embedded = !front && top != at && !xml_offsets_count_from_the_file(&file, koly.xml);
        let start = if embedded { at - top } else { 0 };
        let partitions = if koly.xml.length == 0 {
            ensure!(
                (u64::from(RSRC_HEADER)..=RSRC_LIMIT).contains(&koly.resource.length),
                "DMG has neither a property list nor a resource fork 7-Zip reads"
            );
            resource_partitions(&read_span(&file, start, koly.resource, "resource fork")?)?
        } else {
            ensure!(
                koly.xml.length <= MAX_PLIST,
                "DMG property list is larger than {MAX_PLIST} bytes"
            );
            plist_partitions(&read_span(&file, start, koly.xml, "property list")?)?
        };
        let udif = Udif {
            file,
            start,
            koly,
            partitions,
        };
        udif.check_headers()?;
        Ok(Some(udif))
    }

    // EXTERNAL(7-Zip DmgHandler Open2): the master checksum is the CRC-32 of the partition checksums,
    // and 7-Zip computes the data fork CRC-32 only when the trailer sets one.
    fn check_headers(&self) -> Result<()> {
        if let Some(master) = self.koly.master_checksum.crc32() {
            let mut hasher = crc32fast::Hasher::new();
            let mut whole = true;
            for partition in &self.partitions {
                let bits = partition.header.checksum.bits;
                if bits % 8 != 0 || bits / 8 > CHECKSUM_BYTES as u32 {
                    whole = false;
                    break;
                }
                hasher.update(&partition.header.checksum.data[..(bits / 8) as usize]);
            }
            ensure!(
                !whole || hasher.finalize() == master,
                "DMG master checksum does not match its partitions"
            );
        }
        if let Some(expected) = self.koly.data_checksum.crc32() {
            let length = self.file.len()?;
            let data = self.koly.data;
            ensure!(
                data.offset < POSITION_LIMIT
                    && data
                        .offset
                        .checked_add(data.length)
                        .is_some_and(|end| self.start + end <= length),
                "DMG data fork lies past the end of the file"
            );
            ensure!(
                file_crc(&self.file, self.start + data.offset, data.length)? == expected,
                "DMG data fork fails its CRC-32 check"
            );
        }
        Ok(())
    }

    fn main_partition(&self) -> Result<&Partition> {
        let mut candidates = self
            .partitions
            .iter()
            .filter(|partition| holds_a_file_system(&partition.name));
        let (Some(main), None) = (candidates.next(), candidates.next()) else {
            bail!("DMG has no single file system partition; {BACKEND_HINT}");
        };
        Ok(main)
    }
}

const APPLE_NAMES: [(&str, bool, Option<&str>); 13] = [
    ("Apple_HFS", true, Some("hfs")),
    ("Apple_HFSX", true, Some("hfsx")),
    ("Apple_UFS", true, Some("ufs")),
    ("Apple_APFS", true, Some("apfs")),
    ("Apple_ISO", true, Some("iso")),
    ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", false, None),
    ("Apple_Free", false, None),
    ("DDM", false, None),
    ("Apple_partition_map", false, None),
    (" GPT ", false, None),
    ("MBR", false, None),
    ("Driver", false, None),
    ("Patches", false, None),
];

// EXTERNAL(7-Zip DmgHandler Is_Apple_FS_Or_Unknown): the first known name a partition name
// contains decides, and a name with none of them may hold a file system.
fn holds_a_file_system(name: &str) -> bool {
    APPLE_NAMES
        .iter()
        .find(|(known, _, _)| name.contains(known))
        .is_none_or(|&(_, file_system, _)| file_system)
}

struct Cached {
    block: usize,
    bytes: Vec<u8>,
    used: u64,
}

struct PartitionVolume<'u> {
    file: &'u ArchiveFile,
    base: u64,
    partition: &'u Partition,
    layout: &'u Layout,
    cache: Vec<Cached>,
    cached_bytes: usize,
    clock: u64,
    crcs: Vec<Option<u32>>,
    packed: Vec<u8>,
}

fn decode(method: Method, packed: &[u8], output: &mut [u8]) -> Result<()> {
    match method {
        Method::Adc => crate::adc::decode(packed, output)?,
        Method::Lzfse => crate::lzfse::decode(packed, output)?,
        Method::Zlib => {
            let mut inflater = flate2::Decompress::new(true);
            let status = inflater
                .decompress(packed, output, flate2::FlushDecompress::Finish)
                .context("DMG zlib chunk is damaged")?;
            ensure!(
                status == flate2::Status::StreamEnd
                    && inflater.total_out() == output.len() as u64
                    && inflater.total_in() == packed.len() as u64,
                "DMG zlib chunk does not fill its block exactly"
            );
        }
        Method::Bzip2 => {
            let mut decoder = bzip2::Decompress::new(false);
            let status = decoder
                .decompress(packed, output)
                .map_err(|error| anyhow::anyhow!("DMG bzip2 chunk is damaged: {error}"))?;
            ensure!(
                status == bzip2::Status::StreamEnd
                    && decoder.total_out() == output.len() as u64
                    && decoder.total_in() == packed.len() as u64,
                "DMG bzip2 chunk does not fill its block exactly"
            );
        }
        Method::Xz => {
            let mut reader =
                crate::lzma::XzReader::new(packed).with_dictionary_limit(XZ_DICTIONARY);
            reader
                .read_exact(output)
                .context("DMG xz chunk is damaged or short")?;
            let mut rest = [0u8; 1];
            ensure!(
                reader.read(&mut rest).context("DMG xz chunk is damaged")? == 0,
                "DMG xz chunk decodes past its block"
            );
        }
        Method::Zero | Method::Ignore | Method::Raw | Method::Unknown(_) => unreachable!(),
    }
    Ok(())
}

fn zeros_crc(length: u64) -> crc32fast::Hasher {
    let mut result = crc32fast::Hasher::new();
    let mut power = crc32fast::Hasher::new();
    power.update(&[0]);
    let mut left = length;
    while left != 0 {
        if left & 1 == 1 {
            result.combine(&power);
        }
        let doubled = power.clone();
        power.combine(&doubled);
        left >>= 1;
    }
    result
}

impl<'u> PartitionVolume<'u> {
    fn new(udif: &'u Udif, partition: &'u Partition) -> Result<Self> {
        let layout = partition.layout()?;
        let length = udif.file.len()?;
        let base = udif.start + udif.koly.data.offset + partition.header.data_offset;
        for block in &layout.blocks {
            match block.method {
                Method::Zero | Method::Ignore => ensure!(
                    block.packed_length == 0,
                    "DMG partition {:?} has a zero block with data",
                    partition.name
                ),
                Method::Raw => ensure!(
                    block.packed_length == block.length,
                    "DMG partition {:?} has a raw block whose data is not its size",
                    partition.name
                ),
                Method::Unknown(kind) => {
                    bail!(
                        "DMG partition {:?} has blocks of method {kind:#x}, which 7-Zip does not decode",
                        partition.name
                    )
                }
                _ => ensure!(
                    block.length <= LARGEST_CHUNK,
                    "DMG partition {:?} has a compressed block larger than 256 MiB; {BACKEND_HINT}",
                    partition.name
                ),
            }
            ensure!(
                base.checked_add(block.packed)
                    .and_then(|start| start.checked_add(block.packed_length))
                    .is_some_and(|end| end <= length),
                "DMG partition {:?} has a block whose data lies past the end of the file",
                partition.name
            );
        }
        Ok(PartitionVolume {
            file: &udif.file,
            base,
            partition,
            layout,
            cache: Vec::new(),
            cached_bytes: 0,
            clock: 0,
            crcs: vec![None; layout.blocks.len()],
            packed: Vec::new(),
        })
    }

    fn decoded(&mut self, index: usize) -> Result<Vec<u8>> {
        let block = self.layout.blocks[index];
        self.packed.resize(block.packed_length as usize, 0);
        read_exact_at(self.file, &mut self.packed, self.base + block.packed)?;
        let mut bytes = vec![0u8; block.length as usize];
        decode(block.method, &self.packed, &mut bytes).with_context(|| {
            format!(
                "DMG partition {:?} has a damaged block at byte {}",
                self.partition.name, block.start
            )
        })?;
        self.crcs[index] = Some(crc32fast::hash(&bytes));
        Ok(bytes)
    }

    fn cached(&mut self, index: usize) -> Result<usize> {
        self.clock += 1;
        if let Some(slot) = self.cache.iter().position(|cached| cached.block == index) {
            self.cache[slot].used = self.clock;
            return Ok(slot);
        }
        let bytes = self.decoded(index)?;
        while !self.cache.is_empty()
            && (self.cache.len() >= CACHE_BLOCKS || self.cached_bytes + bytes.len() > CACHE_BYTES)
        {
            let oldest = (0..self.cache.len())
                .min_by_key(|&slot| self.cache[slot].used)
                .unwrap();
            self.cached_bytes -= self.cache.swap_remove(oldest).bytes.len();
        }
        self.cached_bytes += bytes.len();
        self.cache.push(Cached {
            block: index,
            bytes,
            used: self.clock,
        });
        Ok(self.cache.len() - 1)
    }

    // EXTERNAL(7-Zip DmgHandler Extract): a partition's CRC-32 covers every block but those
    // marked to be ignored, with zero blocks counted as zeros.
    fn verify(&mut self) -> Result<()> {
        let Some(expected) = self.partition.header.checksum.crc32() else {
            return Ok(());
        };
        let mut total = crc32fast::Hasher::new();
        for index in 0..self.layout.blocks.len() {
            let block = self.layout.blocks[index];
            let part = match block.method {
                Method::Ignore => continue,
                Method::Zero => zeros_crc(block.length),
                Method::Raw => crc32fast::Hasher::new_with_initial_len(
                    file_crc(self.file, self.base + block.packed, block.length)?,
                    block.length,
                ),
                _ => {
                    let crc = match self.crcs[index] {
                        Some(crc) => crc,
                        None => {
                            self.decoded(index)?;
                            self.crcs[index].unwrap()
                        }
                    };
                    crc32fast::Hasher::new_with_initial_len(crc, block.length)
                }
            };
            total.combine(&part);
        }
        ensure!(
            total.finalize() == expected,
            "DMG partition {:?} fails its CRC-32 check",
            self.partition.name
        );
        Ok(())
    }
}

impl Volume for PartitionVolume<'_> {
    fn size(&self) -> u64 {
        self.layout.size
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.layout.size),
            "DMG read past the end of its partition"
        );
        let mut offset = offset;
        let mut buffer = buffer;
        while !buffer.is_empty() {
            let index = self
                .layout
                .blocks
                .partition_point(|block| block.start <= offset)
                - 1;
            let block = self.layout.blocks[index];
            let inside = offset - block.start;
            let take = (block.length - inside).min(buffer.len() as u64) as usize;
            let (now, rest) = buffer.split_at_mut(take);
            match block.method {
                Method::Zero | Method::Ignore => now.fill(0),
                Method::Raw => read_exact_at(self.file, now, self.base + block.packed + inside)?,
                _ => {
                    let slot = self.cached(index)?;
                    let start = inside as usize;
                    now.copy_from_slice(&self.cache[slot].bytes[start..start + take]);
                }
            }
            buffer = rest;
            offset += take as u64;
        }
        Ok(())
    }
}

struct FileVolume<'f> {
    file: &'f ArchiveFile,
    length: u64,
}

impl Volume for FileVolume<'_> {
    fn size(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.length),
            "read past the end of the disk image"
        );
        read_exact_at(self.file, buffer, offset)
    }
}

struct Window<'v> {
    inner: &'v mut dyn Volume,
    start: u64,
    length: u64,
}

impl Volume for Window<'_> {
    fn size(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.length),
            "read past the end of a partition"
        );
        self.inner.read_exact_at(self.start + offset, buffer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MapEntry {
    pub(crate) start: u32,
    pub(crate) blocks: u32,
    pub(crate) flags: u32,
    pub(crate) entries: u32,
    pub(crate) kind: [u8; 32],
}

pub(crate) fn apm_block_log(sector: &[u8; 512]) -> Field<u32> {
    field_check(
        &sector[..2] == b"ER" && sector[12..16] == [0; 4],
        "partition map has no driver descriptor",
    )?;
    let size = u16_at(sector, 2);
    field_check(
        matches!(size, 512 | 1024 | 2048 | 4096),
        "partition map block size is not 512 to 4096 bytes",
    )?;
    Ok(size.trailing_zeros())
}

pub(crate) fn map_entry(sector: &[u8; 512]) -> Field<MapEntry> {
    field_check(
        &sector[..4] == b"PM\0\0",
        "partition map entry has no PM signature",
    )?;
    let start = u32_at(sector, 8);
    let blocks = u32_at(sector, 12);
    field_check(
        start.checked_add(blocks).is_some(),
        "partition map entry ends past block 2^32",
    )?;
    Ok(MapEntry {
        start,
        blocks,
        flags: u32_at(sector, 0x58),
        entries: u32_at(sector, 4),
        kind: sector[0x30..0x50].try_into().unwrap(),
    })
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

const MAX_MAP_ENTRIES: u32 = 256;

// EXTERNAL(7-Zip ApmHandler Open): entries follow the descriptor one block apart, and a map
// whose first entry sits at sector 1 and starts at block 1 is read in 512-byte blocks.
fn apm_partition(volume: &mut dyn Volume) -> Result<Located> {
    let mut sector = [0u8; 512];
    volume.read_exact_at(0, &mut sector)?;
    let mut log = apm_block_log(&sector)?;
    volume.read_exact_at(512, &mut sector)?;
    if log != SECTOR_LOG && map_entry(&sector).is_ok_and(|entry| entry.start == 1) {
        log = SECTOR_LOG;
    }
    let mut main = None;
    let mut index = 0u32;
    loop {
        volume.read_exact_at(u64::from(index + 1) << log, &mut sector)?;
        let entry = map_entry(&sector)?;
        ensure!(
            entry.entries <= MAX_MAP_ENTRIES && entry.entries > index,
            "partition map counts its entries wrongly"
        );
        let kind = c_string(&entry.kind);
        if entry.flags & 3 != 0 && holds_a_file_system(&kind) {
            ensure!(
                main.is_none(),
                "partition map holds two file systems; {BACKEND_HINT}"
            );
            main = Some(entry);
        }
        index += 1;
        if index == entry.entries {
            break;
        }
    }
    let main =
        main.with_context(|| format!("partition map holds no file system; {BACKEND_HINT}"))?;
    Ok(Located::Inside(
        u64::from(main.start) << log,
        u64::from(main.blocks) << log,
    ))
}

const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const MAX_GPT_ENTRIES: u32 = 1 << 16;

// EXTERNAL(7-Zip GptHandler Open2): the header at LBA 1 with its CRC-32, a table whose CRC-32
// matches, and 7-Zip opens the only used entry.
fn gpt_partition(volume: &mut dyn Volume, start: &[u8]) -> Result<Located> {
    let log = [9u32, 12]
        .into_iter()
        .find(|&log| start.get(1 << log..(1 << log) + 8) == Some(GPT_SIGNATURE))
        .context("GPT has no header")?;
    let sector = 1usize << log;
    ensure!(
        volume.size() >= 2 * sector as u64,
        "GPT disk is shorter than its header"
    );
    let mut header = vec![0u8; sector];
    volume.read_exact_at(sector as u64, &mut header)?;
    let size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    ensure!(size <= sector && size >= 0x5C, "GPT header has a bad size");
    let crc = u32::from_le_bytes(header[16..20].try_into().unwrap());
    header[16..20].fill(0);
    ensure!(
        crc32fast::hash(&header[..size]) == crc,
        "GPT header fails its CRC-32 check"
    );
    let le64 = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
    let le32 = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    ensure!(le64(0x18) == 1, "GPT header is not at LBA 1");
    let table = le64(0x48);
    ensure!(
        table >= 2 && table >> (63 - log) == 0,
        "GPT table lies outside the disk"
    );
    let entries = le32(0x50);
    let entry_size = le32(0x54);
    ensure!(entries <= MAX_GPT_ENTRIES, "GPT has too many entries");
    ensure!(
        (128..=4096).contains(&entry_size),
        "GPT entries have a bad size"
    );
    let bytes = entries as usize * entry_size as usize;
    let rounded = bytes.div_ceil(sector) * sector;
    ensure!(
        (table << log)
            .checked_add(rounded as u64)
            .is_some_and(|end| end <= volume.size()),
        "GPT table lies past the end of its disk"
    );
    let mut list = vec![0u8; rounded];
    volume.read_exact_at(table << log, &mut list)?;
    ensure!(
        crc32fast::hash(&list[..bytes]) == le32(0x58),
        "GPT table fails its CRC-32 check"
    );
    let mut used = Vec::new();
    for entry in list[..bytes].chunks_exact(entry_size as usize) {
        if entry[..16] == [0; 16] {
            continue;
        }
        let first = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        ensure!(
            last >= first && last >> (63 - log) == 0,
            "GPT entry ends before it starts"
        );
        used.push((first << log, (last - first + 1) << log));
    }
    let [(offset, length)] = used[..] else {
        bail!(
            "GPT holds {} partitions, and 7-Zip opens only one; {BACKEND_HINT}",
            used.len()
        );
    };
    Ok(Located::Inside(offset, length))
}

// EXTERNAL(7-Zip MbrHandler ReadTables): four primary entries whose status is 0 or 0x80, in
// rising order; 7-Zip opens the only one.
fn mbr_partition(start: &[u8], size: u64) -> Result<Located> {
    let mut parts = Vec::new();
    let mut limit = 1u64;
    let mut last = 0u64;
    for index in 0..4 {
        let entry = &start[0x1BE + index * 16..0x1BE + index * 16 + 16];
        let kind = entry[4];
        if kind == 0 {
            continue;
        }
        ensure!(
            ![5, 0xF, 0xEE].contains(&kind),
            "MBR has an extended or protective partition; {BACKEND_HINT}"
        );
        ensure!(
            entry[0] == 0 || entry[0] == 0x80,
            "MBR entry has a bad status"
        );
        let chs = |at: usize| {
            (entry[at] | entry[at + 1] | entry[at + 2]) == 0 || entry[at + 1] & 0x3F > 0
        };
        ensure!(chs(1) && chs(5), "MBR entry has a bad CHS address");
        let lba = u64::from(u32::from_le_bytes(entry[8..12].try_into().unwrap()));
        let blocks = u64::from(u32::from_le_bytes(entry[12..16].try_into().unwrap()));
        ensure!(
            blocks > 0 && lba + blocks <= u64::from(u32::MAX),
            "MBR entry has a bad size"
        );
        ensure!(lba >= limit, "MBR entries overlap");
        limit = lba + blocks;
        last = last.max(limit);
        parts.push((lba << SECTOR_LOG, blocks << SECTOR_LOG));
    }
    ensure!(
        last << 12 > size,
        "MBR may use 4 KiB sectors, which Zipper leaves to 7-Zip; {BACKEND_HINT}"
    );
    let [(offset, length)] = parts[..] else {
        bail!(
            "MBR holds {} partitions, and 7-Zip opens only one; {BACKEND_HINT}",
            parts.len()
        );
    };
    Ok(Located::Inside(offset, length))
}

enum Located {
    Hfs,
    Apfs,
    Inside(u64, u64),
}

fn locate(volume: &mut dyn Volume) -> Result<Located> {
    let size = volume.size();
    let mut start = vec![0u8; (PROBE as u64).min(size) as usize];
    volume.read_exact_at(0, &mut start)?;
    if hfs::starts_with_volume(&start) {
        return Ok(Located::Hfs);
    }
    if start.len() >= 1024 && apm_block_log(start[..512].try_into().unwrap()).is_ok() {
        return apm_partition(volume);
    }
    if start.len() >= 1024 && start[0x1FE..0x200] == [0x55, 0xAA] {
        if [512usize, 4096]
            .iter()
            .any(|&at| start.get(at..at + 8) == Some(GPT_SIGNATURE))
        {
            return gpt_partition(volume, &start);
        }
        return mbr_partition(&start, size);
    }
    if crate::native_apfs::is_container(&start) {
        return Ok(Located::Apfs);
    }
    bail!("partition holds a file system Zipper does not read; {BACKEND_HINT}")
}

fn extract_volume(
    volume: &mut dyn Volume,
    depth: usize,
    tree: &mut tree::Tree,
    options: ReadOptions,
) -> Result<()> {
    match locate(volume)? {
        Located::Hfs => hfs::read(volume, 0, tree, options),
        Located::Apfs => crate::native_apfs::read(volume, tree, options),
        Located::Inside(start, length) => {
            ensure!(
                depth < MAX_NESTING,
                "partition maps nest deeper than {MAX_NESTING}"
            );
            ensure!(
                start
                    .checked_add(length)
                    .is_some_and(|end| end <= volume.size()),
                "partition lies past the end of its disk"
            );
            let mut window = Window {
                inner: volume,
                start,
                length,
            };
            extract_volume(&mut window, depth + 1, tree, options)
        }
    }
}

fn plan(volume: &mut dyn Volume, depth: usize) -> Result<()> {
    match locate(volume)? {
        Located::Hfs => Ok(()),
        Located::Apfs => crate::native_apfs::plan(volume),
        Located::Inside(start, length) => {
            ensure!(
                depth < MAX_NESTING,
                "partition maps nest deeper than {MAX_NESTING}"
            );
            ensure!(
                start
                    .checked_add(length)
                    .is_some_and(|end| end <= volume.size()),
                "partition lies past the end of its disk"
            );
            plan(
                &mut Window {
                    inner: volume,
                    start,
                    length,
                },
                depth + 1,
            )
        }
    }
}

/// Returns whether the file ends, or rarely starts, with a UDIF `koly`
/// trailer.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    if length < TRAILER as u64 {
        return Ok(false);
    }
    let mut trailer = [0u8; KOLY.len()];
    for at in [0, length - TRAILER as u64] {
        if read_exact_at(&file, &mut trailer, at).is_ok() && trailer == *KOLY {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns whether the file is a raw HFS+ or HFSX volume or a disk with an
/// Apple partition map.
pub fn sniff_disk(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut start = vec![0u8; hfs::HEADER_AREA];
    if read_exact_at(&file, &mut start, 0).is_err() {
        return Ok(false);
    }
    Ok(hfs::starts_with_volume(&start)
        || apm_block_log(start[..512].try_into().unwrap()).is_ok()
            && map_entry(start[512..1024].try_into().unwrap()).is_ok())
}

/// Returns whether the default engine sends this image to 7-Zip: a DMG or
/// disk whose file system is not HFS+, HFSX or APFS, an APFS container with
/// an encrypted, sealed or snapshot volume or a Fusion container, or one laid
/// out in a way Zipper leaves to 7-Zip.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let natively = || -> Result<()> {
        match Udif::open(path)? {
            Some(udif) => {
                let main = udif.main_partition()?;
                let mut volume = PartitionVolume::new(&udif, main)?;
                plan(&mut volume, 0)
            }
            None => {
                let file = crate::volumes::open(path)?;
                let length = file.len()?;
                plan(
                    &mut FileVolume {
                        file: &file,
                        length,
                    },
                    0,
                )
            }
        }
    };
    Ok(natively().is_err_and(|error| format!("{error:#}").contains(BACKEND_HINT)))
}

/// Verify and optionally extract a DMG image, a raw HFS+ or HFSX volume or
/// an Apple partition map disk into a new directory, in the tree `7zz x`
/// writes: an HFS+ volume's own folder at the top, or an APFS container's
/// files as `native_apfs::read_archive` writes them, with folders, links
/// and hard links. A DMG's partition must hold HFS+, HFSX or APFS, and its
/// CRC-32 checks must pass. A DMG holding another file system, and an
/// image holding a file with a resource fork, are refused with a hint to
/// use 7-Zip.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let udif = Udif::open(path)?;
    tree::extract("DMG", path, destination, options, |tree| match &udif {
        Some(udif) => {
            let main = udif.main_partition()?;
            let mut volume = PartitionVolume::new(udif, main)?;
            extract_volume(&mut volume, 0, tree, options)?;
            volume.verify()
        }
        None => {
            let file = crate::volumes::open(path)?;
            let length = file.len()?;
            extract_volume(
                &mut FileVolume {
                    file: &file,
                    length,
                },
                0,
                tree,
                options,
            )
        }
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn zero_runs_have_the_crc_of_their_bytes() {
        for length in [0u64, 1, 2, 3, 511, 512, 4096, 100_000, 1 << 20] {
            assert_eq!(
                zeros_crc(length).finalize(),
                crc32fast::hash(&vec![0u8; length as usize]),
                "{length}"
            );
        }
    }

    fn reference_base64(text: &[u8]) -> Option<Vec<u8>> {
        let digits: Vec<u8> = text
            .iter()
            .copied()
            .filter(|&b| !matches!(b, b'\t' | b'\n' | b'\r' | b' ' | 0xA0))
            .collect();
        let end = digits
            .iter()
            .position(|&b| b == b'=')
            .unwrap_or(digits.len());
        let (body, padding) = digits.split_at(end);
        if body.iter().any(|&b| base64_value(b) >= 64) {
            return None;
        }
        let mut out = Vec::new();
        for group in body.chunks(4) {
            let mut value = 0u32;
            for &b in group {
                value = value << 6 | u32::from(base64_value(b));
            }
            match group.len() {
                4 => out.extend_from_slice(&value.to_be_bytes()[1..]),
                3 => out.extend_from_slice(&(value << 6).to_be_bytes()[1..3]),
                2 => out.push((value >> 4) as u8),
                _ => return None,
            }
        }
        let tail = body.len() % 4;
        let padded = match (tail, padding.len()) {
            (0, 0) => true,
            (3, 1) => true,
            (2, 2) => padding == b"==",
            _ => false,
        };
        padded.then_some(out)
    }

    #[test]
    fn every_short_base64_text_decodes_as_a_plain_reference_does() {
        let alphabet = [b'A', b'Q', b'/', b'=', b' ', b'\n', b'*', b'z'];
        let mut cases = 0;
        for length in 0..=7 {
            let mut digits = vec![0usize; length];
            'all: loop {
                let text: Vec<u8> = digits.iter().map(|&d| alphabet[d]).collect();
                let ours = base64(&text);
                let theirs = reference_base64(&text);
                let spaced_after_padding = text
                    .iter()
                    .rposition(|&b| b == b'=')
                    .is_some_and(|last| text[..last].iter().any(|&b| b == b' ' || b == b'\n'));
                if !spaced_after_padding {
                    assert_eq!(ours, theirs, "{:?}", String::from_utf8_lossy(&text));
                }
                cases += 1;
                for digit in digits.iter_mut() {
                    *digit += 1;
                    if *digit < alphabet.len() {
                        continue 'all;
                    }
                    *digit = 0;
                }
                break;
            }
        }
        assert!(cases > 2_000_000, "{cases}");
    }

    #[test]
    fn xml_reads_as_7zip_reads_it() {
        let parse = |text: &str| {
            XmlText {
                bytes: text.as_bytes(),
            }
            .document()
            .is_some()
        };
        assert!(parse(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE plist>\n<plist a=\"1\"><dict/></plist>\n"
        ));
        assert!(parse("<a> text </a>"));
        assert!(!parse("<a>text</a >"));
        assert!(!parse("<a><!-- no --></a>"));
        assert!(!parse("<a>text</b>"));
        assert!(!parse("<a b='1'></a>"));
        assert!(!parse("<a/> trailing"));
        assert!(!parse("text"));
        let deep = "<a>".repeat(1001) + &"</a>".repeat(1001);
        assert!(!parse(&deep));
        let deep = "<a>".repeat(1000) + "<a/>" + &"</a>".repeat(1000);
        assert!(parse(&deep));
    }

    #[test]
    fn partition_names_choose_their_file_system_as_7zip_does() {
        assert!(holds_a_file_system("disk image (Apple_HFS : 4)"));
        assert!(holds_a_file_system("whole disk (Apple_APFS : 0)"));
        assert!(holds_a_file_system("something else"));
        assert!(!holds_a_file_system(
            "Protective Master Boot Record (MBR : 0)"
        ));
        assert!(!holds_a_file_system("GPT Header (Primary GPT Header : 1)"));
        assert!(!holds_a_file_system(" (Apple_Free : 3)"));
        assert!(holds_a_file_system("(Apple_HFS : 1) MBR"));
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn koly_trailers_never_panic() {
        let bytes: [u8; TRAILER] = kani::any();
        if let Ok(trailer) = koly(&bytes) {
            assert!(bytes[..12] == *KOLY);
            assert!(trailer.data.offset == u64_at(&bytes, 0x18));
        }
    }

    #[kani::proof]
    fn mish_headers_fill_exactly_their_table() {
        let bytes: [u8; MISH_HEADER] = kani::any();
        let length: usize = kani::any();
        if let Ok(header) = mish_header(&bytes, length) {
            assert!(header.blocks * MISH_BLOCK + MISH_HEADER == length);
        }
    }

    #[kani::proof]
    fn block_records_stay_below_7zips_limits() {
        let bytes: [u8; MISH_BLOCK] = kani::any();
        if let Ok(record) = block_record(&bytes) {
            assert!(record.start % 512 == 0 && record.length % 512 == 0);
            assert!(record.start + record.length < POSITION_LIMIT);
        }
    }

    #[kani::proof]
    fn partition_map_fields_never_panic() {
        let sector: [u8; 512] = kani::any();
        if let Ok(log) = apm_block_log(&sector) {
            assert!((9..=12).contains(&log));
        }
        if let Ok(entry) = map_entry(&sector) {
            assert!(entry.start.checked_add(entry.blocks).is_some());
        }
    }
}
