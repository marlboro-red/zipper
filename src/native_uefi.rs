//! In-process extraction of UEFI firmware volumes and capsules, as 7-Zip
//! extracts them: the files of a volume, their sections, and the volumes
//! and compressed sections inside them, named as 7-Zip names them. See
//! `docs/reference/uefi-reader.md`.
pub use crate::tree::ReadOptions;
use crate::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{io::Read, path::Path};

pub(crate) const VOLUME_HEADER: usize = 56;
const SIGNATURE_AT: usize = 40;
const SIGNATURE: &[u8; 4] = b"_FVH";
const FILE_SYSTEM_AT: usize = 16;
const FFS1: [u8; 16] = guid(
    0x7A93_54D9,
    0x0468,
    0x444A,
    [0x81, 0xCE, 0x0B, 0xF6, 0x17, 0xD8, 0x90, 0xDF],
);
const FFS2: [u8; 16] = guid(
    0x8C8C_E578,
    0x8A3D,
    0x4F1C,
    [0x99, 0x35, 0x89, 0x61, 0x85, 0xC3, 0x2D, 0xD3],
);
const ERASE_POLARITY: u32 = 0x800;
const BLOCK_MAP_ENTRY: usize = 8;
pub(crate) const FILE_HEADER: usize = 24;
const FILE_ALIGNMENT: usize = 8;
const FILE_STATE_REQUIRED: u8 = 0x04;
const FILE_STATE_REFUSED: u8 = 0x38;
const FILE_DATA_CHECKSUM: u8 = 0x40;
const FILE_RAW: u8 = 0x01;
const FILE_PAD: u8 = 0xF0;
pub(crate) const SECTION_HEADER: usize = 4;
const SECTION_ALIGNMENT: usize = 4;
const EXTENDED_SIZE: u32 = 0xFF_FFFF;
const COMPRESSION: u8 = 0x01;
const GUID_DEFINED: u8 = 0x02;
const DXE_DEPEX: u8 = 0x13;
const VERSION: u8 = 0x14;
const USER_INTERFACE: u8 = 0x15;
const VOLUME_IMAGE: u8 = 0x17;
const FREEFORM_SUBTYPE_GUID: u8 = 0x18;
const PEI_DEPEX: u8 = 0x1B;
const COMPRESSION_HEADER: usize = 5;
const NOT_COMPRESSED: u8 = 0;
pub(crate) const GUID_HEADER: usize = 20;
const CRC32_FIELD: usize = 4;
const LZMA: [u8; 16] = guid(
    0xEE4E_5898,
    0x3914,
    0x4259,
    [0x9D, 0x6E, 0xDC, 0x7B, 0xD7, 0x94, 0x03, 0xCF],
);
const CRC32: [u8; 16] = guid(
    0xFC1B_CDB0,
    0x7D31,
    0x49AA,
    [0x93, 0x6A, 0xA4, 0x60, 0x0D, 0x9D, 0xD0, 0x83],
);
const CAPSULE: [u8; 16] = guid(
    0x5391_82B9,
    0xABB5,
    0x4391,
    [0xB6, 0x9A, 0xE3, 0xA9, 0x43, 0xF7, 0x2F, 0xCC],
);
pub(crate) const CAPSULE_HEADER: usize = 28;
const DEPEX_PUSH_OPERAND: usize = 16;
const DEPEX_LAST_OPCODE: u8 = 0x09;
const DEPEX_OPCODES_WITH_GUID: std::ops::RangeInclusive<u8> = 0..=2;
const NESTING_LIMIT: usize = 64;
const LARGEST_VOLUME: u64 = 512 << 20;
const SCAN_CHUNK: usize = 1 << 20;
const DECODED_BUDGET: u64 = 256 << 20;
const CANDIDATE_LIMIT: usize = 16;
const SNIFF_WINDOW: u64 = 1 << 20;
const FILE_MODE: u32 = 0o644;
const FOLDER_MODE: u32 = 0o755;
const VOLUME_FOLDER: &str = "VOLUME";
const JUNK: &str = "[junk]";
const PROPERTIES: &str = ".prop";
const FOREIGN_VOLUME: &str = "[VOL]";

const fn guid(first: u32, second: u16, third: u16, rest: [u8; 8]) -> [u8; 16] {
    let [a, b, c, d] = first.to_le_bytes();
    let [e, f] = second.to_le_bytes();
    let [g, h] = third.to_le_bytes();
    [
        a, b, c, d, e, f, g, h, rest[0], rest[1], rest[2], rest[3], rest[4], rest[5], rest[6],
        rest[7],
    ]
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le24(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], 0])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VolumeKind {
    Foreign,
    Volume { length: u64, header_length: usize },
}

