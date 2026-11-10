//! In-process extraction of Microsoft Compiled HTML Help (`.chm`, ITSF) and
//! Microsoft Help 2 (`.hxs`, ITOLITLS) files with LZX sections. See
//! `docs/reference/chm-reader.md` for what it accepts and how its tree
//! matches `7zz x`.
pub use crate::tree::ReadOptions;
use crate::tree::{self, BACKEND_HINT, MAX_ENTRIES};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check, lzx};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::HashMap, io::Read, ops::Range, path::Path};

const CHM_SIGNATURE: &[u8; 12] = b"ITSF\x03\0\0\0\x60\0\0\0";
const HELP2_SIGNATURE: &[u8; 16] = b"ITOLITLS\x01\0\0\0\x28\0\0\0";
const CHM_HEADER: usize = 0x60;
const HELP2_HEADER: usize = 0x160;
const HELP2_SHORT_HEADER: usize = 0x13C;
const HELP2_SECTIONS: u32 = 5;
const CAOL_AT: usize = 0x110;
const CAOL_WITHOUT_ITSF: u32 = 0x2C;
const CAOL_WITH_ITSF: u32 = 0x50;
const SECTION_ZERO: usize = 0x18;
const SECTION_ZERO_TAG: u32 = 0x01FE;
const ITSP_HEADER: usize = 0x54;
const IFCM_HEADER: usize = 0x20;
const PMGL_HEADER: usize = 20;
const AOLL_HEADER: usize = 0x30;
const SMALLEST_CHM_CHUNK: u32 = 32;
const SMALLEST_HELP2_CHUNK: u32 = 64;
const LARGEST_CHUNK: u32 = 1 << 20;
const QUICKREF_COUNT: usize = 2;
const LONGEST_ENCODED: usize = 9;
const LONGEST_NAME: u64 = 1 << 13;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_SYSTEM_FILE: u64 = 1 << 20;
const BLOCK: u64 = lzx::CHUNK as u64;
const LZX_CONTROL: usize = 24;
const LZX_CONTROL_DWORDS: u32 = 5;
const SMALLEST_DICTIONARY_BITS: u32 = 15;
const LARGEST_LOG: u32 = 16;
const LARGEST_OTHER_CONTROL_DWORDS: u32 = 1 << 27;
const RESET_TABLE_HEADER: usize = 40;
const RESET_ENTRY: u64 = 8;
const RESET_BATCH: usize = 8192;
const LARGEST_PACKED_BLOCK: u64 = 1 << 20;
const GUID: usize = 16;
const FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;
const NAME_LIST: &str = "::DataSpace/NameList";
const STORAGE: &str = "::DataSpace/Storage/";
const CHM_LZX_GUID: [u8; GUID] = [
    0x40, 0x89, 0xC2, 0x7F, 0x31, 0x9D, 0xD0, 0x11, 0x9B, 0x27, 0x00, 0xA0, 0xC9, 0x1E, 0x9C, 0x7C,
];
const HELP2_LZX_GUID: [u8; GUID] = [
    0xC6, 0x07, 0x90, 0x0A, 0x76, 0x40, 0xD3, 0x11, 0x87, 0x89, 0x00, 0x00, 0xF8, 0x10, 0x57, 0x54,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Chm,
    Help2,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Form::Chm => "CHM",
            Form::Help2 => "Help 2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    offset: u64,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    section_zero: Span,
    directory: Span,
    content: u64,
    entries: Option<u64>,
    end: u64,
}

