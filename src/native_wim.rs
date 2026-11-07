//! In-process extraction of Windows Imaging files: WIM with XPRESS, LZX and
//! LZMS resources, and ESD images with LZMS solid resources. See
//! `docs/reference/wim-reader.md` for what it accepts and restores.
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check, lzms, lzx, xpress};
use anyhow::{Context, Result, bail, ensure};
use sha1::Digest;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    ops::Range,
    path::Path,
    time::SystemTime,
};

const SIGNATURE: &[u8; 8] = b"MSWIM\0\0\0";
const HEADER: usize = 0xD0;
const VERSION: u32 = 0x10D00;
const SOLID_VERSION: u32 = 0xE00;
const COMPRESSED: u32 = 0x2;
const METHOD_MASK: u32 = 0xFFFE_0000;
const XPRESS_FLAG: u32 = 1 << 17;
const LZX_FLAG: u32 = 1 << 18;
const LZMS_FLAG: u32 = 1 << 19;
const XPRESS_OTHER_CHUNK_FLAG: u32 = 1 << 21;
const DEFAULT_CHUNK_BITS: u32 = 15;
const SMALLEST_CHUNK_BITS: u32 = 12;
const RESOURCE: usize = 24;
const PACKED_BITS: u32 = 56;
const RESOURCE_METADATA: u8 = 0x02;
const RESOURCE_COMPRESSED: u8 = 0x04;
const RESOURCE_SOLID: u8 = 0x10;
const SOLID_RESOURCE_SIZE: u64 = 1 << 32;
const SOLID_HEADER: usize = 16;
const BLOB: usize = 50;
const HASH: usize = 20;
const LARGEST_SIZE: u64 = 1 << 63;
const SMALL_CHUNK_ENTRIES: u64 = 1 << 32;
const MAX_SOLID_CHUNKS: u64 = 1 << 20;
const MAX_BLOB_TABLE: u64 = 32 * 1024 * 1024;
const BLOB_BATCH: usize = 1024;
const MAX_XML: u64 = 64 * 1024 * 1024;
const MAX_METADATA: u64 = 256 * 1024 * 1024;
const MAX_REPARSE: u64 = 64 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;
const METADATA_PIECE: usize = 64 * 1024;
const METADATA_WINDOW: usize = 1024 * 1024;
const LONGEST_DENTRY_READ: usize = DENTRY + 2 * (u16::MAX as usize + 2);
const LONGEST_STREAM_READ: usize =
    (STREAM_ENTRY + u16::MAX as usize + 2).next_multiple_of(DENTRY_ALIGN);
const LZMS_MAX_CHUNK_BITS: u32 = 26;
const DENTRY: usize = 102;
const DENTRY_ALIGN: usize = 8;
const STREAM_ENTRY: usize = 38;
const TERMINATOR: usize = 8;
const DIRECTORY: u32 = 0x10;
const READ_ONLY: u32 = 0x01;
const REPARSE_POINT: u32 = 0x400;
const SYMLINK_TAG: u32 = 0xA000_000C;
const JUNCTION_TAG: u32 = 0xA000_0003;
const WSL_SYMLINK_TAG: u32 = 0xA000_001D;
const WSL_SYMLINK_VERSION: u32 = 2;
const SYMLINK_IS_RELATIVE: u32 = 1;
const TABLE_BLOCK: u64 = 4096;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;
const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
const TICKS_PER_SECOND: u64 = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Xpress,
    Lzx,
    Lzms,
}

impl Method {
    fn from_solid(code: u32) -> Field<Method> {
        match code {
            1 => Ok(Method::Xpress),
            2 => Ok(Method::Lzx),
            3 => Ok(Method::Lzms),
            _ => Err(BadField(
                "WIM solid resource uses an unknown compression method",
            )),
        }
    }