// SPEC(UEFI PI 1.8 vol. 3, 3.2.1.1 EFI_FIRMWARE_VOLUME_HEADER): a zero vector, the file system
// GUID, the volume length, the `_FVH` signature, attributes, the header length and checksum,
// the extended header offset and the revision. A block map follows up to the header length.
pub(crate) fn volume_header(bytes: &[u8; VOLUME_HEADER]) -> Field<VolumeKind> {
    let file_system = &bytes[FILE_SYSTEM_AT..FILE_SYSTEM_AT + 16];
    if &bytes[SIGNATURE_AT..SIGNATURE_AT + 4] != SIGNATURE
        || (file_system != FFS1 && file_system != FFS2)
    {
        return Ok(VolumeKind::Foreign);
    }
    field_check(
        le32(bytes, 44) & ERASE_POLARITY != 0,
        "a UEFI volume erases to zero bytes, which 7-Zip does not read",
    )?;
    let header_length = usize::from(le16(bytes, 48));
    let length = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
    field_check(
        header_length >= VOLUME_HEADER + BLOCK_MAP_ENTRY && header_length.is_multiple_of(2),
        "a UEFI volume header has no room for its block map",
    )?;
    field_check(
        length >= header_length as u64,
        "a UEFI volume is shorter than its header",
    )?;
    Ok(VolumeKind::Volume {
        length,
        header_length,
    })
}

pub(crate) fn header_holds(header: &[u8]) -> bool {
    let sum = header.chunks_exact(2).fold(0u16, |sum, pair| {
        sum.wrapping_add(u16::from_le_bytes([pair[0], pair[1]]))
    });
    let map_end = header[VOLUME_HEADER..]
        .chunks_exact(BLOCK_MAP_ENTRY)
        .position(|entry| entry == [0; BLOCK_MAP_ENTRY])
        .map(|index| VOLUME_HEADER + (index + 1) * BLOCK_MAP_ENTRY);
    sum == 0 && map_end == Some(header.len())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileHeader {
    pub(crate) kind: u8,
    pub(crate) attributes: u8,
    pub(crate) size: usize,
    pub(crate) data_checksum: u8,
}

// SPEC(UEFI PI 1.8 vol. 3, 2.2.3 EFI_FFS_FILE_HEADER): the name GUID, the header and data
// checksums, the type, attributes, a 24-bit size and the state, whose bits are inverted in a
// volume that erases to ones. The header sums to zero without the data checksum and state.
pub(crate) fn file_header(bytes: &[u8; FILE_HEADER]) -> Field<FileHeader> {
    let sum = bytes
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != 17 && *at != 23)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    field_check(sum == 0, "a UEFI file header checksum does not match")?;
    let state = !bytes[23];
    field_check(
        state & FILE_STATE_REQUIRED != 0 && state & FILE_STATE_REFUSED == 0,
        "a UEFI file is not in a state 7-Zip reads",
    )?;
    let size = le24(bytes, 20) as usize;
    field_check(
        size >= FILE_HEADER,
        "a UEFI file is shorter than its header",
    )?;
    Ok(FileHeader {
        kind: bytes[18],
        attributes: bytes[19],
        size,
        data_checksum: bytes[17],
    })
}