fn end_of(spans: &[Span], at_least: u64) -> Field<u64> {
    spans.iter().try_fold(at_least, |end, span| {
        let span_end = span
            .offset
            .checked_add(span.size)
            .ok_or(BadField("header section ends past 2^64 bytes"))?;
        Ok(end.max(span_end))
    })
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn span_at(bytes: &[u8], at: usize) -> Span {
    Span {
        offset: u64_at(bytes, at),
        size: u64_at(bytes, at + 8),
    }
}

fn chm_header(bytes: &[u8; CHM_HEADER]) -> Field<Layout> {
    field_check(
        bytes[..CHM_SIGNATURE.len()] == CHM_SIGNATURE[..],
        "not a CHM file: no ITSF version 3 header of 0x60 bytes",
    )?;
    field_check(
        matches!(u32_at(bytes, 12), 0 | 1),
        "CHM header field after its size is not 0 or 1",
    )?;
    Ok(Layout {
        section_zero: span_at(bytes, 0x38),
        directory: span_at(bytes, 0x48),
        content: u64_at(bytes, 0x58),
        entries: None,
        end: end_of(
            &[span_at(bytes, 0x38), span_at(bytes, 0x48)],
            CHM_HEADER as u64,
        )?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Help2Header {
    section_zero: Span,
    directory: Span,
    end: u64,
    entries: u64,
    caol: u32,
}

fn help2_header(bytes: &[u8; HELP2_SHORT_HEADER]) -> Field<Help2Header> {
    field_check(
        bytes[..HELP2_SIGNATURE.len()] == HELP2_SIGNATURE[..],
        "not a Help 2 file: no ITOLITLS version 1 header",
    )?;
    field_check(
        u32_at(bytes, 16) == HELP2_SECTIONS,
        "Help 2 header does not list five header sections",
    )?;
    field_check(
        &bytes[CAOL_AT..CAOL_AT + 4] == b"CAOL" && u32_at(bytes, CAOL_AT + 4) == 2,
        "Help 2 header has no CAOL version 2 record",
    )?;
    let caol = u32_at(bytes, CAOL_AT + 8);
    field_check(
        caol == CAOL_WITH_ITSF || caol == CAOL_WITHOUT_ITSF,
        "Help 2 CAOL record is neither 0x2C nor 0x50 bytes",
    )?;
    Ok(Help2Header {
        section_zero: span_at(bytes, 0x28),
        directory: span_at(bytes, 0x38),
        end: end_of(
            &[0, 1, 2, 3, 4].map(|section| span_at(bytes, 0x28 + 16 * section)),
            0,
        )?,
        entries: u64_at(bytes, 0xB8),
        caol,
    })
}

fn help2_itsf(bytes: &[u8; HELP2_HEADER - HELP2_SHORT_HEADER]) -> Field<u64> {
    field_check(
        &bytes[4..8] == b"ITSF" && u32_at(bytes, 8) == 4 && u32_at(bytes, 12) == 0x20,
        "Help 2 CAOL record holds no ITSF version 4 record of 0x20 bytes",
    )?;
    field_check(
        matches!(u32_at(bytes, 16), 0 | 1),
        "Help 2 ITSF field after its size is not 0 or 1",
    )?;
    Ok(u64_at(bytes, 20))
}

fn section_zero(bytes: &[u8; SECTION_ZERO]) -> Field<u64> {
    field_check(
        u32_at(bytes, 0) == SECTION_ZERO_TAG,
        "CHM header section 0 does not start with 0x1FE",
    )?;
    Ok(u64_at(bytes, 8))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Listing {
    chunk_size: u32,
    chunks: u32,
}

fn itsp_header(bytes: &[u8; ITSP_HEADER]) -> Field<Listing> {
    field_check(
        &bytes[..4] == b"ITSP" && u32_at(bytes, 4) == 1,
        "CHM directory does not start with an ITSP version 1 header",
    )?;
    let chunk_size = u32_at(bytes, 16);
    field_check(
        (SMALLEST_CHM_CHUNK..=LARGEST_CHUNK).contains(&chunk_size),
        "CHM directory chunks are not 32 bytes to 1 MiB",
    )?;
    Ok(Listing {
        chunk_size,
        chunks: u32_at(bytes, 0x2C),
    })
}

fn ifcm_header(bytes: &[u8; IFCM_HEADER]) -> Field<Listing> {
    field_check(
        &bytes[..4] == b"IFCM" && u32_at(bytes, 4) == 1,
        "Help 2 directory does not start with an IFCM version 1 header",
    )?;
    let chunk_size = u32_at(bytes, 8);
    field_check(
        (SMALLEST_HELP2_CHUNK..=LARGEST_CHUNK).contains(&chunk_size),
        "Help 2 directory chunks are not 64 bytes to 1 MiB",
    )?;
    Ok(Listing {
        chunk_size,
        chunks: u32_at(bytes, 24),
    })
}

fn encoded_integer(bytes: &[u8], at: &mut usize) -> Field<u64> {
    let mut value = 0u64;
    for _ in 0..LONGEST_ENCODED {
        let byte = *bytes
            .get(*at)
            .ok_or(BadField("CHM directory entry runs past its listing"))?;
        *at += 1;
        value |= u64::from(byte & 0x7F);
        if byte < 0x80 {
            return Ok(value);
        }
        value <<= 7;
    }
    Err(BadField("CHM encoded integer is longer than nine bytes"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryEntry {
    name: Range<usize>,
    section: u64,
    offset: u64,
    size: u64,
}

fn directory_entry(listing: &[u8], at: &mut usize) -> Field<DirectoryEntry> {
    let length = encoded_integer(listing, at)?;
    field_check(
        (1..=LONGEST_NAME).contains(&length),
        "CHM directory entry name is empty or longer than 8,192 bytes",
    )?;
    let start = *at;
    let end = start + length as usize;
    field_check(
        end <= listing.len(),
        "CHM directory entry runs past its listing",
    )?;
    *at = end;
    Ok(DirectoryEntry {
        name: start..end,
        section: encoded_integer(listing, at)?,
        offset: encoded_integer(listing, at)?,
        size: encoded_integer(listing, at)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chunk {
    Listing,
    Index,
}

fn chunk_kind(chunk: &[u8], form: Form) -> Field<Chunk> {
    let (listing, index) = match form {
        Form::Chm => (b"PMGL", b"PMGI"),
        Form::Help2 => (b"AOLL", b"AOLI"),
    };
    match chunk.get(..4) {
        Some(tag) if tag == listing => Ok(Chunk::Listing),
        Some(tag) if tag == index => Ok(Chunk::Index),
        _ => Err(BadField(
            "CHM directory chunk is neither a listing nor an index",
        )),
    }
}

fn walk_listing(
    chunk: &[u8],
    form: Form,
    mut each: impl FnMut(DirectoryEntry) -> Field<()>,
) -> Field<u64> {
    let header = match form {
        Form::Chm => PMGL_HEADER,
        Form::Help2 => AOLL_HEADER,
    };
    field_check(
        chunk.len() >= header,
        "CHM listing chunk is shorter than its header",
    )?;
    let quickref = u32_at(chunk, 4) as usize;
    field_check(
        (QUICKREF_COUNT..=chunk.len()).contains(&quickref),
        "CHM listing quickref area is under 2 bytes or larger than its chunk",
    )?;
    let limit = chunk.len() - quickref;
    field_check(
        header <= limit,
        "CHM listing quickref area overlaps its chunk header",
    )?;
    let listing = &chunk[..limit];
    let mut at = header;
    let mut count = 0u64;
    while at < limit {
        each(directory_entry(listing, &mut at)?)?;
        count += 1;
    }
    let stored = u64::from(u16_at(chunk, chunk.len() - QUICKREF_COUNT));
    // EXTERNAL(7-Zip ChmIn): a CHM listing may store 0 for its entry count, as Lazarus 9-26-2 does.
    field_check(
        stored == count || form == Form::Chm && stored == 0,
        "CHM listing chunk stores a different entry count",
    )?;
    Ok(count)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LzxControl {
    dwords: u32,
    interval_bits: u32,
    window_bits: u32,
}

fn exact_log(value: u32) -> Option<u32> {
    (value.is_power_of_two() && value.trailing_zeros() <= LARGEST_LOG)
        .then_some(value.trailing_zeros())
}

fn lzx_control(bytes: &[u8; LZX_CONTROL]) -> Field<LzxControl> {
    let dwords = u32_at(bytes, 0);
    field_check(
        dwords >= LZX_CONTROL_DWORDS && &bytes[4..8] == b"LZXC",
        "CHM LZX control data does not start with LZXC",
    )?;
    field_check(
        matches!(u32_at(bytes, 8), 2 | 3),
        "CHM LZX control data is not version 2 or 3",
    )?;
    let interval_bits = exact_log(u32_at(bytes, 12)).ok_or(BadField(
        "CHM LZX reset interval is not a power of two up to 2^16 blocks",
    ))?;
    let window_bits = exact_log(u32_at(bytes, 16)).ok_or(BadField(
        "CHM LZX window size is not a power of two up to 2^16 units",
    ))?;
    Ok(LzxControl {
        dwords,
        interval_bits,
        window_bits: SMALLEST_DICTIONARY_BITS + window_bits,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResetTable {
    entries: u64,
    uncompressed: u64,
    compressed: u64,
    extra_entry: bool,
}

fn reset_table_header(bytes: &[u8; RESET_TABLE_HEADER], size: u64) -> Field<ResetTable> {
    field_check(
        matches!(u32_at(bytes, 0), 2 | 3),
        "CHM reset table is not version 2 or 3",
    )?;
    let entries = u64::from(u32_at(bytes, 4));
    field_check(
        u32_at(bytes, 8) == RESET_ENTRY as u32 && u32_at(bytes, 12) == RESET_TABLE_HEADER as u32,
        "CHM reset table entries are not 8 bytes after a 40-byte header",
    )?;
    field_check(
        RESET_TABLE_HEADER as u64 + RESET_ENTRY * entries == size,
        "CHM reset table size does not match its entry count",
    )?;
    let uncompressed = u64_at(bytes, 16);
    let compressed = u64_at(bytes, 24);
    field_check(
        u64_at(bytes, 32) == BLOCK,
        "CHM reset table blocks are not 32 KiB",
    )?;
    // EXTERNAL(7-Zip ChmIn): the block count is (size + 32 KiB + 1) / 32 KiB, and a table may
    // list one entry more, which must then equal the compressed size.
    let blocks = uncompressed
        .checked_add(BLOCK + 1)
        .ok_or(BadField("CHM reset table uncompressed size overflows"))?
        / BLOCK;
    field_check(
        entries == blocks || entries == blocks + 1,
        "CHM reset table entry count does not match its uncompressed size",
    )?;
    Ok(ResetTable {
        entries,
        uncompressed,
        compressed,
        extra_entry: entries == blocks + 1,
    })
}

#[derive(Default)]
struct OffsetCheck {
    previous: Option<u64>,
}

impl OffsetCheck {
    fn take(&mut self, offset: u64) -> Field<()> {
        match self.previous {
            None => field_check(offset == 0, "CHM reset table does not start at offset 0")?,
            Some(previous) => {
                field_check(offset >= previous, "CHM reset table offsets go backwards")?
            }
        }
        self.previous = Some(offset);
        Ok(())
    }

    fn finish(self, table: &ResetTable) -> Field<()> {
        field_check(
            !table.extra_entry || self.previous == Some(table.compressed),
            "CHM reset table's extra entry is not its compressed size",
        )
    }
}

fn block_input(table: &ResetTable, index: u64, start: u64, next: Option<u64>) -> Field<Range<u64>> {
    field_check(
        index < table.entries,
        "CHM entry reaches past the blocks its reset table lists",
    )?;
    let end = match next {
        Some(next) if index + 1 < table.entries => next,
        _ => table.compressed,
    };
    field_check(
        start < end && end - start <= LARGEST_PACKED_BLOCK,
        "CHM LZX block is empty, ends before it starts or packs more than 1 MiB",
    )?;
    Ok(start..end)
}

fn guid_string(guid: &[u8; GUID]) -> String {
    let hex = |bytes: &mut dyn Iterator<Item = &u8>| -> String {
        bytes.map(|byte| format!("{byte:02X}")).collect()
    };
    format!(
        "{{{}-{}-{}-{}-{}}}",
        hex(&mut guid[0..4].iter().rev()),
        hex(&mut guid[4..6].iter().rev()),
        hex(&mut guid[6..8].iter().rev()),
        hex(&mut guid[8..10].iter()),
        hex(&mut guid[10..16].iter()),
    )
}

fn name_list(bytes: &[u8]) -> Field<Vec<String>> {
    let mut at = 2;
    let mut word = || -> Field<u16> {
        let pair = bytes
            .get(at..at + 2)
            .ok_or(BadField("CHM name list is truncated"))?;
        at += 2;
        Ok(u16::from_le_bytes([pair[0], pair[1]]))
    };
    let count = word()?;
    let mut names = Vec::new();
    for _ in 0..count {
        let length = word()?;
        let mut units = Vec::new();
        let mut ended = false;
        for _ in 0..length {
            let unit = word()?;
            ended |= unit == 0;
            if !ended {
                units.push(unit);
            }
        }
        field_check(word()? == 0, "CHM name list name is not followed by 0")?;
        names.push(
            String::from_utf16(&units).map_err(|_| BadField("CHM section name is not UTF-16"))?,
        );
    }
    Ok(names)
}

/// Returns whether the file starts with a CHM or Help 2 signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(HELP2_SIGNATURE.len());
    crate::volumes::open(path)?
        .take(HELP2_SIGNATURE.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic.starts_with(CHM_SIGNATURE) || magic.starts_with(HELP2_SIGNATURE))
}

/// Returns whether the default engine sends this file to 7-Zip: a Help 2
/// file whose CAOL record has no ITSF part, which 7-Zip lists as one text
/// item and Zipper does not read.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut bytes = [0u8; HELP2_SHORT_HEADER];
    if read_exact_at(&file, &mut bytes, 0).is_err() {
        return Ok(false);
    }
    Ok(help2_header(&bytes).is_ok_and(|header| header.caol == CAOL_WITHOUT_ITSF))
}

fn read_exact_at(file: &ArchiveFile, mut buffer: &mut [u8], mut offset: u64) -> Result<()> {
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        ensure!(read > 0, "unexpected end of file");
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct Stored {
    section: u64,
    offset: u64,
    size: u64,
}

struct Item {
    name: Range<u32>,
    directory: bool,
    stored: Stored,
    index: u32,
}

struct LzxSection {
    base: u64,
    interval_bits: u32,
    window_bits: u32,
    table: ResetTable,
    table_at: u64,
}

enum Method {
    Lzx(LzxSection),
    Other(String),
}

struct Archive {
    form: Form,
    file: ArchiveFile,
    length: u64,
    content: u64,
    items: Vec<Item>,
    names: String,
    system: HashMap<Vec<u8>, Stored>,
    sections: Vec<Method>,
}

fn readable(span: Span, length: u64) -> u64 {
    span.size.min(length.saturating_sub(span.offset))
}

impl Archive {
    fn open(path: &Path) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut head = [0u8; HELP2_SIGNATURE.len()];
        let form = match file.read_at(&mut head, 0)? {
            read if head[..read].starts_with(CHM_SIGNATURE) => Form::Chm,
            read if head[..read].starts_with(HELP2_SIGNATURE) => Form::Help2,
            _ => bail!("not a CHM or Help 2 file"),
        };
        let layout = match form {
            Form::Chm => {
                let mut bytes = [0u8; CHM_HEADER];
                read_exact_at(&file, &mut bytes, 0).context("truncated CHM header")?;
                chm_header(&bytes)?
            }
            Form::Help2 => {
                let mut bytes = [0u8; HELP2_HEADER];
                read_exact_at(&file, &mut bytes[..HELP2_SHORT_HEADER], 0)
                    .context("truncated Help 2 header")?;
                let header = help2_header(bytes[..HELP2_SHORT_HEADER].try_into().unwrap())?;
                ensure!(
                    header.caol == CAOL_WITH_ITSF,
                    "Help 2 file has no ITSF record in its CAOL record, which Zipper does not read; {BACKEND_HINT}"
                );
                read_exact_at(
                    &file,
                    &mut bytes[HELP2_SHORT_HEADER..],
                    HELP2_SHORT_HEADER as u64,
                )
                .context("truncated Help 2 header")?;
                Layout {
                    section_zero: header.section_zero,
                    directory: header.directory,
                    content: help2_itsf(bytes[HELP2_SHORT_HEADER..].try_into().unwrap())?,
                    entries: Some(header.entries),
                    end: header.end,
                }
            }
        };
        let label = form.label();
        let mut zero = [0u8; SECTION_ZERO];
        ensure!(
            readable(layout.section_zero, length) >= SECTION_ZERO as u64,
            "truncated {label} header section 0"
        );
        read_exact_at(&file, &mut zero, layout.section_zero.offset)?;
        let end = layout.end.max(section_zero(&zero)?);
        ensure!(
            end <= length,
            "{label} header places data up to byte {end}, past the end of the {length}-byte file"
        );
        let mut archive = Archive {
            form,
            file,
            length,
            content: layout.content,
            items: Vec::new(),
            names: String::new(),
            system: HashMap::new(),
            sections: Vec::new(),
        };
        archive.read_directory(&layout)?;
        archive.read_sections()?;
        archive.sort_and_check()?;
        Ok(archive)
    }

    fn read_directory(&mut self, layout: &Layout) -> Result<()> {
        let label = self.form.label();
        let available = readable(layout.directory, self.length);
        let header = match self.form {
            Form::Chm => ITSP_HEADER,
            Form::Help2 => IFCM_HEADER,
        };
        ensure!(
            available >= header as u64,
            "truncated {label} directory header"
        );
        let mut bytes = vec![0u8; header];
        read_exact_at(&self.file, &mut bytes, layout.directory.offset)?;
        let listing = match self.form {
            Form::Chm => itsp_header(bytes.as_slice().try_into().unwrap())?,
            Form::Help2 => ifcm_header(bytes.as_slice().try_into().unwrap())?,
        };
        ensure!(
            u64::from(listing.chunks) * u64::from(listing.chunk_size) <= available - header as u64,
            "truncated {label} directory: {} chunks of {} bytes do not fit",
            listing.chunks,
            listing.chunk_size
        );
        let mut chunk = vec![0u8; listing.chunk_size as usize];
        let mut entries = layout.entries;
        let mut name_bytes = 0usize;
        let mut at = layout.directory.offset + header as u64;
        for _ in 0..listing.chunks {
            read_exact_at(&self.file, &mut chunk, at)?;
            at += u64::from(listing.chunk_size);
            if chunk_kind(&chunk, self.form)? == Chunk::Index {
                continue;
            }
            let mut found = Vec::new();
            let count = walk_listing(&chunk, self.form, |entry| {
                found.push(entry);
                Ok(())
            })?;
            if let Some(left) = &mut entries {
                *left = left
                    .checked_sub(count)
                    .context("Help 2 directory lists more entries than its header counts")?;
            }
            for entry in found {
                self.add_entry(&chunk[entry.name.clone()], &entry, &mut name_bytes)?;
            }
        }
        ensure!(
            entries.is_none_or(|left| left == 0),
            "Help 2 directory lists fewer entries than its header counts"
        );
        Ok(())
    }

    fn add_entry(
        &mut self,
        name: &[u8],
        entry: &DirectoryEntry,
        name_bytes: &mut usize,
    ) -> Result<()> {
        let label = self.form.label();
        let index = self.items.len() + self.system.len();
        ensure!(
            index < MAX_ENTRIES,
            "{label} directory has more than {MAX_ENTRIES} entries"
        );
        *name_bytes += name.len();
        ensure!(
            *name_bytes <= MAX_NAME_BYTES,
            "{label} names exceed 64 MiB in total"
        );
        ensure!(
            !name.contains(&0),
            "{label} entry name {:?} holds a NUL byte",
            String::from_utf8_lossy(name)
        );
        let stored = Stored {
            section: entry.section,
            offset: entry.offset,
            size: entry.size,
        };
        if name.starts_with(b"::") {
            self.system.entry(name.to_vec()).or_insert(stored);
        } else if name.len() > 1 && name[0] == b'/' {
            let text = std::str::from_utf8(&name[1..]).map_err(|_| {
                anyhow::anyhow!(
                    "{label} entry name {:?} is not UTF-8; {BACKEND_HINT}",
                    String::from_utf8_lossy(name)
                )
            })?;
            let start = self.names.len() as u32;
            self.names.push_str(text);
            self.items.push(Item {
                name: start..self.names.len() as u32,
                directory: text.ends_with('/'),
                stored,
                index: index as u32,
            });
        }
        Ok(())
    }

    fn name(&self, item: &Item) -> &str {
        &self.names[item.name.start as usize..item.name.end as usize]
    }

    fn find(&self, name: &str) -> Result<Stored> {
        self.system.get(name.as_bytes()).copied().with_context(|| {
            format!(
                "{} file has no {name}, which 7-Zip lists only as raw entries; {BACKEND_HINT}",
                self.form.label()
            )
        })
    }

    fn start_of(&self, stored: &Stored) -> Result<u64> {
        self.content
            .checked_add(stored.offset)
            .with_context(|| format!("{} entry offset overflows", self.form.label()))
    }

    fn system_file(&self, name: &str) -> Result<(Vec<u8>, Stored)> {
        let stored = self.find(name)?;
        let start = self.start_of(&stored)?;
        let size = stored
            .size
            .min(MAX_SYSTEM_FILE)
            .min(self.length.saturating_sub(start));
        let mut bytes = vec![0u8; size as usize];
        read_exact_at(&self.file, &mut bytes, start)?;
        Ok((bytes, stored))
    }

    fn read_sections(&mut self) -> Result<()> {
        let label = self.form.label();
        let names = name_list(&self.system_file(NAME_LIST)?.0)
            .map_err(|error| anyhow::anyhow!("{error}; {BACKEND_HINT}"))?;
        for name in names.iter().skip(1) {
            let method = self.read_section(name)?;
            self.sections.push(method);
        }
        for item in &self.items {
            ensure!(
                item.directory
                    || item.stored.section == 0
                    || item.stored.section <= self.sections.len() as u64,
                "{label} entry {:?} names section {}, and the file has {}",
                self.name(item),
                item.stored.section,
                self.sections.len() + 1
            );
        }
        Ok(())
    }

    fn read_section(&self, name: &str) -> Result<Method> {
        let label = self.form.label();
        let prefix = format!("{STORAGE}{name}/");
        let content = self.find(&format!("{prefix}Content"))?;
        let transforms = format!("{prefix}Transform/");
        let guids: Vec<[u8; GUID]> = match self.form {
            Form::Chm => vec![CHM_LZX_GUID],
            Form::Help2 => {
                let (bytes, stored) = self.system_file(&format!("{transforms}List"))?;
                ensure!(
                    stored.size.is_multiple_of(GUID as u64)
                        && stored.size >= GUID as u64
                        && bytes.len() as u64 == stored.size,
                    "Help 2 transform list of section {name:?} is not a whole number of GUIDs"
                );
                bytes
                    .chunks_exact(GUID)
                    .map(|guid| guid.try_into().unwrap())
                    .collect()
            }
        };
        let (control, control_stored) = self.system_file(&format!("{prefix}ControlData"))?;
        let control_limit = control_stored
            .size
            .min(self.length.saturating_sub(self.start_of(&control_stored)?));
        let mut at = 0u64;
        let mut controls = Vec::with_capacity(guids.len());
        for guid in &guids {
            let start = at as usize;
            if is_lzx(guid) {
                let bytes = control
                    .get(start..start + LZX_CONTROL)
                    .with_context(|| format!("truncated {label} control data"))?;
                let parsed = lzx_control(bytes.try_into().unwrap())?;
                at += 4 + u64::from(parsed.dwords) * 4;
                controls.push(Some(parsed));
            } else {
                let bytes = control
                    .get(start..start + 4)
                    .with_context(|| format!("truncated {label} control data"))?;
                let dwords = u32_at(bytes, 0);
                ensure!(
                    dwords <= LARGEST_OTHER_CONTROL_DWORDS,
                    "{label} control data of a transform is longer than 2^27 words"
                );
                at += 4 + u64::from(dwords) * 4;
                controls.push(None);
            }
            ensure!(at <= control_limit, "truncated {label} control data");
        }
        let (span, _) = self.system_file(&format!("{prefix}SpanInfo"))?;
        let span_size = u64_at(
            span.get(..8)
                .with_context(|| format!("truncated {label} span information"))?,
            0,
        );
        let mut tables = Vec::with_capacity(guids.len());
        for guid in guids.iter().filter(|guid| is_lzx(guid)) {
            let path = format!("{transforms}{}/InstanceData/ResetTable", guid_string(guid));
            tables.push(self.reset_table(&path, span_size)?);
        }
        Ok(
            match (guids.as_slice(), controls.as_slice(), tables.as_slice()) {
                ([_], [Some(control)], [(table, table_at)]) => Method::Lzx(LzxSection {
                    base: self.start_of(&content)?,
                    interval_bits: control.interval_bits,
                    window_bits: control.window_bits,
                    table: *table,
                    table_at: *table_at,
                }),
                _ => Method::Other(guids.iter().map(guid_string).collect::<Vec<_>>().join(" ")),
            },
        )
    }

    fn reset_table(&self, path: &str, span_size: u64) -> Result<(ResetTable, u64)> {
        let label = self.form.label();
        let stored = self.find(path)?;
        let start = self.start_of(&stored)?;
        if stored.size < 4 {
            // EXTERNAL(7-Zip ChmIn): a .chw file holds an empty reset table for an empty section.
            ensure!(
                stored.size == 0 && span_size == 0,
                "{label} reset table is shorter than its header"
            );
            return Ok((
                ResetTable {
                    entries: 0,
                    uncompressed: 0,
                    compressed: 0,
                    extra_entry: false,
                },
                start,
            ));
        }
        ensure!(
            stored.size >= RESET_TABLE_HEADER as u64
                && start
                    .checked_add(stored.size)
                    .is_some_and(|end| end <= self.length),
            "truncated {label} reset table"
        );
        let mut header = [0u8; RESET_TABLE_HEADER];
        read_exact_at(&self.file, &mut header, start)?;
        let table = reset_table_header(&header, stored.size)?;
        let table_at = start + RESET_TABLE_HEADER as u64;
        let mut check = OffsetCheck::default();
        let mut batch = vec![0u8; RESET_BATCH * RESET_ENTRY as usize];
        let mut done = 0u64;
        while done < table.entries {
            let count = (table.entries - done).min(RESET_BATCH as u64) as usize;
            let bytes = &mut batch[..count * RESET_ENTRY as usize];
            read_exact_at(&self.file, bytes, table_at + done * RESET_ENTRY)?;
            for entry in bytes.chunks_exact(RESET_ENTRY as usize) {
                check.take(u64_at(entry, 0))?;
            }
            done += count as u64;
        }
        check.finish(&table)?;
        Ok((table, table_at))
    }

    fn sort_and_check(&mut self) -> Result<()> {
        let label = self.form.label();
        self.items.sort_by_key(|item| {
            let directory = item.directory;
            (
                !directory,
                if directory { 0 } else { item.stored.section },
                if directory { 0 } else { item.stored.offset },
                if directory { 0 } else { item.stored.size },
                item.index,
            )
        });
        let mut previous_section = 0;
        let mut reached = 0u64;
        let mut held: Option<(u64, u64)> = None;
        for item in self.items.iter().filter(|item| !item.directory) {
            let Stored {
                section,
                offset,
                size,
            } = item.stored;
            if section == 0 {
                continue;
            }
            let end = offset.checked_add(size).with_context(|| {
                format!("{label} entry {:?} ends past 2^64 bytes", self.name(item))
            })?;
            if size > 0 {
                ensure!(
                    held.is_none_or(
                        |(held_section, held_end)| held_section != section || offset >= held_end
                    ),
                    "{label} entry {:?} shares bytes with the entry before it",
                    self.name(item)
                );
                held = Some((section, end));
            }
            // EXTERNAL(7-Zip ChmIn): the first entry of each section sets no end for the next.
            if section != previous_section {
                previous_section = section;
                reached = 0;
                continue;
            }
            ensure!(
                offset >= reached,
                "{label} entry {:?} starts before the entry before it ends",
                self.name(item)
            );
            reached = end;
        }
        Ok(())
    }
}

fn is_lzx(guid: &[u8; GUID]) -> bool {
    *guid == CHM_LZX_GUID || *guid == HELP2_LZX_GUID
}

struct Stretch<'a> {
    file: &'a ArchiveFile,
    at: u64,
}

impl Read for Stretch<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

struct Decoder {
    lzx: Option<Box<lzx::Lzx>>,
    packed: Vec<u8>,
    block: Vec<u8>,
}

struct SectionStream<'a> {
    file: &'a ArchiveFile,
    section: &'a LzxSection,
    decoder: &'a mut Decoder,
    decoded: Option<u64>,
    position: u64,
}

impl<'a> SectionStream<'a> {
    fn open(
        file: &'a ArchiveFile,
        section: &'a LzxSection,
        decoder: &'a mut Decoder,
    ) -> Result<Self> {
        let window_bits = u8::try_from(section.window_bits)
            .ok()
            .filter(|bits| (lzx::MIN_WINDOW_BITS..=lzx::MAX_WINDOW_BITS).contains(bits))
            .context("CHM LZX window is not 2^15 to 2^21 bytes")?;
        match &mut decoder.lzx {
            Some(lzx) => lzx.reset_to_window(window_bits)?,
            None => decoder.lzx = Some(Box::new(lzx::Lzx::new(window_bits, None)?)),
        }
        if decoder.block.is_empty() {
            decoder.block = vec![0; BLOCK as usize];
        }
        Ok(SectionStream {
            file,
            section,
            decoder,
            decoded: None,
            position: 0,
        })
    }

    fn table_entry(&self, index: u64) -> Result<u64> {
        let mut bytes = [0u8; RESET_ENTRY as usize];
        read_exact_at(
            self.file,
            &mut bytes,
            self.section.table_at + index * RESET_ENTRY,
        )?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn decode(&mut self, index: u64, fresh: bool) -> Result<()> {
        let table = &self.section.table;
        ensure!(
            index < table.entries,
            "CHM entry reaches past the blocks its reset table lists"
        );
        let start = self.table_entry(index)?;
        let next = if index + 1 < table.entries {
            Some(self.table_entry(index + 1)?)
        } else {
            None
        };
        let range = block_input(table, index, start, next)?;
        let at = self
            .section
            .base
            .checked_add(range.start)
            .context("CHM LZX block offset overflows")?;
        let size = (range.end - range.start) as usize;
        self.decoder.packed.resize(size, 0);
        read_exact_at(self.file, &mut self.decoder.packed, at)
            .context("truncated CHM LZX block")?;
        let lzx = self.decoder.lzx.as_mut().unwrap();
        if fresh {
            lzx.reset();
        }
        let output = lzx
            .chunk(&self.decoder.packed, BLOCK as usize)
            .map_err(|error| anyhow::anyhow!("damaged CHM LZX block {index}: {error}"))?;
        self.decoder.block.copy_from_slice(output);
        self.decoded = Some(index);
        Ok(())
    }

    fn reach(&mut self, block: u64) -> Result<()> {
        let first = block >> self.section.interval_bits << self.section.interval_bits;
        let mut next = match self.decoded {
            Some(decoded) if decoded == block => return Ok(()),
            Some(decoded) if (first..block).contains(&decoded) => decoded + 1,
            Some(decoded) if decoded > block => bail!("CHM entries go backwards in their section"),
            _ => first,
        };
        while next <= block {
            self.decode(next, next == first)?;
            next += 1;
        }
        Ok(())
    }
}

impl Read for SectionStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let block = self.position / BLOCK;
        self.reach(block).map_err(std::io::Error::other)?;
        let within = (self.position % BLOCK) as usize;
        let count = buffer.len().min(BLOCK as usize - within);
        buffer[..count].copy_from_slice(&self.decoder.block[within..within + count]);
        self.position += count as u64;
        Ok(count)
    }
}

fn write_stored(tree: &mut tree::Tree, archive: &Archive, item: &Item) -> Result<()> {
    let size = item.stored.size;
    if size == 0 {
        return tree.file(
            archive.name(item),
            0,
            FILE_MODE,
            None,
            &mut std::io::empty(),
        );
    }
    let start = archive.start_of(&item.stored)?;
    ensure!(
        start
            .checked_add(size)
            .is_some_and(|end| end <= archive.length),
        "{} entry {:?} runs past the end of the file",
        archive.form.label(),
        archive.name(item)
    );
    let mut data = Stretch {
        file: &archive.file,
        at: start,
    }
    .take(size);
    tree.file(archive.name(item), size, FILE_MODE, None, &mut data)
}

fn parse(tree: &mut tree::Tree, archive: &Archive) -> Result<()> {
    let label = archive.form.label();
    let mut items = archive.items.iter().peekable();
    while let Some(item) = items.next_if(|item| item.directory) {
        tree.check()?;
        tree.directory(archive.name(item), DIRECTORY_MODE, None)?;
    }
    let mut decoder = Decoder {
        lzx: None,
        packed: Vec::new(),
        block: Vec::new(),
    };
    while let Some(item) = items.next() {
        tree.check()?;
        let section = item.stored.section;
        if section == 0 || item.stored.size == 0 {
            write_stored(tree, archive, item)?;
            continue;
        }
        let lzx_section = match &archive.sections[section as usize - 1] {
            Method::Lzx(lzx_section) => lzx_section,
            Method::Other(guids) => bail!(
                "{label} entry {:?} is in a section transformed by {guids}, which Zipper and 7-Zip do not decode",
                archive.name(item)
            ),
        };
        let mut stream = SectionStream::open(&archive.file, lzx_section, &mut decoder)?;
        let mut next = Some(item);
        while let Some(item) = next {
            tree.check()?;
            if item.stored.size == 0 {
                write_stored(tree, archive, item)?;
            } else {
                stream.position = item.stored.offset;
                let mut data = (&mut stream).take(item.stored.size);
                tree.file(
                    archive.name(item),
                    item.stored.size,
                    FILE_MODE,
                    None,
                    &mut data,
                )?;
            }
            next = items.next_if(|later| later.stored.section == section);
        }
    }
    Ok(())
}

/// Verify and optionally extract a Compiled HTML Help (`.chm`) or Help 2
/// (`.hxs`) file into a new directory. It writes every entry whose name
/// starts with `/`, as `7zz x` does, including the `#` and `$` system
/// files, and leaves out the `::DataSpace` storage. It reads entries stored
/// in section 0 and LZX sections, and refuses any other transform.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let archive = Archive::open(path)?;
    tree::extract(archive.form.label(), path, destination, options, |tree| {
        parse(tree, &archive)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn reference_integer(bytes: &[u8]) -> Option<(u64, usize)> {
        let mut value: u128 = 0;
        for (index, &byte) in bytes.iter().enumerate().take(LONGEST_ENCODED) {
            value = value * 128 + u128::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Some((value as u64, index + 1));
            }
        }
        None
    }

    #[test]
    fn encoded_integers_read_as_a_reference_reads_them_on_every_short_input() {
        let mut inputs: Vec<Vec<u8>> = Vec::new();
        for first in 0..=255u8 {
            inputs.push(vec![first]);
            for second in 0..=255u8 {
                inputs.push(vec![first, second]);
            }
        }
        for length in 1..=11 {
            for last in [0x00, 0x7F, 0x80, 0xFF] {
                let mut bytes = vec![0xFFu8; length];
                bytes[length - 1] = last;
                inputs.push(bytes.clone());
                bytes[0] = 0x80;
                inputs.push(bytes);
            }
        }
        for bytes in inputs {
            let mut at = 0;
            let ours = encoded_integer(&bytes, &mut at)
                .ok()
                .map(|value| (value, at));
            assert_eq!(ours, reference_integer(&bytes), "{bytes:02x?}");
        }
        let mut longest = vec![0xFFu8; 8];
        longest.push(0x7F);
        assert_eq!(encoded_integer(&longest, &mut 0).unwrap(), (1 << 63) - 1);
    }

    #[test]
    fn guid_strings_spell_the_lzx_transforms_as_7zip_names_them() {
        assert_eq!(
            guid_string(&CHM_LZX_GUID),
            "{7FC28940-9D31-11D0-9B27-00A0C91E9C7C}"
        );
        assert_eq!(
            guid_string(&HELP2_LZX_GUID),
            "{0A9007C6-4076-11D3-8789-0000F8105754}"
        );
    }

    fn words(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }

    #[test]
    fn name_lists_stop_each_name_at_its_first_nul_and_need_a_zero_after_it() {
        let a = u16::from(b'a');
        let b = u16::from(b'b');
        assert_eq!(
            name_list(&words(&[0, 2, 1, a, 0, 2, a, b, 0])).unwrap(),
            ["a", "ab"]
        );
        assert_eq!(name_list(&words(&[0, 1, 3, a, 0, b, 0])).unwrap(), ["a"]);
        assert!(name_list(&words(&[0, 1, 1, a, 1])).is_err());
        assert!(name_list(&words(&[0, 2, 1, a, 0])).is_err());
        assert!(name_list(&words(&[0, 1, 1, 0xD800, 0])).is_err());
        assert_eq!(name_list(&words(&[7, 0])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn control_data_keeps_exact_powers_of_two_up_to_2_16() {
        let control = |interval: u32, window: u32, version: u32| {
            let mut bytes = [0u8; LZX_CONTROL];
            let values = [
                6,
                u32::from_le_bytes(*b"LZXC"),
                version,
                interval,
                window,
                1,
            ];
            for (index, value) in values.into_iter().enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            lzx_control(&bytes)
        };
        let parsed = control(2, 2, 2).unwrap();
        assert_eq!((parsed.interval_bits, parsed.window_bits), (1, 16));
        assert_eq!(control(1 << 16, 64, 3).unwrap().interval_bits, 16);
        for (interval, window, version) in
            [(3, 2, 2), (2, 0, 2), (1 << 17, 2, 2), (2, 2, 1), (2, 2, 4)]
        {
            assert!(control(interval, window, version).is_err());
        }
    }

    #[test]
    fn the_block_count_follows_7zip_at_every_boundary() {
        let header = |uncompressed: u64, entries: u32| {
            let mut bytes = [0u8; RESET_TABLE_HEADER];
            for (at, value) in [(0, 2u32), (4, entries), (8, 8), (12, 40)] {
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes[16..24].copy_from_slice(&uncompressed.to_le_bytes());
            bytes[32..40].copy_from_slice(&BLOCK.to_le_bytes());
            reset_table_header(&bytes, 40 + 8 * u64::from(entries))
        };
        for (uncompressed, blocks) in [
            (0, 1),
            (1, 1),
            (32_766, 1),
            (32_767, 2),
            (32_768, 2),
            (65_534, 2),
            (65_535, 3),
        ] {
            assert!(header(uncompressed, blocks).is_ok(), "{uncompressed}");
            assert!(header(uncompressed, blocks + 1).unwrap().extra_entry);
            assert!(header(uncompressed, blocks + 2).is_err());
            assert!(header(uncompressed, blocks - 1).is_err());
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn chm_headers_never_panic_and_end_past_both_sections() {
        let bytes: [u8; CHM_HEADER] = kani::any();
        if let Ok(layout) = chm_header(&bytes) {
            for span in [layout.section_zero, layout.directory] {
                assert!(u128::from(layout.end) >= u128::from(span.offset) + u128::from(span.size));
            }
            assert!(layout.end >= CHM_HEADER as u64);
            assert!(layout.content == u64_at(&bytes, 0x58));
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn help2_headers_never_panic_and_end_past_all_five_sections() {
        let bytes: [u8; HELP2_SHORT_HEADER] = kani::any();
        if let Ok(header) = help2_header(&bytes) {
            for section in 0..5 {
                let span = span_at(&bytes, 0x28 + 16 * section);
                assert!(u128::from(header.end) >= u128::from(span.offset) + u128::from(span.size));
            }
            assert!(header.caol == CAOL_WITH_ITSF || header.caol == CAOL_WITHOUT_ITSF);
        }
        let itsf: [u8; HELP2_HEADER - HELP2_SHORT_HEADER] = kani::any();
        if let Ok(content) = help2_itsf(&itsf) {
            assert!(content == u64_at(&itsf, 20));
        }
    }

    #[kani::proof]
    fn directory_headers_keep_chunk_sizes_inside_their_bounds() {
        let itsp: [u8; ITSP_HEADER] = kani::any();
        if let Ok(listing) = itsp_header(&itsp) {
            assert!((SMALLEST_CHM_CHUNK..=LARGEST_CHUNK).contains(&listing.chunk_size));
        }
        let ifcm: [u8; IFCM_HEADER] = kani::any();
        if let Ok(listing) = ifcm_header(&ifcm) {
            assert!((SMALLEST_HELP2_CHUNK..=LARGEST_CHUNK).contains(&listing.chunk_size));
        }
        let zero: [u8; SECTION_ZERO] = kani::any();
        if let Ok(size) = section_zero(&zero) {
            assert!(size == u64_at(&zero, 8));
        }
    }

    #[kani::proof]
    #[kani::unwind(11)]
    fn encoded_integers_read_seven_bits_a_byte_and_at_most_nine_bytes() {
        let bytes: [u8; 10] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= 10);
        let start: usize = kani::any();
        kani::assume(start <= length);
        let mut at = start;
        let result = encoded_integer(&bytes[..length], &mut at);
        assert!(at <= length);
        if let Ok(value) = result {
            let used = at - start;
            assert!((1..=LONGEST_ENCODED).contains(&used));
            assert!(u128::from(value) < 1u128 << (7 * used));
            assert!(bytes[at - 1] < 0x80);
            assert!(bytes[start..at - 1].iter().all(|&byte| byte >= 0x80));
        }
    }

    #[kani::proof]
    #[kani::unwind(11)]
    fn chm_listings_never_panic_and_keep_every_name_inside_the_listing() {
        let chunk: [u8; SMALLEST_CHM_CHUNK as usize] = kani::any();
        let limit = chunk.len().saturating_sub(u32_at(&chunk, 4) as usize);
        let mut seen = 0u64;
        let result = walk_listing(&chunk, Form::Chm, |entry| {
            assert!(entry.name.start > PMGL_HEADER);
            assert!(entry.name.start < entry.name.end && entry.name.end <= limit);
            seen += 1;
            Ok(())
        });
        if let Ok(count) = result {
            assert!(count == seen);
            assert!(PMGL_HEADER <= limit);
            let stored = u64::from(u16_at(&chunk, chunk.len() - QUICKREF_COUNT));
            assert!(stored == count || stored == 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(11)]
    fn help2_listings_never_panic_and_count_exactly_their_entries() {
        let chunk: [u8; SMALLEST_HELP2_CHUNK as usize] = kani::any();
        let limit = chunk.len().saturating_sub(u32_at(&chunk, 4) as usize);
        let mut seen = 0u64;
        let result = walk_listing(&chunk, Form::Help2, |entry| {
            assert!(entry.name.start > AOLL_HEADER);
            assert!(entry.name.end <= limit);
            seen += 1;
            Ok(())
        });
        if let Ok(count) = result {
            assert!(count == seen);
            assert!(AOLL_HEADER <= limit);
            assert!(u64::from(u16_at(&chunk, chunk.len() - QUICKREF_COUNT)) == count);
        }
    }

    #[kani::proof]
    fn reset_table_headers_fit_their_size_and_7zips_block_count() {
        let bytes: [u8; RESET_TABLE_HEADER] = kani::any();
        let size: u64 = kani::any();
        if let Ok(table) = reset_table_header(&bytes, size) {
            assert!(RESET_TABLE_HEADER as u64 + RESET_ENTRY * table.entries == size);
            let blocks =
                (u128::from(table.uncompressed) + u128::from(BLOCK) + 1) / u128::from(BLOCK);
            assert!(u128::from(table.entries) == blocks + u128::from(table.extra_entry));
            assert!(table.entries <= u64::from(u32::MAX));
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn offset_checks_accept_exactly_tables_from_zero_that_never_go_back() {
        let offsets: [u64; 3] = kani::any();
        let count: usize = kani::any();
        kani::assume(count <= 3);
        let table = ResetTable {
            entries: count as u64,
            uncompressed: kani::any(),
            compressed: kani::any(),
            extra_entry: kani::any(),
        };
        let mut check = OffsetCheck::default();
        let mut accepted = true;
        for &offset in &offsets[..count] {
            accepted &= check.take(offset).is_ok();
        }
        accepted &= check.finish(&table).is_ok();
        let ordered = offsets[..count].windows(2).all(|pair| pair[0] <= pair[1]);
        let from_zero = count == 0 || offsets[0] == 0;
        let ends = !table.extra_entry || count > 0 && offsets[count - 1] == table.compressed;
        assert!(accepted == (ordered && from_zero && ends));
    }

    #[kani::proof]
    fn block_inputs_are_never_empty_and_never_past_1_mib() {
        let table = ResetTable {
            entries: kani::any(),
            uncompressed: kani::any(),
            compressed: kani::any(),
            extra_entry: kani::any(),
        };
        let index: u64 = kani::any();
        let start: u64 = kani::any();
        let next: Option<u64> = kani::any();
        if let Ok(range) = block_input(&table, index, start, next) {
            assert!(index < table.entries);
            assert!(range.start == start && range.start < range.end);
            assert!(range.end - range.start <= LARGEST_PACKED_BLOCK);
            assert!(index + 1 < table.entries || range.end == table.compressed);
        }
    }

    #[kani::proof]
    fn lzx_control_keeps_powers_of_two_and_a_window_from_15_bits() {
        let bytes: [u8; LZX_CONTROL] = kani::any();
        if let Ok(control) = lzx_control(&bytes) {
            assert!(control.dwords >= LZX_CONTROL_DWORDS);
            assert!(control.interval_bits <= LARGEST_LOG);
            assert!(1u32 << control.interval_bits == u32_at(&bytes, 12));
            assert!(control.window_bits >= SMALLEST_DICTIONARY_BITS);
            assert!(1u32 << (control.window_bits - SMALLEST_DICTIONARY_BITS) == u32_at(&bytes, 16));
        }
    }
}