    fn chunk_bits(self) -> Range<u32> {
        match self {
            Method::Xpress => xpress::MIN_CHUNK_BITS..xpress::MAX_CHUNK_BITS + 1,
            Method::Lzx => u32::from(lzx::MIN_WINDOW_BITS)..u32::from(lzx::MAX_WINDOW_BITS) + 1,
            Method::Lzms => 15..LZMS_MAX_CHUNK_BITS + 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Resource {
    packed: u64,
    flags: u8,
    offset: u64,
    size: u64,
}

impl Resource {
    fn end(&self) -> u64 {
        self.offset.saturating_add(self.packed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    version: u32,
    flags: u32,
    method: Option<Method>,
    chunk_bits: u32,
    part: u16,
    parts: u16,
    images: u32,
    blob_table: Resource,
    xml: Resource,
    boot: Resource,
    boot_index: u32,
    integrity: Resource,
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

fn resource(bytes: &[u8; RESOURCE]) -> Resource {
    Resource {
        packed: u64_at(bytes, 0) & ((1 << PACKED_BITS) - 1),
        flags: bytes[7],
        offset: u64_at(bytes, 8),
        size: u64_at(bytes, 16),
    }
}

fn resource_at(bytes: &[u8], at: usize) -> Resource {
    resource(bytes[at..at + RESOURCE].try_into().unwrap())
}

// EXTERNAL(7-Zip WimIn): a chunk size field of 0 means 32 KiB, and any other value must be
// a power of two of at least 4 KiB.
fn chunk_bits(chunk_size: u32) -> Field<u32> {
    if chunk_size == 0 {
        return Ok(DEFAULT_CHUNK_BITS);
    }
    field_check(
        chunk_size.is_power_of_two() && chunk_size >= 1 << SMALLEST_CHUNK_BITS,
        "WIM chunk size is not a power of two of at least 4 KiB",
    )?;
    Ok(chunk_size.trailing_zeros())
}

fn header(bytes: &[u8; HEADER]) -> Field<Header> {
    field_check(&bytes[..8] == SIGNATURE, "not a WIM file")?;
    field_check(
        u32_at(bytes, 8) == HEADER as u32,
        "WIM header is not 208 bytes, as WIM 1.13 and ESD write it; pass --zip-engine backend to read it with 7-Zip",
    )?;
    let version = u32_at(bytes, 12);
    field_check(
        version == VERSION || version == SOLID_VERSION,
        "WIM version is neither 1.13 nor the ESD version 0.14; pass --zip-engine backend to read it with 7-Zip",
    )?;
    let flags = u32_at(bytes, 16);
    let method = if flags & COMPRESSED == 0 {
        None
    } else {
        Some(match flags & METHOD_MASK {
            XPRESS_FLAG | XPRESS_OTHER_CHUNK_FLAG => Method::Xpress,
            LZX_FLAG => Method::Lzx,
            LZMS_FLAG => Method::Lzms,
            _ => return Err(BadField("WIM header names an unknown compression method")),
        })
    };
    let part = u16_at(bytes, 40);
    let parts = u16_at(bytes, 42);
    field_check(
        part != 0 && part <= parts,
        "WIM header gives a part number outside its part count",
    )?;
    Ok(Header {
        version,
        flags,
        method,
        chunk_bits: chunk_bits(u32_at(bytes, 20))?,
        part,
        parts,
        images: u32_at(bytes, 44),
        blob_table: resource_at(bytes, 48),
        xml: resource_at(bytes, 72),
        boot: resource_at(bytes, 96),
        boot_index: u32_at(bytes, 120),
        integrity: resource_at(bytes, 124),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlobEntry {
    resource: Resource,
    part: u16,
    references: u32,
    hash: [u8; HASH],
}

fn blob_entry(bytes: &[u8; BLOB]) -> BlobEntry {
    BlobEntry {
        resource: resource_at(bytes, 0),
        part: u16_at(bytes, 24),
        references: u32_at(bytes, 26),
        hash: bytes[30..].try_into().unwrap(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkTable {
    chunks: u64,
    entry_bytes: u64,
    table_bytes: u64,
}

// EXTERNAL(7-Zip WimIn): a compressed resource starts with one offset per chunk after the
// first, 8 bytes each when the resource is 4 GiB or more and 4 bytes otherwise.
fn chunk_table(size: u64, packed: u64, chunk_bits: u32) -> Field<ChunkTable> {
    field_check(
        size > 0 && size < LARGEST_SIZE,
        "WIM compressed resource is empty or larger than 2^63 bytes",
    )?;
    field_check(
        (SMALLEST_CHUNK_BITS..32).contains(&chunk_bits),
        "WIM chunk size is not 4 KiB to 2 GiB",
    )?;
    let chunks = ((size - 1) >> chunk_bits) + 1;
    let entry_bytes = if size < SMALL_CHUNK_ENTRIES { 4 } else { 8 };
    let table_bytes = (chunks - 1) * entry_bytes;
    field_check(
        table_bytes <= packed,
        "WIM resource is smaller than its chunk table",
    )?;
    Ok(ChunkTable {
        chunks,
        entry_bytes,
        table_bytes,
    })
}

fn chunk_size(size: u64, chunk_bits: u32, index: u64) -> u64 {
    (size - (index << chunk_bits)).min(1 << chunk_bits)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SolidHeader {
    size: u64,
    chunk_bits: u32,
    method: Method,
    chunks: u64,
}

// EXTERNAL(7-Zip WimIn): a solid resource starts with its size, chunk size and method,
// then a 4-byte packed size for every chunk.
fn solid_header(bytes: &[u8; SOLID_HEADER], packed: u64) -> Field<SolidHeader> {
    let size = u64_at(bytes, 0);
    field_check(
        size < LARGEST_SIZE,
        "WIM solid resource is larger than 2^63 bytes",
    )?;
    let chunk = u32_at(bytes, 8);
    field_check(
        chunk.is_power_of_two() && chunk >= 1 << SMALLEST_CHUNK_BITS,
        "WIM solid resource chunk size is not a power of two of at least 4 KiB",
    )?;
    let chunk_bits = chunk.trailing_zeros();
    let method = Method::from_solid(u32_at(bytes, 12))?;
    let chunks = size.div_ceil(1 << chunk_bits);
    field_check(
        chunks <= MAX_SOLID_CHUNKS,
        "WIM solid resource has more than 2^20 chunks; pass --zip-engine backend to read it with 7-Zip",
    )?;
    field_check(
        SOLID_HEADER as u64 + 4 * chunks <= packed,
        "WIM solid resource is smaller than its chunk table",
    )?;
    Ok(SolidHeader {
        size,
        chunk_bits,
        method,
        chunks,
    })
}

// EXTERNAL(7-Zip WimIn): security data is a total length, a count and one 8-byte size per
// descriptor; entries start at the total rounded up to 8, or at 8 when the total is 0.
fn security_end(prefix: &[u8], meta_len: usize) -> Field<usize> {
    field_check(
        prefix.len() >= 8 && meta_len >= prefix.len(),
        "WIM metadata is shorter than 8 bytes",
    )?;
    let total = u32_at(prefix, 0) as usize;
    if total == 0 {
        return Ok(8);
    }
    let count = u32_at(prefix, 4) as usize;
    field_check(
        total >= 8 && total <= meta_len && count <= (total - 8) / 8,
        "WIM security data lies outside its metadata",
    )?;
    let mut end = 8 + count * 8;
    field_check(
        prefix.len() >= end,
        "WIM security sizes lie outside the bytes read",
    )?;
    for index in 0..count {
        let size = u64_at(prefix, 8 + index * 8);
        field_check(
            size <= (total - end) as u64,
            "WIM security descriptor runs past its security data",
        )?;
        end += size as usize;
    }
    let aligned = total.next_multiple_of(DENTRY_ALIGN);
    field_check(
        end.next_multiple_of(DENTRY_ALIGN) == aligned && aligned <= meta_len,
        "WIM security descriptors do not fill their security data",
    )?;
    Ok(aligned)
}

fn security_sizes_length(head: &[u8; 8], meta_len: usize) -> usize {
    (8 + 8 * u32_at(head, 4) as usize).min(meta_len)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DentryFields {
    length: usize,
    attributes: u32,
    subdir: u64,
    modified: u64,
    hash: [u8; HASH],
    tag_or_group: u64,
    streams: u16,
    name: Range<usize>,
}

fn name_is_terminated(record: &[u8], name: &Range<usize>) -> bool {
    let units_nonzero = name
        .clone()
        .step_by(2)
        .all(|at| record[at] != 0 || record[at + 1] != 0);
    units_nonzero && record[name.end] == 0 && record[name.end + 1] == 0
}

fn record_fits(record: &[u8], at: usize, meta_len: usize, longest: usize) -> bool {
    at <= meta_len && record.len() == (meta_len - at).min(longest)
}

// EXTERNAL(7-Zip WimIn): a directory entry is at least 102 bytes, aligned to 8, and holds
// its name and short name in UTF-16, each followed by a NUL when present.
fn dentry(record: &[u8], at: usize, meta_len: usize) -> Field<Option<DentryFields>> {
    field_check(
        at.is_multiple_of(DENTRY_ALIGN)
            && record_fits(record, at, meta_len, LONGEST_DENTRY_READ)
            && record.len() >= TERMINATOR,
        "WIM directory entry lies outside its metadata",
    )?;
    let length = u64_at(record, 0);
    if length == 0 {
        return Ok(None);
    }
    field_check(
        length <= (meta_len - at) as u64
            && length.is_multiple_of(DENTRY_ALIGN as u64)
            && length >= DENTRY as u64,
        "WIM directory entry has an impossible length",
    )?;
    let length = length as usize;
    let subdir = u64_at(record, 16);
    field_check(
        subdir <= meta_len as u64,
        "WIM directory entry names a folder list outside its metadata",
    )?;
    let short_bytes = usize::from(u16_at(record, 98));
    let name_bytes = usize::from(u16_at(record, 100));
    field_check(
        short_bytes.is_multiple_of(2) && name_bytes.is_multiple_of(2),
        "WIM directory entry has a name of an odd number of bytes",
    )?;
    field_check(
        name_bytes != 0 || short_bytes == 0,
        "WIM directory entry has a short name and no name",
    )?;
    let with_nul = |bytes: usize| if bytes == 0 { 0 } else { bytes + 2 };
    field_check(
        DENTRY + with_nul(name_bytes) + with_nul(short_bytes) <= length,
        "WIM directory entry is shorter than its names",
    )?;
    let name = DENTRY..DENTRY + name_bytes;
    field_check(
        name_is_terminated(record, &name),
        "WIM directory entry name holds a NUL or does not end with one",
    )?;
    Ok(Some(DentryFields {
        length,
        attributes: u32_at(record, 8),
        subdir,
        modified: u64_at(record, 0x38),
        hash: record[0x40..0x40 + HASH].try_into().unwrap(),
        tag_or_group: u64_at(record, 0x58),
        streams: u16_at(record, 0x60),
        name,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StreamFields {
    length: usize,
    hash: [u8; HASH],
    name: Range<usize>,
}

// EXTERNAL(7-Zip WimIn): an extra stream entry is at least 38 bytes, aligned to 8, with
// its hash and its UTF-16 name followed by a NUL.
fn stream_entry(record: &[u8], at: usize, meta_len: usize) -> Field<StreamFields> {
    field_check(
        record_fits(record, at, meta_len, LONGEST_STREAM_READ) && record.len() >= TERMINATOR,
        "WIM stream entry lies outside its metadata",
    )?;
    let length = u64_at(record, 0);
    field_check(
        length <= (meta_len - at) as u64
            && length.is_multiple_of(DENTRY_ALIGN as u64)
            && length >= STREAM_ENTRY as u64,
        "WIM stream entry has an impossible length",
    )?;
    let length = length as usize;
    let name_bytes = usize::from(u16_at(record, 36));
    field_check(
        name_bytes.is_multiple_of(2)
            && (STREAM_ENTRY + name_bytes + 2).next_multiple_of(DENTRY_ALIGN) <= length,
        "WIM stream entry is shorter than its name",
    )?;
    let name = STREAM_ENTRY..STREAM_ENTRY + name_bytes;
    field_check(
        name_is_terminated(record, &name),
        "WIM stream entry name holds a NUL or does not end with one",
    )?;
    Ok(StreamFields {
        length,
        hash: record[16..16 + HASH].try_into().unwrap(),
        name,
    })
}

fn modified(ticks: u64) -> Option<SystemTime> {
    if ticks == 0 {
        return None;
    }
    let seconds = ticks / TICKS_PER_SECOND;
    let nanos = (ticks % TICKS_PER_SECOND * 100) as u32;
    let since = |seconds: u64| std::time::Duration::new(seconds, nanos);
    if seconds >= WINDOWS_TO_UNIX_SECONDS {
        std::time::UNIX_EPOCH.checked_add(since(seconds - WINDOWS_TO_UNIX_SECONDS))
    } else {
        std::time::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(
                WINDOWS_TO_UNIX_SECONDS - seconds,
            ))?
            .checked_add(std::time::Duration::new(0, nanos))
    }
}

fn utf16(bytes: &[u8]) -> Option<String> {
    char::decode_utf16(
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
    )
    .collect::<Result<String, _>>()
    .ok()
}

/// Returns whether the file starts with the WIM signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(SIGNATURE.len());
    crate::volumes::open(path)?
        .take(SIGNATURE.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == SIGNATURE)
}

/// Returns whether the default engine sends this WIM to 7-Zip: a part of a
/// split WIM, or a header from a WIM version before 1.13.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut bytes = [0u8; HEADER];
    if read_exact_at(&file, &mut bytes, 0).is_err() {
        return Ok(false);
    }
    Ok(match header(&bytes) {
        Ok(header) => header.parts != 1,
        Err(_) => {
            let version = u32_at(&bytes, 12);
            u32_at(&bytes, 8) != HEADER as u32 || version < VERSION && version != SOLID_VERSION
        }
    })
}

fn read_exact_at(
    file: &ArchiveFile,
    mut buffer: &mut [u8],
    mut offset: u64,
) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Place {
    Stored {
        offset: u64,
    },
    Chunked {
        offset: u64,
        packed: u64,
        method: Method,
        chunk_bits: u32,
    },
    Solid {
        solid: usize,
        offset: u64,
    },
}

#[derive(Clone, Copy, Debug)]
struct Blob {
    place: Place,
    size: u64,
    hash: [u8; HASH],
}

struct Solid {
    offset: u64,
    header: SolidHeader,
    chunk_starts: Vec<u64>,
}

struct Archive {
    file: ArchiveFile,
    header: Header,
    blobs: Vec<Blob>,
    solids: Vec<Solid>,
    images: Vec<Blob>,
    xml: Vec<u8>,
}

fn place_of(header: &Header, resource: &Resource) -> Result<Place> {
    if resource.flags & RESOURCE_COMPRESSED == 0 {
        ensure!(
            resource.packed == resource.size,
            "WIM resource at offset {} is stored with a packed size unlike its size",
            resource.offset
        );
        return Ok(Place::Stored {
            offset: resource.offset,
        });
    }
    let method = header
        .method
        .context("WIM resource is compressed, and the header names no compression method")?;
    Ok(Place::Chunked {
        offset: resource.offset,
        packed: resource.packed,
        method,
        chunk_bits: header.chunk_bits,
    })
}

fn ensure_inside(length: u64, resource: &Resource, what: &str) -> Result<()> {
    ensure!(
        resource
            .offset
            .checked_add(resource.packed)
            .is_some_and(|end| end <= length),
        "WIM {what} lies past the end of the file"
    );
    ensure!(
        resource.packed == 0 || resource.offset >= HEADER as u64,
        "WIM {what} overlaps the header"
    );
    Ok(())
}

fn ensure_no_overlap(mut spans: Vec<Range<u64>>) -> Result<()> {
    spans.retain(|span| !span.is_empty());
    spans.sort_unstable_by_key(|span| (span.start, span.end));
    ensure!(
        spans.windows(2).all(|pair| pair[0].end <= pair[1].start),
        "WIM resources share bytes of the file"
    );
    Ok(())
}

impl Archive {
    fn open(path: &Path) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut bytes = [0u8; HEADER];
        read_exact_at(&file, &mut bytes, 0).context("truncated WIM header")?;
        let header = header(&bytes)?;
        ensure!(
            header.parts == 1,
            "WIM file is part {} of a split WIM of {} parts, which Zipper does not join; {BACKEND_HINT}",
            header.part,
            header.parts
        );
        let mut archive = Archive {
            file,
            header,
            blobs: Vec::new(),
            solids: Vec::new(),
            images: Vec::new(),
            xml: Vec::new(),
        };
        let entries = (header.blob_table.size / BLOB as u64).min(MAX_BLOB_TABLE / BLOB as u64);
        let mut spans = Vec::with_capacity(entries as usize + 4);
        spans.push(0..HEADER as u64);
        for (resource, what) in [
            (header.blob_table, "blob table"),
            (header.xml, "XML data"),
            (header.integrity, "integrity table"),
        ] {
            ensure_inside(length, &resource, what)?;
            spans.push(resource.offset..resource.end());
        }
        archive.read_blob_table(length, &mut spans)?;
        ensure_no_overlap(spans)?;
        archive.xml = archive.read_header_resource(&header.xml, MAX_XML, "XML data")?;
        ensure!(
            !xml_says_encrypted(&archive.xml),
            "WIM file is an encrypted ESD; {BACKEND_HINT}"
        );
        Ok(archive)
    }

    fn read_header_resource(&self, resource: &Resource, limit: u64, what: &str) -> Result<Vec<u8>> {
        ensure!(
            resource.flags & RESOURCE_SOLID == 0,
            "WIM {what} is in a solid resource"
        );
        ensure!(
            resource.size <= limit,
            "WIM {what} is larger than {limit} bytes"
        );
        let blob = Blob {
            place: place_of(&self.header, resource)?,
            size: resource.size,
            hash: [0; HASH],
        };
        let mut decoders = Decoders::default();
        let mut bytes = Vec::new();
        BlobReader::new(self, &mut decoders, &blob, false)?
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read the WIM {what}"))?;
        Ok(bytes)
    }

    fn read_blob_table(&mut self, length: u64, spans: &mut Vec<Range<u64>>) -> Result<()> {
        let table = self.header.blob_table;
        ensure!(
            table.flags & RESOURCE_SOLID == 0,
            "WIM blob table is in a solid resource"
        );
        ensure!(
            table.size <= MAX_BLOB_TABLE,
            "WIM blob table is larger than {MAX_BLOB_TABLE} bytes"
        );
        ensure!(
            table.size.is_multiple_of(BLOB as u64),
            "WIM blob table is not a whole number of 50-byte entries"
        );
        let source = Blob {
            place: place_of(&self.header, &table)?,
            size: table.size,
            hash: [0; HASH],
        };
        let mut decoders = Decoders::default();
        let mut batch = vec![0u8; BLOB * BLOB_BATCH];
        let mut group_start = 0usize;
        let mut group_size = 0u64;
        let mut in_group = false;
        let mut pending_small = Vec::new();
        for index in 0..(table.size / BLOB as u64) as usize {
            let within = index % BLOB_BATCH;
            if within == 0 {
                let first = (index * BLOB) as u64;
                let count = ((table.size - first) as usize).min(batch.len());
                let mut reader = BlobReader {
                    archive: self,
                    decoders: &mut decoders,
                    blob: &source,
                    at: first,
                    hasher: None,
                };
                reader
                    .read_exact(&mut batch[..count])
                    .context("cannot read the WIM blob table")?;
            }
            let bytes = &batch[within * BLOB..(within + 1) * BLOB];
            let entry = blob_entry(bytes.try_into().unwrap());
            let resource = entry.resource;
            ensure!(
                entry.part == 1,
                "WIM blob {index} lies in part {} of a split WIM; {BACKEND_HINT}",
                entry.part
            );
            if resource.flags & RESOURCE_SOLID == 0 {
                self.finish_group(&mut pending_small, group_start)?;
                in_group = false;
                ensure_inside(length, &resource, "resource")?;
                spans.push(resource.offset..resource.end());
                let blob = Blob {
                    place: place_of(&self.header, &resource)?,
                    size: resource.size,
                    hash: entry.hash,
                };
                if resource.flags & RESOURCE_METADATA != 0 {
                    ensure!(
                        entry.references <= 1,
                        "WIM image metadata is referenced {} times",
                        entry.references
                    );
                    if entry.references == 1 {
                        self.images.push(blob);
                    }
                } else {
                    self.add_blob(blob)?;
                }
                continue;
            }
            ensure!(
                resource.flags == RESOURCE_SOLID,
                "WIM solid resource {index} has flags other than the solid flag"
            );
            if !in_group {
                in_group = true;
                group_start = self.solids.len();
                group_size = 0;
            }
            if resource.size == SOLID_RESOURCE_SIZE {
                ensure_inside(length, &resource, "solid resource")?;
                spans.push(resource.offset..resource.end());
                let solid = self.read_solid(&resource)?;
                group_size = group_size
                    .checked_add(solid.header.size)
                    .context("WIM solid resources overflow")?;
                self.solids.push(solid);
            } else {
                ensure!(
                    resource.size == 0,
                    "WIM solid blob {index} has a size field that is neither 0 nor 2^32"
                );
                pending_small.push(entry);
            }
        }
        self.finish_group(&mut pending_small, group_start)?;
        self.blobs.sort_unstable_by_key(|blob| blob.hash);
        ensure!(
            self.blobs
                .windows(2)
                .all(|pair| pair[0].hash != pair[1].hash),
            "WIM blob table lists one hash twice"
        );
        Ok(())
    }

    fn read_solid(&self, resource: &Resource) -> Result<Solid> {
        let mut bytes = [0u8; SOLID_HEADER];
        read_exact_at(&self.file, &mut bytes, resource.offset)
            .context("truncated WIM solid resource header")?;
        let header = solid_header(&bytes, resource.packed)?;
        let mut sizes = vec![0u8; 4 * header.chunks as usize];
        read_exact_at(
            &self.file,
            &mut sizes,
            resource.offset + SOLID_HEADER as u64,
        )
        .context("truncated WIM solid resource chunk table")?;
        let mut chunk_starts = Vec::with_capacity(header.chunks as usize + 1);
        let mut at = SOLID_HEADER as u64 + sizes.len() as u64;
        chunk_starts.push(at);
        for size in sizes.chunks_exact(4) {
            at += u64::from(u32_at(size, 0));
            chunk_starts.push(at);
        }
        ensure!(
            at == resource.packed,
            "WIM solid resource chunks do not fill its packed size"
        );
        Ok(Solid {
            offset: resource.offset,
            header,
            chunk_starts,
        })
    }

    // EXTERNAL(7-Zip WimIn): a solid blob's offset counts from the start of the run of solid
    // resources in the blob table, and the blob lies inside one of them.
    fn finish_group(&mut self, pending: &mut Vec<BlobEntry>, group_start: usize) -> Result<()> {
        let mut starts = Vec::with_capacity(self.solids.len() - group_start);
        let mut start = 0u64;
        for solid in &self.solids[group_start..] {
            starts.push(start);
            start += solid.header.size;
        }
        for entry in pending.drain(..) {
            let offset = entry.resource.offset;
            let size = entry.resource.packed;
            let found = starts
                .partition_point(|&start| start <= offset)
                .checked_sub(1)
                .filter(|&local| {
                    offset - starts[local] < self.solids[group_start + local].header.size
                })
                .context("WIM solid blob lies outside every solid resource")?;
            let within = offset - starts[found];
            ensure!(
                size <= self.solids[group_start + found].header.size - within,
                "WIM solid blob runs past the end of its solid resource"
            );
            self.add_blob(Blob {
                place: Place::Solid {
                    solid: group_start + found,
                    offset: within,
                },
                size,
                hash: entry.hash,
            })?;
        }
        Ok(())
    }

    fn add_blob(&mut self, blob: Blob) -> Result<()> {
        ensure!(
            blob.hash != [0; HASH],
            "WIM blob table has an entry with no hash"
        );
        self.blobs.push(blob);
        Ok(())
    }

    fn blob_index(&self, hash: &[u8; HASH]) -> Option<u32> {
        self.blobs
            .binary_search_by(|blob| blob.hash.cmp(hash))
            .ok()
            .map(|index| index as u32)
    }

    fn ensure_boot_metadata(&self) -> Result<()> {
        let boot = self.header.boot;
        if boot.size == 0 {
            return Ok(());
        }
        let found = self.images.iter().position(|image| match image.place {
            Place::Stored { offset } | Place::Chunked { offset, .. } => offset == boot.offset,
            Place::Solid { .. } => false,
        });
        let index = found.context("WIM boot metadata is not the metadata of any image")?;
        ensure!(
            self.header.boot_index as usize == index + 1 && self.images[index].size == boot.size,
            "WIM boot metadata disagrees with the boot image it names"
        );
        Ok(())
    }
}

fn xml_says_encrypted(xml: &[u8]) -> bool {
    let text = String::from_utf16_lossy(
        &xml.chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    );
    text.contains("<ESD>") && text.contains("<ENCRYPTED")
}

#[derive(Default)]
struct Decoders {
    huffman: Option<Box<xpress::Huffman>>,
    lzx: Option<(u32, Box<lzx::Lzx>)>,
    lzms: Option<Box<lzms::Lzms>>,
    packed: Vec<u8>,
    chunk: Vec<u8>,
    cached: Option<(u64, u64)>,
    table: Vec<u64>,
    table_key: Option<(u64, u64)>,
}

impl Decoders {
    fn decode(&mut self, method: Method, chunk_bits: u32, size: usize) -> Field<()> {
        let Decoders { packed, chunk, .. } = self;
        match method {
            Method::Xpress => {
                let huffman = self
                    .huffman
                    .get_or_insert_with(|| Box::new(xpress::Huffman::new()));
                xpress::decode(huffman, packed, &mut chunk[..size])
            }
            Method::Lzx => {
                if self
                    .lzx
                    .as_ref()
                    .is_none_or(|(bits, _)| *bits != chunk_bits)
                {
                    self.lzx = Some((chunk_bits, Box::new(lzx::Lzx::wim(chunk_bits as u8)?)));
                }
                let (_, lzx) = self.lzx.as_mut().expect("set above");
                let decoded = lzx.chunk(packed, size)?;
                chunk[..size].copy_from_slice(decoded);
                Ok(())
            }
            Method::Lzms => {
                let lzms = self.lzms.get_or_insert_with(|| Box::new(lzms::Lzms::new()));
                lzms.decode(packed, &mut chunk[..size])
            }
        }
    }
}

struct BlobReader<'a> {
    archive: &'a Archive,
    decoders: &'a mut Decoders,
    blob: &'a Blob,
    at: u64,
    hasher: Option<sha1::Sha1>,
}

impl<'a> BlobReader<'a> {
    fn new(
        archive: &'a Archive,
        decoders: &'a mut Decoders,
        blob: &'a Blob,
        verify: bool,
    ) -> Result<Self> {
        let mut reader = BlobReader {
            archive,
            decoders,
            blob,
            at: 0,
            hasher: verify.then(sha1::Sha1::new),
        };
        if let Place::Chunked { packed, .. } = blob.place
            && blob.size == 0
        {
            ensure!(packed == 0, "WIM resource of size 0 packs {packed} bytes");
        }
        if blob.size == 0 {
            reader.finish()?;
        }
        Ok(reader)
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(hasher) = self.hasher.take() {
            ensure!(
                hasher.finalize().as_slice() == self.blob.hash,
                "WIM data fails its SHA-1 check"
            );
        }
        Ok(())
    }

    fn table_entry(&mut self, offset: u64, table: &ChunkTable, index: u64) -> Result<u64> {
        let block = index / TABLE_BLOCK;
        if self.decoders.table_key != Some((offset, block)) {
            let first = block * TABLE_BLOCK;
            let count = (table.chunks - 1 - first).min(TABLE_BLOCK);
            let mut bytes = vec![0u8; (count * table.entry_bytes) as usize];
            read_exact_at(
                &self.archive.file,
                &mut bytes,
                offset + first * table.entry_bytes,
            )
            .context("truncated WIM chunk table")?;
            self.decoders.table = bytes
                .chunks_exact(table.entry_bytes as usize)
                .map(|entry| {
                    if entry.len() == 4 {
                        u64::from(u32_at(entry, 0))
                    } else {
                        u64_at(entry, 0)
                    }
                })
                .collect();
            self.decoders.table_key = Some((offset, block));
        }
        Ok(self.decoders.table[(index % TABLE_BLOCK) as usize])
    }

    fn chunk_span(&mut self, index: u64) -> Result<(u64, u64, Method, u32, u64)> {
        Ok(match self.blob.place {
            Place::Chunked {
                offset,
                packed,
                method,
                chunk_bits,
            } => {
                let table = chunk_table(self.blob.size, packed, chunk_bits)?;
                let start = match index {
                    0 => 0,
                    _ => self.table_entry(offset, &table, index - 1)?,
                };
                let end = match index + 1 == table.chunks {
                    true => packed - table.table_bytes,
                    false => self.table_entry(offset, &table, index)?,
                };
                ensure!(
                    start <= end && end <= packed - table.table_bytes,
                    "WIM chunk table offsets go backward or past the resource"
                );
                (
                    offset + table.table_bytes + start,
                    end - start,
                    method,
                    chunk_bits,
                    chunk_size(self.blob.size, chunk_bits, index),
                )
            }
            Place::Solid { solid, .. } => {
                let solid = &self.archive.solids[solid];
                let header = solid.header;
                let start = solid.chunk_starts[index as usize];
                let end = solid.chunk_starts[index as usize + 1];
                (
                    solid.offset + start,
                    end - start,
                    header.method,
                    header.chunk_bits,
                    chunk_size(header.size, header.chunk_bits, index),
                )
            }
            Place::Stored { .. } => unreachable!("stored data has no chunks"),
        })
    }

    // EXTERNAL(7-Zip WimIn): a chunk whose packed size equals its size is stored, and a
    // chunk is decoded only when it packs to less than the chunk size.
    fn load_chunk(&mut self, key: u64, index: u64) -> Result<()> {
        if self.decoders.cached == Some((key, index)) {
            return Ok(());
        }
        self.decoders.cached = None;
        let (at, packed, method, chunk_bits, size) = self.chunk_span(index)?;
        ensure!(
            method.chunk_bits().contains(&chunk_bits),
            "WIM {method:?} chunks of 2^{chunk_bits} bytes are outside what Zipper decodes; {BACKEND_HINT}"
        );
        let stored = packed == size;
        ensure!(
            stored || packed < 1 << chunk_bits,
            "WIM chunk packs to {packed} bytes, no less than its chunk size"
        );
        let size = size as usize;
        self.decoders.packed.resize(packed as usize, 0);
        read_exact_at(&self.archive.file, &mut self.decoders.packed, at)
            .context("truncated WIM chunk")?;
        self.decoders.chunk.resize(size, 0);
        if stored {
            let Decoders { packed, chunk, .. } = &mut *self.decoders;
            chunk.copy_from_slice(packed);
        } else {
            self.decoders
                .decode(method, chunk_bits, size)
                .map_err(|error| anyhow::anyhow!("damaged WIM {method:?} chunk: {error}"))?;
        }
        self.decoders.cached = Some((key, index));
        Ok(())
    }

    fn read_some(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let left = self.blob.size - self.at;
        let want = (buffer.len() as u64).min(left) as usize;
        let read = match self.blob.place {
            Place::Stored { offset } => {
                read_exact_at(&self.archive.file, &mut buffer[..want], offset + self.at)
                    .context("truncated WIM resource")?;
                want
            }
            Place::Chunked {
                offset, chunk_bits, ..
            } => self.read_chunked(offset, 0, chunk_bits, &mut buffer[..want])?,
            Place::Solid { solid, offset } => {
                let bits = self.archive.solids[solid].header.chunk_bits;
                let key = self.archive.solids[solid].offset;
                self.read_chunked(key, offset, bits, &mut buffer[..want])?
            }
        };
        if let Some(hasher) = &mut self.hasher {
            hasher.update(&buffer[..read]);
        }
        self.at += read as u64;
        if self.at == self.blob.size {
            self.finish()?;
        }
        Ok(read)
    }

    fn read_chunked(
        &mut self,
        key: u64,
        base: u64,
        chunk_bits: u32,
        buffer: &mut [u8],
    ) -> Result<usize> {
        let position = base + self.at;
        let index = position >> chunk_bits;
        self.load_chunk(key, index)?;
        let within = (position - (index << chunk_bits)) as usize;
        let available = &self.decoders.chunk[within..];
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        Ok(count)
    }
}

impl Read for BlobReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.blob.size || buffer.is_empty() {
            return Ok(0);
        }
        self.read_some(buffer).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                Box::<dyn std::error::Error + Send + Sync>::from(error),
            )
        })
    }
}

const NO_BLOB: u32 = u32::MAX;

struct Node {
    parent: Option<u32>,
    name: Range<u32>,
    path_length: u32,
    depth: u32,
    attributes: u32,
    blob: u32,
    modified: u64,
    link_group: u64,
}

struct Used(Vec<u64>);

impl Used {
    fn claim(&mut self, at: usize, length: usize) -> Result<()> {
        for unit in at / DENTRY_ALIGN..(at + length) / DENTRY_ALIGN {
            let (word, bit) = (unit / 64, 1u64 << (unit % 64));
            ensure!(
                self.0[word] & bit == 0,
                "WIM directory entries overlap or form a loop"
            );
            self.0[word] |= bit;
        }
        Ok(())
    }
}

struct Image {
    prefix: String,
    names: String,
    nodes: Vec<Node>,
    with_data: Vec<u32>,
}

impl Image {
    fn path_to(&self, parent: Option<u32>, name: &str) -> String {
        let mut parts = vec![name];
        let mut next = parent;
        while let Some(index) = next {
            let node = &self.nodes[index as usize];
            parts.push(&self.names[node.name.start as usize..node.name.end as usize]);
            next = node.parent;
        }
        parts.reverse();
        format!("{}{}", self.prefix, parts.join("/"))
    }

    fn path(&self, index: u32) -> String {
        let node = &self.nodes[index as usize];
        let name = &self.names[node.name.start as usize..node.name.end as usize];
        self.path_to(node.parent, name)
    }

    fn keep(&mut self, node: Node, name: &str) -> u32 {
        let start = self.names.len() as u32;
        self.names.push_str(name);
        self.nodes.push(Node {
            name: start..self.names.len() as u32,
            ..node
        });
        self.nodes.len() as u32 - 1
    }
}

struct Metadata<'a> {
    archive: &'a Archive,
    blob: Blob,
    decoders: Decoders,
    pieces: Vec<[u8; HASH]>,
    start: usize,
    bytes: Vec<u8>,
}

fn piece_hash(bytes: &[u8]) -> [u8; HASH] {
    sha1::Sha1::digest(bytes).into()
}

impl<'a> Metadata<'a> {
    fn open(archive: &'a Archive, blob: Blob) -> Result<Self> {
        ensure!(
            blob.size <= MAX_METADATA,
            "WIM image metadata is larger than {MAX_METADATA} bytes"
        );
        let mut decoders = Decoders::default();
        let mut pieces = Vec::with_capacity(blob.size.div_ceil(METADATA_PIECE as u64) as usize);
        let mut piece = vec![0u8; METADATA_PIECE];
        let mut reader = BlobReader::new(archive, &mut decoders, &blob, true)?;
        let mut left = blob.size as usize;
        while left > 0 {
            let length = left.min(METADATA_PIECE);
            reader
                .read_exact(&mut piece[..length])
                .context("cannot read the WIM image metadata")?;
            pieces.push(piece_hash(&piece[..length]));
            left -= length;
        }
        Ok(Metadata {
            archive,
            blob,
            decoders,
            pieces,
            start: 0,
            bytes: Vec::new(),
        })
    }

    fn len(&self) -> usize {
        self.blob.size as usize
    }

    fn read(&mut self, at: usize, want: usize) -> Result<&[u8]> {
        ensure!(
            at <= self.len(),
            "WIM directory entry lies outside its metadata"
        );
        let end = at.saturating_add(want).min(self.len());
        if at < self.start || end > self.start + self.bytes.len() {
            let first = at / METADATA_PIECE * METADATA_PIECE;
            let fill = end
                .max(first + METADATA_WINDOW)
                .next_multiple_of(METADATA_PIECE)
                .min(self.len());
            self.bytes.resize(fill - first, 0);
            let mut reader = BlobReader {
                archive: self.archive,
                decoders: &mut self.decoders,
                blob: &self.blob,
                at: first as u64,
                hasher: None,
            };
            reader
                .read_exact(&mut self.bytes)
                .context("cannot read the WIM image metadata")?;
            for (index, piece) in self.bytes.chunks(METADATA_PIECE).enumerate() {
                ensure!(
                    piece_hash(piece) == self.pieces[first / METADATA_PIECE + index],
                    "WIM image metadata changed while it was read"
                );
            }
            self.start = first;
        }
        Ok(&self.bytes[at - self.start..end - self.start])
    }
}

// EXTERNAL(7-Zip WimIn): an unnamed extra stream of a file or link is its data when the
// entry itself has no hash; any other extra stream is an alternate data stream.
fn read_streams(
    meta: &mut Metadata,
    at: &mut usize,
    count: u16,
    directory: bool,
    hash: &mut Option<[u8; HASH]>,
    used: &mut Used,
    path: impl Fn() -> String,
) -> Result<()> {
    let meta_len = meta.len();
    for _ in 0..count {
        let record = meta.read(*at, LONGEST_STREAM_READ)?;
        let stream = stream_entry(record, *at, meta_len)?;
        let name = utf16(&record[stream.name.clone()]).unwrap_or_default();
        used.claim(*at, stream.length)?;
        let has_data = stream.hash != [0; HASH];
        if stream.name.is_empty() && !directory && hash.is_none() {
            *hash = has_data.then_some(stream.hash);
        } else if !stream.name.is_empty() {
            bail!(
                "WIM entry {:?} has the alternate data stream {name:?}, which Zipper does not write yet; {BACKEND_HINT}",
                path()
            );
        } else {
            ensure!(
                !has_data,
                "WIM entry {:?} has a second unnamed stream with data; {BACKEND_HINT}",
                path()
            );
        }
        *at += stream.length;
    }
    Ok(())
}

fn read_dentry(meta: &mut Metadata, at: usize) -> Result<Option<(DentryFields, Option<String>)>> {
    let meta_len = meta.len();
    let record = meta.read(at, LONGEST_DENTRY_READ)?;
    Ok(match dentry(record, at, meta_len)? {
        None => None,
        Some(fields) => {
            let name = utf16(&record[fields.name.clone()]);
            Some((fields, name))
        }
    })
}

fn directory_start(meta: &mut Metadata) -> Result<usize> {
    let meta_len = meta.len();
    let head: [u8; 8] = match meta.read(0, 8)?.try_into() {
        Ok(head) => head,
        Err(_) => bail!(BadField("WIM metadata is shorter than 8 bytes")),
    };
    let sizes = security_sizes_length(&head, meta_len);
    Ok(security_end(meta.read(0, sizes)?, meta_len)?)
}

fn symlink_target(tag: u32, data: &[u8], path: &str, depth: usize) -> Result<String> {
    let unsupported = || {
        anyhow::anyhow!(
            "WIM entry {path:?} is a reparse point of tag {tag:#010x}, which Zipper does not restore; {BACKEND_HINT}"
        )
    };
    let damaged = || anyhow::anyhow!("WIM link {path:?} has damaged reparse data");
    let (target, relative) = match tag {
        WSL_SYMLINK_TAG => {
            ensure!(
                data.len() >= 4 && u32_at(data, 0) == WSL_SYMLINK_VERSION,
                damaged()
            );
            let target = std::str::from_utf8(&data[4..]).map_err(|_| damaged())?;
            (target.to_owned(), !target.starts_with('/'))
        }
        SYMLINK_TAG | JUNCTION_TAG => {
            let fixed = if tag == SYMLINK_TAG { 12 } else { 8 };
            ensure!(data.len() >= fixed, damaged());
            let offset = usize::from(u16_at(data, 0));
            let length = usize::from(u16_at(data, 2));
            let names = &data[fixed..];
            ensure!(
                offset % 2 == 0
                    && length % 2 == 0
                    && offset <= names.len()
                    && names.len() - offset >= length,
                damaged()
            );
            let target = utf16(&names[offset..offset + length]).ok_or_else(damaged)?;
            let relative = tag == SYMLINK_TAG && u32_at(data, 8) & SYMLINK_IS_RELATIVE != 0;
            (target.replace('\\', "/"), relative)
        }
        _ => return Err(unsupported()),
    };
    if relative {
        return Ok(target);
    }
    let rooted = target
        .strip_prefix("/??/")
        .map(|rest| match rest.as_bytes() {
            [drive, b':', ..] if drive.is_ascii_alphabetic() => &rest[2..],
            _ => "",
        })
        .unwrap_or(&target);
    ensure!(
        rooted.starts_with('/'),
        "WIM link {path:?} points to {target:?}, outside the image"
    );
    let mut parts: Vec<&str> = Vec::new();
    for part in rooted.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(
                    parts.pop().is_some(),
                    "WIM link {path:?} points outside the image"
                );
            }
            _ => parts.push(part),
        }
    }
    let up = "../".repeat(depth);
    Ok(match parts.is_empty() {
        true if depth == 0 => ".".to_owned(),
        true => up.trim_end_matches('/').to_owned(),
        false => format!("{up}{}", parts.join("/")),
    })
}

fn file_mode(attributes: u32) -> u32 {
    if attributes & READ_ONLY != 0 {
        READ_ONLY_MODE
    } else {
        FILE_MODE
    }
}

struct Writer<'o> {
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    leaders: HashMap<(u32, u64), String>,
    decoders: Decoders,
    name_keys: std::hash::RandomState,
}

fn name_key(keys: &std::hash::RandomState, name: &str) -> u64 {
    std::hash::BuildHasher::hash_one(keys, name)
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }

    fn read_small(
        &mut self,
        archive: &Archive,
        blob: &Blob,
        limit: u64,
        what: &str,
    ) -> Result<Vec<u8>> {
        ensure!(
            blob.size <= limit,
            "WIM {what} is larger than {limit} bytes"
        );
        let mut bytes = Vec::with_capacity(blob.size as usize);
        BlobReader::new(archive, &mut self.decoders, blob, true)?
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read the WIM {what}"))?;
        Ok(bytes)
    }

    fn file(
        &mut self,
        tree: &mut tree::Tree,
        archive: &Archive,
        node: &Node,
        path: String,
    ) -> Result<()> {
        tree.check()?;
        let group = (node.link_group != 0).then_some((node.blob, node.link_group));
        if let Some(leader) = group.and_then(|group| self.leaders.get(&group))
            && self.writes(&path)
        {
            let leader = leader.clone();
            return tree.hard_link(&path, &leader);
        }
        let time = modified(node.modified);
        let mode = file_mode(node.attributes);
        match archive.blobs.get(node.blob as usize) {
            Some(blob) => {
                let mut reader = BlobReader::new(archive, &mut self.decoders, blob, true)?;
                tree.file(&path, blob.size, mode, time, &mut reader)?;
            }
            None => tree.file(&path, 0, mode, time, &mut std::io::empty())?,
        }
        if let Some(group) = group
            && self.writes(&path)
        {
            self.leaders.insert(group, path);
        }
        Ok(())
    }

    // EXTERNAL(7-Zip WimIn): from WIM 1.12 the root list holds only one folder with an empty name.
    fn parse_image(
        &mut self,
        tree: &mut tree::Tree,
        meta: &mut Metadata,
        archive: &Archive,
        prefix: String,
    ) -> Result<Image> {
        let start = directory_start(meta)?;
        let mut used = Used(vec![0; meta.len().div_ceil(DENTRY_ALIGN).div_ceil(64)]);
        let (root, _) = read_dentry(meta, start)?.context("WIM image has no root directory")?;
        ensure!(
            root.name.is_empty() && root.attributes & (DIRECTORY | REPARSE_POINT) == DIRECTORY,
            "WIM image root is not one folder with an empty name; {BACKEND_HINT}"
        );
        used.claim(start, root.length)?;
        let mut at = start + root.length;
        read_streams(
            meta,
            &mut at,
            root.streams,
            true,
            &mut None,
            &mut used,
            String::new,
        )?;
        ensure!(
            read_dentry(meta, at)?.is_none(),
            "WIM image root list holds more than the root folder; {BACKEND_HINT}"
        );
        used.claim(at, TERMINATOR)?;
        if let Some(folder) = prefix.strip_suffix('/') {
            tree.check()?;
            tree.directory(folder, DIRECTORY_MODE, modified(root.modified))?;
        }
        let mut image = Image {
            prefix,
            names: String::new(),
            nodes: Vec::new(),
            with_data: Vec::new(),
        };
        let mut lists = VecDeque::new();
        let mut entries = 0usize;
        let mut path_bytes = 0usize;
        if root.subdir != 0 {
            lists.push_back((root.subdir as usize, None));
        }
        while let Some((mut at, parent)) = lists.pop_front() {
            ensure!(at >= start, "WIM folder list lies inside the security data");
            let mut names_here = HashSet::new();
            while let Some((fields, name)) = read_dentry(meta, at)? {
                used.claim(at, fields.length)?;
                entries += 1;
                ensure!(
                    entries <= tree::MAX_ENTRIES,
                    "WIM image has more than {} entries",
                    tree::MAX_ENTRIES
                );
                let name = name
                    .with_context(|| format!("WIM name is not valid UTF-16; {BACKEND_HINT}"))?
                    .replace('/', "_");
                ensure!(
                    !name.is_empty(),
                    "WIM directory entry other than the root has no name"
                );
                ensure!(
                    name != "." && name != "..",
                    "unsafe archive path: WIM entry named {name:?}"
                );
                ensure!(
                    names_here.insert(name_key(&self.name_keys, &name)),
                    "WIM folder {:?} holds two entries named {name:?}",
                    image.path_to(parent, "")
                );
                let (parent_length, depth) = match parent {
                    None => (image.prefix.len(), 0),
                    Some(parent) => {
                        let parent: &Node = &image.nodes[parent as usize];
                        (parent.path_length as usize + 1, parent.depth + 1)
                    }
                };
                let path_length = parent_length + name.len();
                path_bytes += path_length;
                ensure!(
                    path_bytes <= MAX_PATH_BYTES,
                    "WIM image paths exceed 64 MiB"
                );
                let directory = fields.attributes & DIRECTORY != 0;
                let reparse = fields.attributes & REPARSE_POINT != 0;
                let mut hash = (fields.hash != [0; HASH]).then_some(fields.hash);
                at += fields.length;
                let path = || image.path_to(parent, &name);
                read_streams(
                    meta,
                    &mut at,
                    fields.streams,
                    directory && !reparse,
                    &mut hash,
                    &mut used,
                    path,
                )?;
                let blob = match &hash {
                    Some(hash) => archive.blob_index(hash).with_context(|| {
                        format!(
                            "WIM entry {:?} names data the blob table does not have",
                            path()
                        )
                    })?,
                    None => NO_BLOB,
                };
                ensure!(
                    !directory || fields.subdir == 0 || !reparse,
                    "WIM link {:?} has entries under it",
                    path()
                );
                let node = Node {
                    parent,
                    name: 0..0,
                    path_length: path_length as u32,
                    depth,
                    attributes: fields.attributes,
                    blob,
                    modified: fields.modified,
                    link_group: fields.tag_or_group,
                };
                if reparse {
                    let path = path();
                    ensure!(blob != NO_BLOB, "WIM link {path:?} has no reparse data");
                    let blob = archive.blobs[blob as usize];
                    let data = self.read_small(archive, &blob, MAX_REPARSE, "reparse data")?;
                    let target =
                        symlink_target(node.link_group as u32, &data, &path, depth as usize)?;
                    tree.check()?;
                    tree.symlink(&path, target)?;
                } else if directory {
                    tree.check()?;
                    tree.directory(&path(), DIRECTORY_MODE, modified(node.modified))?;
                    let kept = image.keep(node, &name);
                    if fields.subdir != 0 {
                        lists.push_back((fields.subdir as usize, Some(kept)));
                    }
                } else if blob == NO_BLOB {
                    let path = path();
                    self.file(tree, archive, &node, path)?;
                } else {
                    let kept = image.keep(node, &name);
                    image.with_data.push(kept);
                }
            }
            used.claim(at, TERMINATOR)?;
        }
        Ok(image)
    }

    fn image(
        &mut self,
        tree: &mut tree::Tree,
        archive: &Archive,
        number: usize,
        many: bool,
    ) -> Result<()> {
        let mut meta = Metadata::open(archive, archive.images[number])?;
        let prefix = if many {
            format!("{}/", number + 1)
        } else {
            String::new()
        };
        self.leaders.clear();
        let mut image = self.parse_image(tree, &mut meta, archive, prefix)?;
        drop(meta);
        self.leaders.clear();
        let mut with_data = std::mem::take(&mut image.with_data);
        with_data.sort_by_key(|&index| {
            let blob = &archive.blobs[image.nodes[index as usize].blob as usize];
            let order = match blob.place {
                Place::Stored { offset } | Place::Chunked { offset, .. } => (offset, 0),
                Place::Solid { solid, offset } => (archive.solids[solid].offset, offset),
            };
            (order, index)
        });
        let mut run = NO_BLOB;
        for index in with_data {
            let blob = image.nodes[index as usize].blob;
            if blob != run {
                self.leaders.clear();
                run = blob;
            }
            let path = image.path(index);
            self.file(tree, archive, &image.nodes[index as usize], path)?;
        }
        Ok(())
    }
}

fn parse(tree: &mut tree::Tree, archive: &Archive, options: ReadOptions) -> Result<()> {
    let mut writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        leaders: HashMap::new(),
        decoders: Decoders::default(),
        name_keys: std::hash::RandomState::new(),
    };
    let many = archive.images.len() > 1;
    for number in 0..archive.images.len() {
        writer.image(tree, archive, number, many)?;
    }
    if many {
        tree.check()?;
        let xml = &archive.xml;
        tree.file(
            &format!("[{}].xml", archive.header.part),
            xml.len() as u64,
            FILE_MODE,
            None,
            &mut &xml[..],
        )?;
    }
    Ok(())
}

/// Verify and optionally extract a WIM or ESD file into a new directory. It
/// reads stored, XPRESS, LZX and LZMS resources and LZMS, XPRESS and LZX
/// solid resources, checks the SHA-1 of every resource it reads, and writes
/// each image as `7zz x` writes it: one image at the top of the
/// destination, several as folders `1`, `2` and so on with the XML data
/// beside them. Files get their modification time and read-only attribute,
/// hard links stay linked, and symbolic links and junctions that stay
/// inside the image are created. A split WIM, an encrypted ESD and
/// alternate data streams are refused.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let archive = Archive::open(path)?;
    ensure!(
        archive.images.len() == archive.header.images as usize,
        "WIM header counts {} images, and its blob table holds {}",
        archive.header.images,
        archive.images.len()
    );
    archive.ensure_boot_metadata()?;
    tree::extract("WIM", path, destination, options, |tree| {
        parse(tree, &archive, options)
    })
}