// SPEC(UEFI PI 1.8 vol. 3, 2.2.4 EFI_COMMON_SECTION_HEADER): a 24-bit size that counts the
// header, and the type. 7-Zip 26.05 refuses the extended size form.
pub(crate) fn section_header(bytes: &[u8; SECTION_HEADER]) -> Field<(usize, u8)> {
    let size = le24(bytes, 0);
    field_check(
        size != EXTENDED_SIZE,
        "a UEFI section uses the extended size, which 7-Zip does not read",
    )?;
    field_check(
        size as usize >= SECTION_HEADER,
        "a UEFI section is shorter than its header",
    )?;
    Ok((size as usize, bytes[3]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GuidSection {
    pub(crate) guid: [u8; 16],
    pub(crate) data_offset: usize,
}

// SPEC(UEFI PI 1.8 vol. 3, 2.2.4.6 EFI_GUID_DEFINED_SECTION): the section definition GUID, then
// the offset of the data from the start of the section, and attributes.
pub(crate) fn guid_section(bytes: &[u8; GUID_HEADER], section_size: usize) -> Field<GuidSection> {
    let data_offset = usize::from(le16(bytes, 16));
    field_check(
        data_offset >= SECTION_HEADER + GUID_HEADER && data_offset <= section_size,
        "a UEFI GUID section's data lies outside it",
    )?;
    Ok(GuidSection {
        guid: bytes[..16].try_into().unwrap(),
        data_offset,
    })
}

// SPEC(UEFI PI 1.8 vol. 2, 13.3 Dependency Expression Instruction Set): opcodes 0 to 9, where
// BEFORE, AFTER and PUSH take a GUID. 7-Zip 26.05 reads every byte as an opcode, END included,
// and needs no END.
pub(crate) fn depex_holds(bytes: &[u8]) -> bool {
    let mut at = 0;
    while let Some(&opcode) = bytes.get(at) {
        if opcode > DEPEX_LAST_OPCODE {
            return false;
        }
        at += 1;
        if DEPEX_OPCODES_WITH_GUID.contains(&opcode) {
            if bytes.len() - at < DEPEX_PUSH_OPERAND {
                return false;
            }
            at += DEPEX_PUSH_OPERAND;
        }
    }
    true
}

pub(crate) fn utf16_units(bytes: &[u8]) -> Option<&[u8]> {
    let units = bytes.strip_suffix(&[0, 0])?;
    let whole = units.len().is_multiple_of(2);
    (whole && units.chunks_exact(2).all(|unit| unit != [0, 0])).then_some(units)
}

fn ui_name(bytes: &[u8]) -> Option<String> {
    let units = utf16_units(bytes)?;
    let mut utf8 = Vec::with_capacity(units.len() * 2);
    let mut pending = units
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .peekable();
    while let Some(unit) = pending.next() {
        let low = pending
            .peek()
            .copied()
            .filter(|low| (0xDC00..0xE000).contains(low));
        let point = match (unit, low) {
            (0xD800..0xDC00, Some(low)) => {
                pending.next();
                0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
            }
            (0xD800..0xE000, _) => u32::from(char::REPLACEMENT_CHARACTER),
            _ => u32::from(unit),
        };
        push_utf8(&mut utf8, point);
    }
    Some(utf8.into_iter().map(char::from).collect())
}

fn push_utf8(out: &mut Vec<u8>, point: u32) {
    match point {
        0..0x80 => out.push(point as u8),
        0x80..0x800 => out.extend([0xC0 | (point >> 6) as u8, 0x80 | (point & 0x3F) as u8]),
        0x800..0x10000 => out.extend([
            0xE0 | (point >> 12) as u8,
            0x80 | ((point >> 6) & 0x3F) as u8,
            0x80 | (point & 0x3F) as u8,
        ]),
        _ => out.extend([
            0xF0 | (point >> 18) as u8,
            0x80 | ((point >> 12) & 0x3F) as u8,
            0x80 | ((point >> 6) & 0x3F) as u8,
            0x80 | (point & 0x3F) as u8,
        ]),
    }
}

fn short_name(guid: &[u8]) -> String {
    format!("{:08X}", le32(guid, 0))
}

fn foreign_volume_name(volume: &[u8]) -> String {
    let file_system = &volume[FILE_SYSTEM_AT..FILE_SYSTEM_AT + 16];
    match file_system.iter().all(|&byte| byte == 0xFF) {
        true => FOREIGN_VOLUME.to_owned(),
        false => short_name(file_system) + FOREIGN_VOLUME,
    }
}

fn full_name(guid: &[u8]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        le32(guid, 0),
        le16(guid, 4),
        le16(guid, 6),
        guid[8],
        guid[9],
        guid[10],
        guid[11],
        guid[12],
        guid[13],
        guid[14],
        guid[15]
    )
}

fn section_extension(kind: u8) -> String {
    match kind {
        0x10 => "efi".to_owned(),
        0x11 => "PIC".to_owned(),
        0x12 => "te".to_owned(),
        DXE_DEPEX => "DXE_DEPEX".to_owned(),
        VERSION => "VERSION".to_owned(),
        USER_INTERFACE => "USER_INTERFACE".to_owned(),
        0x16 => "COMPATIBILITY16".to_owned(),
        FREEFORM_SUBTYPE_GUID => "FREEFORM_SUBTYPE_GUID".to_owned(),
        0x19 => "raw".to_owned(),
        PEI_DEPEX => "PEI_DEPEX".to_owned(),
        other => other.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bytes {
    buffer: usize,
    start: usize,
    end: usize,
}

enum Node {
    Leaf {
        label: String,
        named: bool,
        data: Bytes,
    },
    Container {
        file: bool,
        name: String,
        children: Vec<Node>,
    },
    Volume(Vec<Item>),
}

struct Item {
    name: String,
    mergeable: bool,
    content: Content,
}

enum Content {
    Data(Bytes),
    Folder(Vec<Item>),
}

#[derive(Debug)]
enum Failure {
    Format(&'static str),
    Headers(&'static str),
}

impl From<BadField> for Failure {
    fn from(error: BadField) -> Self {
        Failure::Format(error.0)
    }
}

type Parsed<T> = std::result::Result<T, Failure>;

fn format_check(holds: bool, message: &'static str) -> Parsed<()> {
    if holds {
        Ok(())
    } else {
        Err(Failure::Format(message))
    }
}

fn reserve(decoded: &mut u64, size: u64) -> Parsed<usize> {
    *decoded = decoded.saturating_add(size);
    format_check(
        *decoded <= DECODED_BUDGET,
        "UEFI compressed sections decode to more than 256 MiB",
    )?;
    Ok(size as usize)
}

struct Parser {
    buffers: Vec<Vec<u8>>,
    decoded: u64,
}

impl Parser {
    fn bytes(&self, at: Bytes) -> &[u8] {
        &self.buffers[at.buffer][at.start..at.end]
    }

    fn sub(at: Bytes, start: usize, end: usize) -> Bytes {
        Bytes {
            buffer: at.buffer,
            start: at.start + start,
            end: at.start + end,
        }
    }

    fn keep(&mut self, decoded: Vec<u8>) -> Bytes {
        self.buffers.push(decoded);
        let buffer = self.buffers.len() - 1;
        Bytes {
            buffer,
            start: 0,
            end: self.buffers[buffer].len(),
        }
    }

    fn volume(&mut self, at: Bytes, depth: usize) -> Parsed<Vec<Item>> {
        let data = self.bytes(at);
        format_check(
            data.len() >= VOLUME_HEADER,
            "a UEFI volume is cut inside its header",
        )?;
        let VolumeKind::Volume {
            length,
            header_length,
        } = volume_header(data[..VOLUME_HEADER].try_into().unwrap())?
        else {
            return Err(Failure::Format("not a UEFI volume"));
        };
        format_check(
            length <= data.len() as u64,
            "a UEFI volume runs past the data that holds it",
        )?;
        let length = length as usize;
        format_check(
            header_holds(&data[..header_length]),
            "a UEFI volume header checksum or block map does not hold",
        )?;
        let mut items = Vec::new();
        let mut used = std::collections::HashSet::new();
        let mut offset = header_length.next_multiple_of(FILE_ALIGNMENT);
        while offset < length {
            let data = self.bytes(at);
            let rest = &data[offset..length];
            if rest.len() < FILE_HEADER || rest[..FILE_HEADER] == [0xFF; FILE_HEADER] {
                if let Some(junk) = rest.iter().position(|&byte| byte != 0xFF) {
                    items.push(Item {
                        name: JUNK.to_owned(),
                        mergeable: true,
                        content: Content::Data(Self::sub(at, offset + junk, length)),
                    });
                }
                break;
            }
            let header = file_header(rest[..FILE_HEADER].try_into().unwrap())?;
            format_check(
                header.size <= rest.len(),
                "a UEFI file runs past its volume",
            )?;
            let body = &rest[FILE_HEADER..header.size];
            if header.attributes & FILE_DATA_CHECKSUM != 0 {
                let sum = body
                    .iter()
                    .fold(header.data_checksum, |sum, byte| sum.wrapping_add(*byte));
                format_check(sum == 0, "a UEFI file data checksum does not match")?;
            }
            let erased = body.iter().all(|&byte| byte == 0xFF);
            let guid: [u8; 16] = rest[..16].try_into().unwrap();
            let body = Self::sub(at, offset + FILE_HEADER, offset + header.size);
            offset = (offset + header.size).next_multiple_of(FILE_ALIGNMENT);
            if header.kind == FILE_PAD && erased {
                continue;
            }
            let mut name = short_name(&guid);
            if !used.insert(name.clone()) {
                name = full_name(&guid);
            }
            let node = match header.kind {
                FILE_RAW | FILE_PAD => Node::Leaf {
                    label: name,
                    named: true,
                    data: body,
                },
                _ => {
                    let mut ui = None;
                    let children = self.sections(body, depth + 2, &mut ui, true)?;
                    Node::Container {
                        file: true,
                        name: ui.unwrap_or(name),
                        children,
                    }
                }
            };
            items.push(render(node));
        }
        Ok(items)
    }

    fn sections(
        &mut self,
        at: Bytes,
        depth: usize,
        ui: &mut Option<String>,
        own_list: bool,
    ) -> Parsed<Vec<Node>> {
        let structure = |message: &'static str| match own_list {
            true => Failure::Headers(message),
            false => Failure::Format(message),
        };
        format_check(
            depth <= NESTING_LIMIT,
            "UEFI volumes and sections nest too deeply",
        )?;
        let mut nodes = Vec::new();
        let mut offset = 0;
        let length = at.end - at.start;
        while offset < length {
            let data = self.bytes(at);
            format_check(
                length - offset >= SECTION_HEADER,
                "UEFI sections end with bytes too few for a section header",
            )?;
            let (size, kind) =
                section_header(data[offset..offset + SECTION_HEADER].try_into().unwrap())
                    .map_err(|error| structure(error.0))?;
            if size > length - offset {
                return Err(structure("a UEFI section runs past its file"));
            }
            let section = Self::sub(at, offset, offset + size);
            let body = Self::sub(at, offset + SECTION_HEADER, offset + size);
            offset += size;
            if offset < length {
                offset = offset.next_multiple_of(SECTION_ALIGNMENT);
                format_check(
                    offset <= length,
                    "a UEFI section's padding runs past its file",
                )?;
            }
            self.section(kind, section, body, depth, ui, &mut nodes)?;
        }
        Ok(nodes)
    }

    fn section(
        &mut self,
        kind: u8,
        section: Bytes,
        body: Bytes,
        depth: usize,
        ui: &mut Option<String>,
        nodes: &mut Vec<Node>,
    ) -> Parsed<()> {
        let content = self.bytes(body);
        let leaf = |label: String, data: Bytes| Node::Leaf {
            label,
            named: false,
            data,
        };
        match kind {
            COMPRESSION => {
                format_check(
                    content.len() >= COMPRESSION_HEADER,
                    "a UEFI compression section is cut",
                )?;
                let inner = Self::sub(body, COMPRESSION_HEADER, content.len());
                format_check(
                    content[4] == NOT_COMPRESSED,
                    "a UEFI compression section is compressed, which 7-Zip does not decode",
                )?;
                nodes.extend(self.sections(inner, depth + 1, ui, false)?);
            }
            GUID_DEFINED => {
                format_check(content.len() >= GUID_HEADER, "a UEFI GUID section is cut")?;
                let header = guid_section(
                    content[..GUID_HEADER].try_into().unwrap(),
                    section.end - section.start,
                )?;
                let payload = Self::sub(section, header.data_offset, section.end - section.start);
                if header.guid == CRC32 {
                    format_check(
                        content.len() >= GUID_HEADER + CRC32_FIELD,
                        "a UEFI CRC32 section is cut",
                    )?;
                    let stored = le32(content, GUID_HEADER);
                    format_check(
                        crc32fast::hash(self.bytes(payload)) == stored,
                        "a UEFI CRC32 section does not match its data",
                    )?;
                    nodes.extend(self.sections(payload, depth + 1, ui, false)?);
                    return Ok(());
                }
                let payload = match header.guid {
                    LZMA => self.decode_lzma(payload)?,
                    _ => {
                        if header.data_offset > SECTION_HEADER + GUID_HEADER {
                            nodes.push(Node::Leaf {
                                label: short_name(&header.guid) + PROPERTIES,
                                named: true,
                                data: Self::sub(
                                    section,
                                    SECTION_HEADER + GUID_HEADER,
                                    header.data_offset,
                                ),
                            });
                        }
                        payload
                    }
                };
                let mut inner_ui = None;
                let children = self.sections(payload, depth + 1, &mut inner_ui, true)?;
                nodes.push(Node::Container {
                    file: false,
                    name: inner_ui.unwrap_or_else(|| short_name(&header.guid)),
                    children,
                });
            }
            VOLUME_IMAGE => {
                format_check(
                    content.len() >= VOLUME_HEADER,
                    "a UEFI volume section is too short",
                )?;
                match volume_header(content[..VOLUME_HEADER].try_into().unwrap())? {
                    VolumeKind::Foreign => nodes.push(Node::Leaf {
                        label: foreign_volume_name(content),
                        named: true,
                        data: body,
                    }),
                    VolumeKind::Volume { .. } => {
                        nodes.push(Node::Volume(self.volume(body, depth + 1)?))
                    }
                }
            }
            USER_INTERFACE if ui_name(content).is_some() => *ui = ui_name(content),
            VERSION if content.len() >= 2 && utf16_units(&content[2..]).is_some() => {}
            DXE_DEPEX | PEI_DEPEX if depex_holds(content) => {}
            FREEFORM_SUBTYPE_GUID if content.len() >= 16 => nodes.push(leaf(
                short_name(content),
                Self::sub(body, 16, content.len()),
            )),
            other => nodes.push(leaf(section_extension(other), body)),
        }
        Ok(())
    }

    fn decode_lzma(&mut self, payload: Bytes) -> Parsed<Bytes> {
        let data = &self.buffers[payload.buffer][payload.start..payload.end];
        format_check(
            data.len() >= crate::lzma::LZMA_FILE_HEADER,
            "a UEFI LZMA section is cut inside its header",
        )?;
        let properties = crate::lzma::Properties::from_byte(data[0]).ok_or(Failure::Format(
            "a UEFI LZMA section's properties are out of range",
        ))?;
        let dictionary = le32(data, 1);
        let size = u64::from_le_bytes(data[5..crate::lzma::LZMA_FILE_HEADER].try_into().unwrap());
        format_check(
            size != u64::MAX,
            "a UEFI LZMA section does not give its size",
        )?;
        let size = reserve(&mut self.decoded, size)?;
        let mut decoded = vec![0u8; size];
        let input = &data[crate::lzma::LZMA_FILE_HEADER..];
        let mut reader =
            crate::lzma::RawLzmaReader::new(input, properties, dictionary, size as u64)
                .requiring_a_clean_end()
                .refusing_input_after_its_end("a UEFI LZMA section goes on after its stream");
        let mut after = [0u8; 1];
        let ends = reader
            .read_exact(&mut decoded)
            .and_then(|()| reader.read(&mut after))
            .is_ok_and(|read| read == 0);
        format_check(ends, "a UEFI LZMA section is damaged")?;
        Ok(self.keep(decoded))
    }
}

fn expand(children: Vec<Node>) -> Vec<(bool, Node)> {
    let alone = children.len() == 1;
    let mut out = Vec::with_capacity(children.len());
    for child in children {
        match child {
            Node::Volume(mut items) if items.len() == 1 => {
                out.push((true, item_node(items.pop().unwrap())))
            }
            Node::Volume(items) if alone => {
                out.extend(items.into_iter().map(|item| (true, item_node(item))))
            }
            Node::Volume(items) => out.push((
                true,
                item_node(Item {
                    name: VOLUME_FOLDER.to_owned(),
                    mergeable: false,
                    content: Content::Folder(items),
                }),
            )),
            other => out.push((false, other)),
        }
    }
    out
}

fn item_node(item: Item) -> Node {
    Node::Volume(vec![item])
}

fn render(node: Node) -> Item {
    match node {
        Node::Leaf { label, data, .. } => Item {
            name: label,
            mergeable: true,
            content: Content::Data(data),
        },
        Node::Volume(mut items) => items.pop().unwrap(),
        Node::Container {
            file,
            name,
            children,
        } => {
            let mut rendered: Vec<(Option<String>, Item)> = expand(children)
                .into_iter()
                .map(|(_, node)| match node {
                    Node::Leaf {
                        label,
                        named: false,
                        data,
                    } => (
                        Some(label),
                        Item {
                            name: String::new(),
                            mergeable: true,
                            content: Content::Data(data),
                        },
                    ),
                    other => (None, render(other)),
                })
                .collect();
            if let [(extension, child)] = &mut rendered[..]
                && (extension.is_some() || child.mergeable)
            {
                let label = extension
                    .take()
                    .unwrap_or_else(|| std::mem::take(&mut child.name));
                let (_, child) = rendered.pop().unwrap();
                return Item {
                    name: format!("{name}.{label}"),
                    mergeable: file,
                    content: child.content,
                };
            }
            let mut index = 0;
            let children = rendered
                .into_iter()
                .map(|(extension, mut item)| {
                    if let Some(extension) = extension {
                        item.name = format!("{index}.{extension}");
                        index += 1;
                    }
                    item
                })
                .collect();
            Item {
                name,
                mergeable: file,
                content: Content::Folder(children),
            }
        }
    }
}

struct Image {
    parser: Parser,
    items: Vec<Item>,
}

// SPEC(UEFI 2.10, 8.5.3 EFI_CAPSULE_HEADER): the capsule GUID, the header size, flags and the
// image size, which counts the header.
pub(crate) fn capsule_header(
    bytes: &[u8; CAPSULE_HEADER],
    length: u64,
) -> Option<std::ops::Range<usize>> {
    let header_size = le32(bytes, 16);
    let image_size = le32(bytes, 24);
    (bytes[..16] == CAPSULE
        && header_size as usize >= CAPSULE_HEADER
        && header_size < image_size
        && u64::from(image_size) <= length)
        .then_some(header_size as usize..image_size as usize)
}

fn open_capsule(
    parser: &mut Parser,
    whole: Bytes,
    payload: std::ops::Range<usize>,
) -> Parsed<Vec<Item>> {
    let body = Parser::sub(whole, payload.start, payload.end);
    let content = parser.bytes(body);
    format_check(
        content.len() >= VOLUME_HEADER,
        "a UEFI capsule is too short to hold a volume",
    )?;
    match volume_header(content[..VOLUME_HEADER].try_into().unwrap())? {
        VolumeKind::Foreign => Ok(vec![Item {
            name: foreign_volume_name(content),
            mergeable: true,
            content: Content::Data(body),
        }]),
        VolumeKind::Volume { .. } => parser.volume(body, 0),
    }
}

fn candidates(data: &[u8]) -> impl Iterator<Item = usize> + '_ {
    data.windows(SIGNATURE.len())
        .enumerate()
        .filter(|(at, window)| *at >= SIGNATURE_AT && window == SIGNATURE)
        .map(|(at, _)| at - SIGNATURE_AT)
}

fn is_volume_at(data: &[u8], start: usize) -> bool {
    data.get(start..start + VOLUME_HEADER)
        .and_then(|head| volume_header(head.try_into().unwrap()).ok())
        .is_some_and(|kind| matches!(kind, VolumeKind::Volume { .. }))
}

fn read_range(file: &ArchiveFile, start: u64, length: u64) -> Result<Vec<u8>> {
    ensure!(
        length <= LARGEST_VOLUME,
        crate::OverLimit(format!(
            "a UEFI volume is larger than {} MiB; {BACKEND_HINT}",
            LARGEST_VOLUME >> 20
        ))
    );
    let mut data = vec![0u8; length as usize];
    let mut done = 0;
    while done < data.len() {
        let read = file
            .read_at(&mut data[done..], start + done as u64)
            .context("cannot read the UEFI image")?;
        ensure!(read > 0, "the UEFI image is truncated");
        done += read;
    }
    Ok(data)
}

fn volume_starts(file: &ArchiveFile, length: u64) -> Result<Vec<u64>> {
    let mut starts = Vec::new();
    let mut chunk = vec![0u8; SCAN_CHUNK];
    let mut offset = 0u64;
    while offset < length && starts.len() < CANDIDATE_LIMIT {
        let want = (length - offset).min(SCAN_CHUNK as u64) as usize;
        let piece = &mut chunk[..want];
        let mut done = 0;
        while done < want {
            let read = file
                .read_at(&mut piece[done..], offset + done as u64)
                .context("cannot read the UEFI image")?;
            ensure!(read > 0, "the UEFI image is truncated");
            done += read;
        }
        for at in candidates(piece) {
            let start = offset + at as u64;
            if at + VOLUME_HEADER > want && start + VOLUME_HEADER as u64 <= length {
                let head = read_range(file, start, VOLUME_HEADER as u64)?;
                if is_volume_at(&head, 0) && !starts.contains(&start) {
                    starts.push(start);
                }
            } else if is_volume_at(piece, at) && !starts.contains(&start) {
                starts.push(start);
            }
        }
        if want < SCAN_CHUNK {
            break;
        }
        offset += (SCAN_CHUNK - VOLUME_HEADER) as u64;
    }
    starts.truncate(CANDIDATE_LIMIT);
    Ok(starts)
}

fn parser_of(data: Vec<u8>) -> (Parser, Bytes) {
    let whole = Bytes {
        buffer: 0,
        start: 0,
        end: data.len(),
    };
    (
        Parser {
            buffers: vec![data],
            decoded: 0,
        },
        whole,
    )
}

fn open(path: &Path) -> Result<Image> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    let mut refusal = "the file holds no UEFI volume";
    let capsule = (length >= CAPSULE_HEADER as u64)
        .then(|| read_range(&file, 0, CAPSULE_HEADER as u64))
        .transpose()?
        .and_then(|head| capsule_header(head[..].try_into().unwrap(), length));
    if let Some(payload) = capsule {
        let data = read_range(
            &file,
            payload.start as u64,
            (payload.end - payload.start) as u64,
        )?;
        let (mut parser, whole) = parser_of(data);
        match open_capsule(&mut parser, whole, 0..whole.end) {
            Ok(items) => return Ok(Image { parser, items }),
            Err(Failure::Headers(message)) => bail!("{message}"),
            Err(Failure::Format(message)) => refusal = message,
        }
    }
    for start in volume_starts(&file, length)? {
        let head = read_range(&file, start, VOLUME_HEADER as u64)?;
        let Ok(VolumeKind::Volume {
            length: declared, ..
        }) = volume_header(head[..].try_into().unwrap())
        else {
            continue;
        };
        let data = read_range(&file, start, declared.min(length - start))?;
        let (mut parser, whole) = parser_of(data);
        match parser.volume(whole, 0) {
            Ok(items) => return Ok(Image { parser, items }),
            Err(Failure::Headers(message)) => bail!("{message}"),
            Err(Failure::Format(message)) => refusal = message,
        }
    }
    bail!("{refusal}")
}

fn paths(items: &[Item], prefix: &str, out: &mut Vec<String>) {
    for item in items {
        let path = format!("{prefix}{}", item.name);
        if let Content::Folder(children) = &item.content {
            paths(children, &format!("{path}/"), out);
        }
        out.push(path);
    }
}

fn emit(tree: &mut tree::Tree, parser: &Parser, items: &[Item], prefix: &str) -> Result<()> {
    for item in items {
        tree.check()?;
        let path = format!("{prefix}{}", item.name);
        crate::safe_path(&path)?;
        match &item.content {
            Content::Folder(children) => {
                tree.directory(&path, FOLDER_MODE, None)?;
                emit(tree, parser, children, &format!("{path}/"))?;
            }
            Content::Data(at) => {
                let mut data = parser.bytes(*at);
                tree.file(&path, data.len() as u64, FILE_MODE, None, &mut data)?;
            }
        }
    }
    Ok(())
}

/// Returns whether the file starts with a UEFI capsule header of the GUID
/// 7-Zip opens as a capsule, or with a firmware volume header.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut head = vec![0u8; CAPSULE_HEADER.max(VOLUME_HEADER)];
    let mut read = 0;
    while read < head.len() {
        let got = file.read_at(&mut head[read..], read as u64)?;
        if got == 0 {
            return Ok(false);
        }
        read += got;
    }
    Ok(head[..16] == CAPSULE || is_volume_at(&head, 0))
}

/// Returns whether a firmware volume header lies in the first MiB of the
/// file, as in firmware images that start with code before their volume.
pub fn sniff_later_volume(path: &Path) -> Result<bool> {
    let mut head = Vec::new();
    crate::volumes::open(path)?
        .take(SNIFF_WINDOW)
        .read_to_end(&mut head)?;
    Ok(candidates(&head).any(|start| is_volume_at(&head, start)))
}

/// Returns whether the default engine should send the image to 7-Zip: one
/// that writes a name that is not a plain relative path, which 7-Zip
/// writes as it is.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let Ok(image) = open(path) else {
        return Ok(false);
    };
    let mut names = Vec::new();
    paths(&image.items, "", &mut names);
    Ok(names.iter().any(|name| crate::safe_path(name).is_err()))
}