#[cfg(test)]
#[path = "../tests/common/wim_writer.rs"]
mod wim_writer;

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn metadata_that_changes_between_its_hash_and_its_parse_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("changing.wim");
        let images = [vec![wim_writer::file("x", b"x")]];
        let layout = std::cell::RefCell::new(wim_writer::Layout::default());
        let shared = std::rc::Rc::new(layout);
        let seen = shared.clone();
        let options = wim_writer::Options {
            file_hook: Some(Box::new(move |_, layout| {
                *seen.borrow_mut() = layout.clone()
            })),
            ..wim_writer::Options::default()
        };
        std::fs::write(&path, wim_writer::write(&images, &options)).unwrap();
        let archive = Archive::open(&path).unwrap();
        let mut meta = Metadata::open(&archive, archive.images[0]).unwrap();
        assert!(meta.read(0, 8).is_ok());
        let (offset, _) = shared.borrow().metadata[0];
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[offset + 1] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let mut fresh = Metadata::open(&archive, archive.images[0]);
        assert!(
            fresh
                .as_mut()
                .is_err_and(|error| format!("{error:#}").contains("SHA-1"))
        );
        meta.start = usize::MAX;
        let error = meta.read(0, 8).unwrap_err();
        assert!(
            error.to_string().contains("changed while it was read"),
            "{error}"
        );
    }

    #[test]
    fn every_chunk_size_field_is_a_power_of_two_of_at_least_4_kib_or_the_default() {
        for shift in 0..32 {
            let value = 1u32 << shift;
            assert_eq!(chunk_bits(value).ok(), (shift >= 12).then_some(shift));
            if shift > 1 {
                assert!(chunk_bits(value + 1).is_err());
            }
        }
        assert_eq!(chunk_bits(0).unwrap(), DEFAULT_CHUNK_BITS);
    }

    #[test]
    fn file_times_read_as_windows_ticks_since_1601() {
        assert_eq!(modified(0), None);
        let unix = WINDOWS_TO_UNIX_SECONDS * TICKS_PER_SECOND;
        assert_eq!(modified(unix), Some(std::time::UNIX_EPOCH));
        assert_eq!(
            modified(unix + 15),
            std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(1500))
        );
        assert_eq!(
            modified(unix - TICKS_PER_SECOND + 1),
            std::time::UNIX_EPOCH.checked_sub(std::time::Duration::from_nanos(999_999_900))
        );
        assert!(modified(u64::MAX).is_some());
    }

    fn symlink(target: &str, relative: bool) -> Vec<u8> {
        let name: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut data = Vec::new();
        for value in [0u16, name.len() as u16, 0, 0] {
            data.extend(value.to_le_bytes());
        }
        data.extend(u32::from(relative).to_le_bytes());
        data.extend(&name);
        data
    }

    #[test]
    fn rooted_link_targets_become_relative_to_the_link_and_stay_in_the_image() {
        let target = |text: &str, relative: bool, depth: usize| {
            symlink_target(SYMLINK_TAG, &symlink(text, relative), "x", depth).ok()
        };
        assert_eq!(target("a\\b", true, 3).as_deref(), Some("a/b"));
        assert_eq!(target("\\a\\b", false, 0).as_deref(), Some("a/b"));
        assert_eq!(target("\\a\\.\\b", false, 2).as_deref(), Some("../../a/b"));
        assert_eq!(
            target("\\??\\D:\\a\\..\\b", false, 1).as_deref(),
            Some("../b")
        );
        assert_eq!(target("\\", false, 0).as_deref(), Some("."));
        assert_eq!(target("\\", false, 2).as_deref(), Some("../.."));
        assert_eq!(target("\\..\\a", false, 2), None);
        assert_eq!(target("\\??\\UNC\\host\\share", false, 0), None);
        assert_eq!(target("\\??\\Volume{1}\\a", false, 0), None);
        assert_eq!(target("a", false, 0), None);
        assert!(symlink_target(0x8000_0013, &[], "x", 0).is_err());
        assert!(symlink_target(SYMLINK_TAG, &[0; 11], "x", 0).is_err());
        assert!(symlink_target(JUNCTION_TAG, &symlink("\\??\\C:\\a", false)[..7], "x", 0).is_err());
    }

    fn solid(size: u64) -> Solid {
        Solid {
            offset: 0,
            header: SolidHeader {
                size,
                chunk_bits: 15,
                method: Method::Lzms,
                chunks: 1,
            },
            chunk_starts: vec![0, 1],
        }
    }

    fn solid_entry(offset: u64, size: u64, tag: u8) -> BlobEntry {
        BlobEntry {
            resource: Resource {
                packed: size,
                flags: RESOURCE_SOLID,
                offset,
                size: 0,
            },
            part: 1,
            references: 1,
            hash: [tag; HASH],
        }
    }

    #[test]
    fn a_solid_blob_lies_in_the_resource_that_holds_its_offset_or_is_refused() {
        let mut bytes = [0u8; HEADER];
        bytes[..8].copy_from_slice(SIGNATURE);
        bytes[8] = HEADER as u8;
        bytes[12..16].copy_from_slice(&SOLID_VERSION.to_le_bytes());
        bytes[40] = 1;
        bytes[42] = 1;
        let mut archive = Archive {
            file: ArchiveFile::from(tempfile::tempfile().unwrap()),
            header: header(&bytes).unwrap(),
            blobs: Vec::new(),
            solids: vec![solid(10), solid(5), solid(7)],
            images: Vec::new(),
            xml: Vec::new(),
        };
        let mut pending = vec![
            solid_entry(0, 5, 1),
            solid_entry(4, 1, 2),
            solid_entry(5, 7, 3),
            solid_entry(11, 1, 4),
        ];
        archive.finish_group(&mut pending, 1).unwrap();
        let placed: Vec<(usize, u64)> = archive
            .blobs
            .iter()
            .map(|blob| match blob.place {
                Place::Solid { solid, offset } => (solid, offset),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(placed, [(1, 0), (1, 4), (2, 0), (2, 6)]);
        for (offset, size) in [(4, 2), (12, 0), (11, 2)] {
            let mut pending = vec![solid_entry(offset, size, 9)];
            assert!(
                archive.finish_group(&mut pending, 1).is_err(),
                "{offset} {size}"
            );
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_never_panic_and_keep_their_chunk_size_parts_and_method_in_range() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(header) = header(&bytes) {
            assert!((SMALLEST_CHUNK_BITS..32).contains(&header.chunk_bits));
            assert!(header.part >= 1 && header.part <= header.parts);
            assert!(header.method.is_some() == (header.flags & COMPRESSED != 0));
            assert!(header.version == VERSION || header.version == SOLID_VERSION);
            assert!(header.blob_table.packed < 1 << PACKED_BITS);
        }
    }

    #[kani::proof]
    fn blob_entries_never_panic_and_keep_56_bit_packed_sizes() {
        let bytes: [u8; BLOB] = kani::any();
        let entry = blob_entry(&bytes);
        assert!(entry.resource.packed < 1 << PACKED_BITS);
        assert!(entry.resource.flags == bytes[7]);
        assert!(entry.hash[..] == bytes[30..]);
    }

    #[kani::proof]
    fn chunk_tables_fit_their_resource_and_every_chunk_lies_inside_its_size() {
        let size: u64 = kani::any();
        let packed: u64 = kani::any();
        let chunk_bits: u32 = kani::any();
        if let Ok(table) = chunk_table(size, packed, chunk_bits) {
            assert!(table.table_bytes <= packed);
            assert!(table.entry_bytes == if size < 1 << 32 { 4 } else { 8 });
            let covered = u128::from(table.chunks) << chunk_bits;
            assert!(covered >= u128::from(size));
            assert!(covered - (1u128 << chunk_bits) < u128::from(size));
            let index: u64 = kani::any();
            kani::assume(index < table.chunks);
            let this = chunk_size(size, chunk_bits, index);
            assert!(this >= 1 && this <= 1 << chunk_bits);
            assert!(u128::from(index << chunk_bits) + u128::from(this) <= u128::from(size));
        }
    }

    #[kani::proof]
    fn solid_headers_never_panic_and_fit_their_chunk_table() {
        let bytes: [u8; SOLID_HEADER] = kani::any();
        let packed: u64 = kani::any();
        if let Ok(header) = solid_header(&bytes, packed) {
            assert!(SOLID_HEADER as u64 + 4 * header.chunks <= packed);
            assert!(header.chunks <= MAX_SOLID_CHUNKS);
            assert!(u128::from(header.chunks) << header.chunk_bits >= u128::from(header.size));
            assert!((SMALLEST_CHUNK_BITS..32).contains(&header.chunk_bits));
        }
    }

    const META: usize = 128;

    #[kani::proof]
    #[kani::unwind(15)]
    fn directory_entries_never_panic_and_keep_their_names_inside_themselves() {
        let record: [u8; META] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= META);
        let at: usize = kani::any();
        let meta_len: usize = kani::any();
        if let Ok(Some(fields)) = dentry(&record[..length], at, meta_len) {
            assert!(at % DENTRY_ALIGN == 0 && at <= meta_len);
            assert!(fields.length >= DENTRY && fields.length % DENTRY_ALIGN == 0);
            assert!(fields.length <= meta_len - at && fields.length <= length);
            assert!(fields.name.start == DENTRY);
            assert!(fields.name.len() % 2 == 0);
            assert!(fields.name.end + 2 <= fields.length);
            assert!(fields.subdir <= meta_len as u64);
        }
    }

    #[kani::proof]
    #[kani::unwind(15)]
    fn stream_entries_never_panic_and_keep_their_names_inside_themselves() {
        let record: [u8; 64] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= record.len());
        let at: usize = kani::any();
        let meta_len: usize = kani::any();
        if let Ok(stream) = stream_entry(&record[..length], at, meta_len) {
            assert!(stream.length >= STREAM_ENTRY && stream.length % DENTRY_ALIGN == 0);
            assert!(at <= meta_len && stream.length <= meta_len - at);
            assert!(stream.length <= length);
            assert!(stream.name.start == STREAM_ENTRY);
            assert!(stream.name.end + 2 <= stream.length);
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn security_data_never_panics_and_ends_aligned_inside_its_metadata() {
        let prefix: [u8; 40] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= prefix.len());
        let meta_len: usize = kani::any();
        if let Ok(end) = security_end(&prefix[..length], meta_len) {
            assert!(end >= 8 && end <= meta_len && end % DENTRY_ALIGN == 0);
        }
        if length >= 8 {
            let head: [u8; 8] = prefix[..8].try_into().unwrap();
            assert!(security_sizes_length(&head, meta_len) <= meta_len);
        }
    }
}