/// Verify and optionally extract a UEFI firmware volume or capsule into a
/// new directory, in the tree `7zz x` writes. Every check of
/// `docs/reference/uefi-reader.md` holds before anything is published.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let image = open(path)?;
    tree::extract("UEFI", path, destination, options, |tree| {
        emit(tree, &image.parser, &image.items, "")
    })
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn volume_headers_never_panic_and_hold_their_block_map() {
        let bytes: [u8; VOLUME_HEADER] = kani::any();
        if let Ok(VolumeKind::Volume {
            length,
            header_length,
        }) = volume_header(&bytes)
        {
            assert!(header_length >= VOLUME_HEADER + BLOCK_MAP_ENTRY);
            assert!(header_length % 2 == 0);
            assert!(length >= header_length as u64);
        }
    }

    #[kani::proof]
    #[kani::unwind(40)]
    fn header_checks_never_panic_on_any_short_header() {
        let bytes: [u8; VOLUME_HEADER + 2 * BLOCK_MAP_ENTRY] = kani::any();
        let length: usize = kani::any();
        kani::assume(length >= VOLUME_HEADER && length <= bytes.len() && length % 2 == 0);
        if header_holds(&bytes[..length]) {
            assert!(bytes[length - BLOCK_MAP_ENTRY..length] == [0; BLOCK_MAP_ENTRY]);
        }
    }

    #[kani::proof]
    #[kani::unwind(25)]
    fn file_headers_never_panic_and_cover_their_header() {
        let bytes: [u8; FILE_HEADER] = kani::any();
        if let Ok(header) = file_header(&bytes) {
            assert!(header.size >= FILE_HEADER && header.size < 1 << 24);
            assert!(!bytes[23] & FILE_STATE_REQUIRED != 0);
        }
    }

    #[kani::proof]
    fn section_and_guid_headers_keep_their_data_inside_the_section() {
        let bytes: [u8; SECTION_HEADER] = kani::any();
        if let Ok((size, _)) = section_header(&bytes) {
            assert!(size >= SECTION_HEADER && size < EXTENDED_SIZE as usize);
        }
        let guid_bytes: [u8; GUID_HEADER] = kani::any();
        let section_size: usize = kani::any();
        if let Ok(found) = guid_section(&guid_bytes, section_size) {
            assert!(found.data_offset >= SECTION_HEADER + GUID_HEADER);
            assert!(found.data_offset <= section_size);
        }
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn dependency_expressions_never_panic_on_any_short_expression() {
        let bytes: [u8; 20] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let _ = depex_holds(&bytes[..length]);
    }

    #[kani::proof]
    fn capsule_headers_keep_their_payload_inside_the_file() {
        let bytes: [u8; CAPSULE_HEADER] = kani::any();
        let length: u64 = kani::any();
        if let Some(payload) = capsule_header(&bytes, length) {
            assert!(payload.start >= CAPSULE_HEADER && payload.start < payload.end);
            assert!(payload.end as u64 <= length);
        }
    }
}
