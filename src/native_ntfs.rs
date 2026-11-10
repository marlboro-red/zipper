//! In-process extraction of NTFS volumes: the MFT with its update sequence
//! fixups, extension records, resident and non-resident data, sparse runs
//! and LZNT1 compression. See `docs/reference/ntfs-reader.md` for what it
//! accepts and restores.
pub use crate::tree::ReadOptions;
use crate::tree::{self, BACKEND_HINT};
use crate::{BadField, Entry, Field, field_check, lznt1};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
    time::SystemTime,
};

const BOOT_SECTOR: usize = 512;
const SIGNATURE: &[u8; 8] = b"NTFS    ";
const SIGNATURE_AT: usize = 3;
const BOOT_MARK: [u8; 2] = [0x55, 0xAA];
const FIXED_DISK: u8 = 0xF8;
const SMALLEST_SECTOR_LOG: u32 = 9;
const LARGEST_SECTOR_LOG: u32 = 12;
const LARGEST_CLUSTER_LOG: u32 = 30;
const SECTOR_COUNT_BITS: u32 = 62;
const LARGEST_FIELD_LOG: u32 = 31;
const SMALL_BLOCK_FIELD: u32 = 256;
const MFT_PROBE: usize = 4096;
const LARGEST_RECORD_LOG: u32 = 12;
const RECORD_HEAD: usize = 0x30;
const NUMBERED_FIXUPS: usize = 0x30;
const FIXUP_STRIDE: usize = 512;
const FILE_MAGIC: u32 = u32::from_le_bytes(*b"FILE");
const LARGEST_UNUSED_MAGIC: u32 = 5;
const IGNORED_MAGICS: [&[u8; 4]; 6] = [b"BAAD", b"INDX", b"HOLE", b"RSTR", b"RCRD", b"CHKD"];
const IN_USE: u16 = 0x1;
const IS_DIRECTORY: u16 = 0x2;
const ATTRIBUTE_HEAD: usize = 0x18;
const NON_RESIDENT_HEAD: usize = 0x40;
const PACKED_HEAD: usize = 0x48;
const END_MARKER: u32 = 0xFFFF_FFFF;
const END_LENGTH: usize = 8;
const ATTRIBUTE_ALIGN: usize = 8;
const STANDARD_INFORMATION: u32 = 0x10;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;
const SPARSE_FLAG: u16 = 0x8000;
const COMPRESSION_FLAGS: u16 = 0x00FF;
const STANDARD_UNIT_LOG: u8 = 4;
const LARGEST_COMPRESSED_CLUSTER_LOG: u32 = 12;
const FILE_NAME_HEAD: usize = 0x42;
const STANDARD_INFORMATION_HEAD: usize = 0x24;
const WIN32_NAME: u8 = 1;
const DOS_NAME: u8 = 2;
const REFERENCE_INDEX: u64 = (1 << 48) - 1;
const SEQUENCE_SHIFT: u32 = 48;
const LONGEST_RUN_FIELD: usize = 8;
const HIGHEST_VCN_END: u64 = 1 << 63;
const SYSTEM_RECORDS: u32 = 16;
const ROOT_RECORD: u64 = 5;
const BAD_CLUSTERS_RECORD: u32 = 8;
const BAD_CLUSTERS_STREAM: &str = "$Bad";
const SYSTEM_FOLDER: &str = "[SYSTEM]";
const LOST_FOLDER: &str = "[LOST]";
const READ_ONLY: u32 = 0x1;
const ENCRYPTED: u32 = 0x4000;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;
const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
const TICKS_PER_SECOND: u64 = 10_000_000;
const MAX_RECORDS: u64 = 1 << 23;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_RUNS: usize = 1 << 23;
const MAX_DEPTH: usize = 256;
const MAX_REPARSE: usize = 16 * 1024;
const SYMLINK_TAG: u32 = 0xA000_000C;
const JUNCTION_TAG: u32 = 0xA000_0003;
const WSL_SYMLINK_TAG: u32 = 0xA000_001D;
const WSL_SYMLINK_VERSION: u32 = 2;
const SYMLINK_IS_RELATIVE: u32 = 1;
const REPARSE_HEAD: usize = 8;

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | u64::from(u32_at(bytes, at + 4)) << 32
}

fn exact_log(value: u32) -> Option<u32> {
    (value.is_power_of_two() && value.trailing_zeros() < LARGEST_FIELD_LOG)
        .then(|| value.trailing_zeros())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Boot {
    sector_log: u32,
    cluster_log: u32,
    clusters: u64,
    mft_cluster: u64,
}

// EXTERNAL(7-Zip NtfsHandler CHeader::Parse): every field check here is 7-Zip's, so a boot
// sector 7-Zip refuses is refused, and one it opens is read the same way.
fn boot_sector(bytes: &[u8; BOOT_SECTOR]) -> Field<Boot> {
    field_check(
        bytes[BOOT_SECTOR - 2..] == BOOT_MARK,
        "NTFS boot sector lacks its 55 AA mark",
    )?;
    field_check(
        bytes[0] == 0xE9 || bytes[0] == 0xEB && bytes[2] == 0x90,
        "NTFS boot sector does not start with a jump",
    )?;
    field_check(
        &bytes[SIGNATURE_AT..SIGNATURE_AT + SIGNATURE.len()] == SIGNATURE,
        "not an NTFS boot sector",
    )?;
    let sector_log = exact_log(u32::from(u16_at(bytes, 11)))
        .filter(|log| (SMALLEST_SECTOR_LOG..=LARGEST_SECTOR_LOG).contains(log))
        .ok_or(BadField("NTFS sector size is not 512 to 4096 bytes"))?;
    let per_cluster_log = exact_log(u32::from(bytes[13]))
        .ok_or(BadField("NTFS sectors per cluster is not a power of two"))?;
    let cluster_log = sector_log + per_cluster_log;
    field_check(
        cluster_log <= LARGEST_CLUSTER_LOG,
        "NTFS cluster is larger than 1 GiB",
    )?;
    field_check(
        bytes[14..21].iter().all(|&byte| byte == 0)
            && bytes[21] == FIXED_DISK
            && u16_at(bytes, 22) == 0
            && u32_at(bytes, 32) == 0
            && bytes[0x25] == 0
            && matches!(bytes[0x26], 0 | 0x80)
            && bytes[0x27] == 0,
        "NTFS boot sector has reserved fields that are not zero",
    )?;
    let sectors = u64_at(bytes, 0x28);
    field_check(
        sectors < 1 << (SECTOR_COUNT_BITS - sector_log),
        "NTFS sector count is too large",
    )?;
    field_check(
        u32_at(bytes, 0x40) < SMALL_BLOCK_FIELD && u32_at(bytes, 0x44) < SMALL_BLOCK_FIELD,
        "NTFS record or index block size is out of range",
    )?;
    Ok(Boot {
        sector_log,
        cluster_log,
        clusters: sectors >> per_cluster_log,
        mft_cluster: u64_at(bytes, 0x30),
    })
}

fn magic_is_ignored(magic: u32) -> bool {
    magic <= LARGEST_UNUSED_MAGIC
        || magic == u32::MAX
        || IGNORED_MAGICS
            .iter()
            .any(|known| magic == u32::from_le_bytes(**known))
}

// EXTERNAL(7-Zip NtfsHandler CMftRec::Parse): the record number at 0x2C may be 0, which
// ntfs-3g writes in unused records.
fn apply_fixups(record: &mut [u8], stride: usize, index: u64) -> Field<()> {
    field_check(
        stride >= 4 && record.len() >= 8 && record.len().is_multiple_of(stride),
        "NTFS record is not a whole number of sectors",
    )?;
    let sectors = record.len() / stride;
    let offset = usize::from(u16_at(record, 4));
    let count = usize::from(u16_at(record, 6));
    field_check(
        offset % 2 == 0 && count != 0 && count - 1 == sectors && offset + 2 * count <= stride - 2,
        "NTFS record has a damaged update sequence array",
    )?;
    if offset >= NUMBERED_FIXUPS {
        let number = u32_at(record, 0x2C);
        field_check(
            number == index as u32 || number == 0,
            "NTFS record holds the number of another record",
        )?;
    }
    let check = u16_at(record, offset);
    for sector in 1..count {
        let end = sector * stride - 2;
        field_check(
            u16_at(record, end) == check,
            "NTFS record fails its update sequence check",
        )?;
        let saved = offset + 2 * sector;
        record.copy_within(saved..saved + 2, end);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordHeader {
    sequence: u16,
    flags: u16,
    base: u64,
    attributes: Range<usize>,
}

fn record_header(record: &[u8]) -> Field<RecordHeader> {
    field_check(record.len() >= RECORD_HEAD, "NTFS record is too short")?;
    let first = usize::from(u16_at(record, 0x14));
    let in_use = u32_at(record, 0x18) as usize;
    let allocated = u32_at(record, 0x1C) as usize;
    field_check(
        first < record.len()
            && first.is_multiple_of(ATTRIBUTE_ALIGN)
            && in_use.is_multiple_of(ATTRIBUTE_ALIGN)
            && in_use <= record.len()
            && allocated == record.len(),
        "NTFS record header has sizes outside the record",
    )?;
    Ok(RecordHeader {
        sequence: u16_at(record, 0x10),
        flags: u16_at(record, 0x16),
        base: u64_at(record, 0x20),
        attributes: first..in_use,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NonResident {
    low_vcn: u64,
    high_vcn: u64,
    allocated: u64,
    size: u64,
    initialized: u64,
    unit: u8,
    flags: u16,
    packed: Option<u64>,
    sparse_packed: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Resident(Range<usize>),
    NonResident(NonResident, Range<usize>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Attribute {
    kind: u32,
    length: usize,
    name: Range<usize>,
    value: Value,
}

// EXTERNAL(7-Zip NtfsHandler CAttr::Parse): the end marker counts 8 bytes, and every other
// attribute is at least 24 bytes, a multiple of 8, and holds its name and value.
fn attribute(bytes: &[u8]) -> Field<Option<Attribute>> {
    field_check(bytes.len() >= 4, "NTFS attribute is cut short")?;
    let kind = u32_at(bytes, 0);
    if kind == END_MARKER {
        return Ok(None);
    }
    field_check(bytes.len() >= ATTRIBUTE_HEAD, "NTFS attribute is cut short")?;
    let length = u32_at(bytes, 4) as usize;
    field_check(
        length <= bytes.len() && length.is_multiple_of(ATTRIBUTE_ALIGN) && length >= ATTRIBUTE_HEAD,
        "NTFS attribute has a length outside its record",
    )?;
    let non_resident = bytes[8] != 0;
    let name_length = usize::from(bytes[9]) * 2;
    let name_offset = usize::from(u16_at(bytes, 0x0A));
    field_check(
        name_length == 0 || name_offset + name_length <= length,
        "NTFS attribute name lies outside the attribute",
    )?;
    let name = if name_length == 0 {
        0..0
    } else {
        name_offset..name_offset + name_length
    };
    field_check(
        !non_resident || length >= NON_RESIDENT_HEAD,
        "NTFS non-resident attribute is cut short",
    )?;
    let (start, size) = if non_resident {
        let start = usize::from(u16_at(bytes, 0x20));
        (start, length.saturating_sub(start))
    } else {
        (
            usize::from(u16_at(bytes, 0x14)),
            u32_at(bytes, 0x10) as usize,
        )
    };
    field_check(
        start <= length && size <= length - start,
        "NTFS attribute value lies outside the attribute",
    )?;
    let range = start..start + size;
    if !non_resident {
        return Ok(Some(Attribute {
            kind,
            length,
            name,
            value: Value::Resident(range),
        }));
    }
    let unit = bytes[0x22];
    let flags = u16_at(bytes, 0x0C);
    field_check(
        unit == 0 || length >= PACKED_HEAD,
        "NTFS compressed attribute is cut short",
    )?;
    let packed = (unit != 0).then(|| u64_at(bytes, 0x40));
    let sparse_packed = (unit == 0 && flags & SPARSE_FLAG != 0 && length >= PACKED_HEAD)
        .then(|| u64_at(bytes, 0x40));
    Ok(Some(Attribute {
        kind,
        length,
        name,
        value: Value::NonResident(
            NonResident {
                low_vcn: u64_at(bytes, 0x10),
                high_vcn: u64_at(bytes, 0x18),
                allocated: u64_at(bytes, 0x28),
                size: u64_at(bytes, 0x30),
                initialized: u64_at(bytes, 0x38),
                unit,
                flags,
                packed,
                sparse_packed,
            },
            range,
        ),
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    vcn: u64,
    lcn: Option<u64>,
}

fn little_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0, |value, &byte| value << 8 | u64::from(byte))
}

fn sign_extended(bytes: &[u8]) -> i64 {
    let unused = 64 - 8 * bytes.len() as u32;
    ((little_endian(bytes) << unused) as i64) >> unused
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    bytes: usize,
    length: u64,
    lcn: Option<u64>,
}

fn run(
    list: &[u8],
    previous_lcn: u64,
    clusters_left: u64,
    clusters: u64,
    holes_allowed: bool,
) -> Field<Option<Run>> {
    let Some((&head, rest)) = list.split_first() else {
        return Ok(None);
    };
    if head == 0 {
        return Ok(None);
    }
    let length_bytes = usize::from(head & 0xF);
    let offset_bytes = usize::from(head >> 4);
    field_check(
        length_bytes != 0 && length_bytes <= LONGEST_RUN_FIELD && length_bytes <= rest.len(),
        "NTFS run has a damaged length",
    )?;
    let length = little_endian(&rest[..length_bytes]);
    field_check(
        length != 0 && length <= clusters_left,
        "NTFS run passes the end of its attribute",
    )?;
    let rest = &rest[length_bytes..];
    field_check(
        offset_bytes <= LONGEST_RUN_FIELD && offset_bytes <= rest.len(),
        "NTFS run has a damaged cluster",
    )?;
    let lcn = if offset_bytes == 0 {
        field_check(
            holes_allowed,
            "NTFS sparse run in a stream that is not sparse",
        )?;
        None
    } else {
        let next = previous_lcn.checked_add_signed(sign_extended(&rest[..offset_bytes]));
        field_check(
            next.is_some_and(|next| length <= clusters && next <= clusters - length),
            "NTFS run lies outside the volume",
        )?;
        next
    };
    Ok(Some(Run {
        bytes: 1 + length_bytes + offset_bytes,
        length,
        lcn,
    }))
}

// EXTERNAL(7-Zip NtfsHandler CAttr::ParseExtents): each piece starts where the last ended, its
// runs fill it exactly, and the list ends in an entry without a cluster where the next starts.
fn runs(
    list: &[u8],
    low_vcn: u64,
    high_vcn: u64,
    clusters: u64,
    sparse: bool,
    extents: &mut Vec<Extent>,
) -> Field<()> {
    let end = high_vcn.wrapping_add(1);
    field_check(
        extents.last().map(|last| last.vcn) == Some(low_vcn)
            && end <= HIGHEST_VCN_END
            && low_vcn <= end,
        "NTFS run list does not continue its attribute",
    )?;
    extents.pop();
    let mut vcn = low_vcn;
    let mut lcn = 0u64;
    let mut at = 0;
    while let Some(found) = run(&list[at..], lcn, end - vcn, clusters, sparse)? {
        at += found.bytes;
        if let Some(cluster) = found.lcn {
            lcn = cluster;
        }
        extents.push(Extent {
            vcn,
            lcn: found.lcn,
        });
        vcn += found.length;
    }
    extents.push(Extent { vcn, lcn: None });
    field_check(vcn == end, "NTFS run list does not fill its attribute")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileNameFields {
    parent: u64,
    kind: u8,
    name: Range<usize>,
}

fn file_name(value: &[u8]) -> Field<FileNameFields> {
    field_check(
        value.len() >= FILE_NAME_HEAD,
        "NTFS file name attribute is cut short",
    )?;
    let length = usize::from(value[0x40]) * 2;
    field_check(
        FILE_NAME_HEAD + length <= value.len(),
        "NTFS file name runs past its attribute",
    )?;
    Ok(FileNameFields {
        parent: u64_at(value, 0),
        kind: value[0x41],
        name: FILE_NAME_HEAD..FILE_NAME_HEAD + length,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Standard {
    modified: u64,
    attributes: u32,
}

fn standard_information(value: &[u8]) -> Field<Standard> {
    field_check(
        value.len() >= STANDARD_INFORMATION_HEAD,
        "NTFS standard information is cut short",
    )?;
    Ok(Standard {
        modified: u64_at(value, 0x08),
        attributes: u32_at(value, 0x20),
    })
}

fn modified(ticks: u64) -> Option<SystemTime> {
    let seconds = ticks / TICKS_PER_SECOND;
    let nanos = (ticks % TICKS_PER_SECOND * 100) as u32;
    if seconds >= WINDOWS_TO_UNIX_SECONDS {
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(
            seconds - WINDOWS_TO_UNIX_SECONDS,
            nanos,
        ))
    } else {
        std::time::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(
                WINDOWS_TO_UNIX_SECONDS - seconds,
            ))?
            .checked_add(std::time::Duration::new(0, nanos))
    }
}

fn utf16_units(bytes: &[u8]) -> impl Iterator<Item = u16> + Clone + '_ {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
}

fn utf16(bytes: &[u8]) -> Option<String> {
    char::decode_utf16(utf16_units(bytes))
        .collect::<Result<String, _>>()
        .ok()
}

// EXTERNAL(7-Zip NtfsHandler GetString): a stream name ends at its first NUL, so a name that
// starts with one names the unnamed stream.
fn stream_name(bytes: &[u8]) -> Vec<u16> {
    utf16_units(bytes).take_while(|&unit| unit != 0).collect()
}

fn names_bad_clusters(name: &[u16]) -> bool {
    name.iter().copied().eq(BAD_CLUSTERS_STREAM.encode_utf16())
}

trait ReadSeek: Read + Seek {}

impl<T: Read + Seek + ?Sized> ReadSeek for T {}

struct Disk<'v> {
    volume: &'v mut dyn ReadSeek,
    length: u64,
}

impl Disk<'_> {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.length),
            "NTFS read passes the end of the volume"
        );
        self.volume.seek(SeekFrom::Start(offset))?;
        self.volume
            .read_exact(buffer)
            .context("cannot read the NTFS volume")
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    cluster_log: u32,
    clusters: u64,
    sector_size: usize,
    record_log: u32,
}

struct Piece {
    header: NonResident,
    runs: Vec<u8>,
}

enum RawPiece {
    Resident(Vec<u8>),
    NonResident(Piece),
}

struct Stream {
    extents: Vec<Extent>,
    size: u64,
    initialized: u64,
    lznt1_unit_log: Option<u32>,
}

enum Data {
    Resident(Vec<u8>),
    NonResident(Stream),
}

impl Data {
    fn size(&self) -> u64 {
        match self {
            Data::Resident(bytes) => bytes.len() as u64,
            Data::NonResident(stream) => stream.size,
        }
    }

    fn placed_clusters(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        let extents: &[Extent] = match self {
            Data::Resident(_) => &[],
            Data::NonResident(stream) => &stream.extents,
        };
        extents
            .windows(2)
            .filter_map(|pair| pair[0].lcn.map(|lcn| (lcn, pair[1].vcn - pair[0].vcn)))
    }
}

// EXTERNAL(7-Zip NtfsHandler DataParseExtents): the first piece's sizes describe the stream,
// and the pieces' clusters cover its allocation.
fn build_stream(mut pieces: Vec<RawPiece>, geometry: Geometry, what: &str) -> Result<Data> {
    if pieces.is_empty() {
        return Ok(Data::Resident(Vec::new()));
    }
    if let [RawPiece::Resident(_)] = &pieces[..] {
        let Some(RawPiece::Resident(bytes)) = pieces.pop() else {
            unreachable!()
        };
        return Ok(Data::Resident(bytes));
    }
    let mut placed = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match piece {
            RawPiece::NonResident(piece) => placed.push(piece),
            RawPiece::Resident(_) => {
                bail!("NTFS stream {what} has more than one piece, and one is resident")
            }
        }
    }
    placed.sort_by_key(|piece| piece.header.low_vcn);
    let first = placed[0].header;
    ensure!(
        first.unit == 0 || first.unit == STANDARD_UNIT_LOG,
        "NTFS stream {what} has a compression unit of 2^{} clusters, which 7-Zip does not read",
        first.unit
    );
    let lznt1 = first.unit != 0 && first.flags & COMPRESSION_FLAGS != 0;
    ensure!(
        !lznt1 || geometry.cluster_log <= LARGEST_COMPRESSED_CLUSTER_LOG,
        "NTFS stream {what} is compressed on clusters over 4 KiB, which NTFS does not allow"
    );
    let cluster = 1u64 << geometry.cluster_log;
    let last = placed[placed.len() - 1].header;
    ensure!(
        first.allocated >= first.size
            && last.high_vcn.wrapping_add(1) == first.allocated >> geometry.cluster_log
            && first.allocated.is_multiple_of(cluster),
        "NTFS stream {what} has sizes that do not match its clusters"
    );
    let sparse = first.unit != 0 || first.flags & SPARSE_FLAG != 0;
    let mut extents = vec![Extent { vcn: 0, lcn: None }];
    for piece in &placed {
        runs(
            &piece.runs,
            piece.header.low_vcn,
            piece.header.high_vcn,
            geometry.clusters,
            sparse,
            &mut extents,
        )
        .map_err(|error| anyhow!("{error} in stream {what}"))?;
        ensure!(
            extents.len() <= MAX_RUNS,
            "NTFS stream {what} has more than {MAX_RUNS} runs"
        );
    }
    let mut placed_clusters = 0u64;
    let mut holes = false;
    for pair in extents.windows(2) {
        match pair[0].lcn {
            Some(_) => placed_clusters += pair[1].vcn - pair[0].vcn,
            None => holes = true,
        }
    }
    let placed_bytes = placed_clusters
        .checked_mul(cluster)
        .context("NTFS stream size overflows")?;
    let declared = match (first.packed, holes) {
        (Some(packed), _) => Some(packed),
        (None, true) => first.sparse_packed,
        (None, false) => Some(first.allocated),
    };
    ensure!(
        declared == Some(placed_bytes),
        "NTFS stream {what} places {placed_bytes} bytes in clusters, not the size it declares"
    );
    Ok(Data::NonResident(Stream {
        extents,
        size: first.size,
        initialized: first.initialized,
        lznt1_unit_log: lznt1.then_some(u32::from(first.unit)),
    }))
}

struct StreamReader<'s> {
    stream: &'s Stream,
    cluster_log: u32,
    position: u64,
    unit: Vec<u8>,
    cached: Option<u64>,
    packed: Vec<u8>,
}

impl<'s> StreamReader<'s> {
    fn new(stream: &'s Stream, geometry: Geometry) -> Self {
        StreamReader {
            stream,
            cluster_log: geometry.cluster_log,
            position: 0,
            unit: Vec::new(),
            cached: None,
            packed: Vec::new(),
        }
    }

    fn extent_index(&self, vcn: u64) -> usize {
        self.stream
            .extents
            .partition_point(|extent| extent.vcn <= vcn)
            - 1
    }

    fn read(&mut self, disk: &mut Disk, buffer: &mut [u8]) -> Result<usize> {
        let stream = self.stream;
        let position = self.position;
        if position >= stream.size || buffer.is_empty() {
            return Ok(0);
        }
        let mut wanted = (buffer.len() as u64).min(stream.size - position) as usize;
        if position >= stream.initialized {
            buffer[..wanted].fill(0);
            self.position += wanted as u64;
            return Ok(wanted);
        }
        wanted = (wanted as u64).min(stream.initialized - position) as usize;
        let log = self.cluster_log;
        let vcn = position >> log;
        let count = match stream.lznt1_unit_log {
            Some(unit_log) => self.read_unit(disk, &mut buffer[..wanted], vcn, unit_log)?,
            None => self.read_raw(disk, &mut buffer[..wanted], vcn)?,
        };
        self.position += count as u64;
        Ok(count)
    }

    fn read_raw(&mut self, disk: &mut Disk, buffer: &mut [u8], vcn: u64) -> Result<usize> {
        let log = self.cluster_log;
        let index = self.extent_index(vcn);
        let extent = self.stream.extents[index];
        let extent_end = self.stream.extents[index + 1].vcn << log;
        let count = (buffer.len() as u64).min(extent_end - self.position) as usize;
        match extent.lcn {
            Some(lcn) => disk.read_exact_at(
                (lcn << log) + self.position - (extent.vcn << log),
                &mut buffer[..count],
            )?,
            None => buffer[..count].fill(0),
        }
        Ok(count)
    }

    // EXTERNAL(7-Zip NtfsHandler CInStream::Read): a unit with an extent without a cluster, the
    // end of the allocation counting as one, is decoded from the clusters before the hole. 7-Zip
    // decodes it for any compression unit; Zipper only when the compression flag says LZNT1.
    fn read_unit(
        &mut self,
        disk: &mut Disk,
        buffer: &mut [u8],
        vcn: u64,
        unit_log: u32,
    ) -> Result<usize> {
        let log = self.cluster_log;
        let unit_clusters = 1u64 << unit_log;
        let unit_start = vcn & !(unit_clusters - 1);
        let unit_end = unit_start + unit_clusters;
        let left = self.extent_index(unit_start);
        let overlapping = self.stream.extents[left..]
            .iter()
            .take_while(|extent| extent.vcn < unit_end);
        if !overlapping.clone().any(|extent| extent.lcn.is_none()) {
            let unit_left = ((unit_end << log) - self.position) as usize;
            let count = buffer.len().min(unit_left);
            return self.read_raw(disk, &mut buffer[..count], vcn);
        }
        if overlapping.clone().all(|extent| extent.lcn.is_none()) {
            let index = self.extent_index(vcn);
            let hole_end = self.stream.extents[index + 1].vcn << log;
            let count = (buffer.len() as u64).min(hole_end - self.position) as usize;
            buffer[..count].fill(0);
            return Ok(count);
        }
        if self.cached != Some(unit_start) {
            self.decode_unit(disk, left, unit_start, unit_end)?;
        }
        let offset = (self.position - (unit_start << log)) as usize;
        let count = buffer.len().min(self.unit.len() - offset);
        buffer[..count].copy_from_slice(&self.unit[offset..offset + count]);
        Ok(count)
    }

    fn decode_unit(
        &mut self,
        disk: &mut Disk,
        left: usize,
        unit_start: u64,
        unit_end: u64,
    ) -> Result<()> {
        let log = self.cluster_log;
        let extents = &self.stream.extents;
        self.cached = None;
        self.packed.clear();
        let mut vcn = unit_start;
        for index in left..extents.len() - 1 {
            let extent = extents[index];
            let Some(lcn) = extent.lcn else { break };
            ensure!(
                extent.vcn < unit_end,
                "NTFS compressed unit has clusters past its end"
            );
            let count = extents[index + 1].vcn.min(unit_end) - vcn;
            let start = self.packed.len();
            self.packed.resize(start + (count << log) as usize, 0);
            disk.read_exact_at((lcn + vcn - extent.vcn) << log, &mut self.packed[start..])?;
            vcn += count;
        }
        let unit_bytes = ((unit_end - unit_start) << log) as usize;
        self.unit.clear();
        self.unit.resize(unit_bytes, 0);
        let wanted = (self.stream.size - (unit_start << log)).min(unit_bytes as u64) as usize;
        let written = lznt1::decode_whole_chunks(&mut self.unit, wanted, &self.packed);
        ensure!(
            written >= wanted,
            "NTFS compressed unit decodes to {written} bytes, short of {wanted}"
        );
        self.cached = Some(unit_start);
        Ok(())
    }
}

struct DataReader<'d, 'v> {
    disk: &'d mut Disk<'v>,
    data: &'d Data,
    stream: Option<StreamReader<'d>>,
    position: usize,
}

impl<'d, 'v> DataReader<'d, 'v> {
    fn new(disk: &'d mut Disk<'v>, data: &'d Data, geometry: Geometry) -> Self {
        let stream = match data {
            Data::NonResident(stream) => Some(StreamReader::new(stream, geometry)),
            Data::Resident(_) => None,
        };
        DataReader {
            disk,
            data,
            stream,
            position: 0,
        }
    }
}

impl Read for DataReader<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if let Some(stream) = &mut self.stream {
            return stream
                .read(self.disk, buffer)
                .map_err(std::io::Error::other);
        }
        let Data::Resident(bytes) = self.data else {
            return Ok(0);
        };
        let count = (&bytes[self.position.min(bytes.len())..]).read(buffer)?;
        self.position += count;
        Ok(count)
    }
}

enum Parsed {
    Ignored,
    File(RecordHeader),
}

fn located(error: BadField, index: u64) -> anyhow::Error {
    anyhow!("{error} at record {index}")
}

// EXTERNAL(Windows, ntfs-3g): NTFS protects every 512 bytes of a record whatever the sector
// size. 7-Zip expects one entry per sector, and either stride is read here.
fn fixup_stride(record: &[u8], sector_size: usize) -> usize {
    let entries = usize::from(u16_at(record, 6));
    if entries == record.len() / FIXUP_STRIDE + 1 {
        FIXUP_STRIDE
    } else {
        sector_size
    }
}

fn parse_record(record: &mut [u8], sector_size: usize, index: u64) -> Result<Parsed> {
    let magic = u32_at(record, 0);
    if magic != FILE_MAGIC {
        ensure!(
            magic_is_ignored(magic),
            "NTFS record {index} has an unknown signature {magic:#010x}"
        );
        return Ok(Parsed::Ignored);
    }
    apply_fixups(record, fixup_stride(record, sector_size), index)
        .map_err(|error| located(error, index))?;
    let header = record_header(record).map_err(|error| located(error, index))?;
    Ok(Parsed::File(header))
}

// EXTERNAL(7-Zip NtfsHandler CMftRec::Parse): the attributes end with the end marker exactly at
// the record's bytes in use.
fn walk_attributes(
    record: &[u8],
    header: &RecordHeader,
    index: u64,
    mut visit: impl FnMut(&Attribute, &[u8]) -> Result<()>,
) -> Result<()> {
    let limit = header.attributes.end;
    let mut at = header.attributes.start;
    loop {
        ensure!(
            at < limit,
            "NTFS record {index} has no end marker inside its bytes in use"
        );
        let bytes = &record[at..limit];
        match attribute(bytes).map_err(|error| located(error, index))? {
            None => {
                ensure!(
                    bytes.len() == END_LENGTH,
                    "NTFS record {index} has bytes in use after its end marker"
                );
                return Ok(());
            }
            Some(found) => {
                visit(&found, &bytes[..found.length])?;
                at += found.length;
            }
        }
    }
}

fn resident<'b>(attribute: &Attribute, bytes: &'b [u8]) -> Option<&'b [u8]> {
    match &attribute.value {
        Value::Resident(range) => Some(&bytes[range.clone()]),
        Value::NonResident(..) => None,
    }
}

fn raw_piece(attribute: &Attribute, bytes: &[u8]) -> RawPiece {
    match &attribute.value {
        Value::Resident(range) => RawPiece::Resident(bytes[range.clone()].to_vec()),
        Value::NonResident(header, range) => RawPiece::NonResident(Piece {
            header: *header,
            runs: bytes[range.clone()].to_vec(),
        }),
    }
}

fn attribute_size(attribute: &Attribute) -> u64 {
    match &attribute.value {
        Value::Resident(range) => range.len() as u64,
        Value::NonResident(header, _) => header.size,
    }
}

fn read_record(
    disk: &mut Disk,
    geometry: Geometry,
    mft: &Data,
    index: u64,
    record: &mut [u8],
) -> Result<()> {
    let offset = index << geometry.record_log;
    match mft {
        Data::Resident(bytes) => {
            let start = offset as usize;
            record.copy_from_slice(&bytes[start..start + record.len()]);
        }
        Data::NonResident(stream) => {
            let mut reader = StreamReader::new(stream, geometry);
            reader.position = offset;
            let mut done = 0;
            while done < record.len() {
                let count = reader.read(disk, &mut record[done..])?;
                ensure!(count > 0, "NTFS MFT ends inside record {index}");
                done += count;
            }
        }
    }
    Ok(())
}

fn open_mft(disk: &mut Disk, boot: Boot) -> Result<(Geometry, Data, u64)> {
    let volume_bytes = boot.clusters << boot.cluster_log;
    ensure!(
        disk.length >= volume_bytes,
        "NTFS volume is {} bytes, short of the {volume_bytes} its boot sector declares",
        disk.length
    );
    ensure!(
        boot.mft_cluster < boot.clusters,
        "NTFS MFT starts outside the volume"
    );
    let mut probe = vec![0u8; MFT_PROBE];
    disk.read_exact_at(boot.mft_cluster << boot.cluster_log, &mut probe)
        .context("cannot read the NTFS MFT")?;
    let record_log = exact_log(u32_at(&probe, 0x1C))
        .filter(|log| (boot.sector_log..=LARGEST_RECORD_LOG).contains(log))
        .context("NTFS MFT record size is not a power of two from the sector size to 4 KiB")?;
    let geometry = Geometry {
        cluster_log: boot.cluster_log,
        clusters: boot.clusters,
        sector_size: 1 << boot.sector_log,
        record_log,
    };
    let record = &mut probe[..1 << record_log];
    let Parsed::File(header) = parse_record(record, geometry.sector_size, 0)? else {
        bail!("NTFS MFT record 0 is not a file record");
    };
    let mut unnamed = Vec::new();
    walk_attributes(record, &header, 0, |attribute, bytes| {
        if attribute.kind == DATA && stream_name(&bytes[attribute.name.clone()]).is_empty() {
            unnamed.push(raw_piece(attribute, bytes));
        }
        Ok(())
    })?;
    ensure!(!unnamed.is_empty(), "NTFS MFT record 0 has no data");
    let mft = build_stream(unnamed, geometry, "$MFT")?;
    let size = mft.size();
    ensure!(
        size >> 4 <= volume_bytes,
        "NTFS MFT is larger than its volume allows"
    );
    let records = size >> record_log;
    ensure!(
        records <= MAX_RECORDS,
        "NTFS MFT holds more than {MAX_RECORDS} records"
    );
    Ok((geometry, mft, records))
}

const FILE_RECORD: u8 = 0x1;
const RECORD_IN_USE: u8 = 0x2;
const RECORD_IS_DIRECTORY: u8 = 0x4;
const EXTENSION: u8 = 0x8;
const NO_SLOT: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct RecordState {
    slot: u32,
    sequence: u16,
    flags: u8,
}

const UNNAMED_DATA: u8 = 0x1;
const NAMED_DATA: u8 = 0x2;
const BAD_CLUSTERS_DATA: u8 = 0x4;
const NONZERO_DATA: u8 = 0x8;
const HAS_REPARSE: u8 = 0x10;

struct Info {
    record: u32,
    standard: Standard,
    data: u8,
    first_item: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelling {
    Valid,
    NotUtf16,
    HasNul,
}

struct Name {
    owner: u32,
    source: u32,
    parent: u64,
    kind: u8,
    text: Range<u32>,
    spelling: Spelling,
}

const NO_ITEM: u32 = u32::MAX;
const NO_NAME: u32 = u32::MAX;

struct Item {
    record: u32,
    name: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Parent {
    Top,
    System,
    Lost,
    Folder(u32),
}

struct Ntfs<'v> {
    disk: Disk<'v>,
    geometry: Geometry,
    mft: Data,
    states: Vec<RecordState>,
    infos: Vec<Info>,
    names: Vec<Name>,
    text: String,
    extensions: Vec<(u32, u32)>,
    items: Vec<Item>,
}

impl<'v> Ntfs<'v> {
    fn open(volume: &'v mut dyn ReadSeek) -> Result<Self> {
        let length = volume.seek(SeekFrom::End(0))?;
        let mut disk = Disk { volume, length };
        let mut bytes = [0u8; BOOT_SECTOR];
        disk.read_exact_at(0, &mut bytes)
            .context("NTFS volume is shorter than its boot sector")?;
        let boot = boot_sector(&bytes).map_err(|error| anyhow!("{error}"))?;
        let (geometry, mft, records) = open_mft(&mut disk, boot)?;
        let mut ntfs = Ntfs {
            disk,
            geometry,
            mft,
            states: Vec::new(),
            infos: Vec::new(),
            names: Vec::new(),
            text: String::new(),
            extensions: Vec::new(),
            items: Vec::new(),
        };
        let pending = ntfs.scan(records)?;
        ntfs.merge_extensions(pending)?;
        ntfs.list_items()?;
        Ok(ntfs)
    }

    fn record_size(&self) -> usize {
        1 << self.geometry.record_log
    }

    fn read_parsed(&mut self, index: u64, record: &mut [u8]) -> Result<Parsed> {
        read_record(&mut self.disk, self.geometry, &self.mft, index, record)?;
        parse_record(record, self.geometry.sector_size, index)
    }

    fn scan(&mut self, records: u64) -> Result<Vec<(u32, u64)>> {
        let mut record = vec![0u8; self.record_size()];
        let mut pending = Vec::new();
        self.states.reserve(records as usize);
        for index in 0..records {
            let header = match self.read_parsed(index, &mut record)? {
                Parsed::Ignored => {
                    self.states.push(RecordState {
                        slot: NO_SLOT,
                        sequence: 0,
                        flags: 0,
                    });
                    continue;
                }
                Parsed::File(header) => header,
            };
            let in_use = header.flags & IN_USE != 0;
            let mut flags = FILE_RECORD;
            if in_use {
                flags |= RECORD_IN_USE;
            }
            if header.flags & IS_DIRECTORY != 0 {
                flags |= RECORD_IS_DIRECTORY;
            }
            if header.base != 0 {
                flags |= EXTENSION;
                ensure!(
                    header.base & REFERENCE_INDEX < records,
                    "NTFS record {index} extends record {}, which the MFT does not hold",
                    header.base & REFERENCE_INDEX
                );
                if in_use {
                    pending.push((index as u32, header.base));
                }
            }
            let slot = self.scan_attributes(&record, &header, index, in_use)?;
            self.states.push(RecordState {
                slot,
                sequence: header.sequence,
                flags,
            });
        }
        Ok(pending)
    }

    fn scan_attributes(
        &mut self,
        record: &[u8],
        header: &RecordHeader,
        index: u64,
        in_use: bool,
    ) -> Result<u32> {
        let mut standard = Standard::default();
        let mut data = 0u8;
        let first_name = self.names.len();
        let first_text = self.text.len();
        let names = &mut self.names;
        let text = &mut self.text;
        walk_attributes(record, header, index, |attribute, bytes| {
            match attribute.kind {
                FILE_NAME => {
                    let value = resident(attribute, bytes).with_context(|| {
                        format!("NTFS record {index} keeps a file name outside the record")
                    })?;
                    let fields = file_name(value).map_err(|error| located(error, index))?;
                    let raw = &value[fields.name.clone()];
                    let start = text.len() as u32;
                    let spelling = match utf16(raw) {
                        None => Spelling::NotUtf16,
                        Some(decoded) if decoded.contains('\0') => Spelling::HasNul,
                        Some(decoded) => {
                            text.extend(decoded.chars().map(|c| match c {
                                '/' | '\\' => '_',
                                other => other,
                            }));
                            Spelling::Valid
                        }
                    };
                    ensure!(
                        text.len() <= MAX_NAME_BYTES,
                        "NTFS names exceed {MAX_NAME_BYTES} bytes"
                    );
                    names.push(Name {
                        owner: index as u32,
                        source: index as u32,
                        parent: fields.parent,
                        kind: fields.kind,
                        text: start..text.len() as u32,
                        spelling,
                    });
                }
                STANDARD_INFORMATION => {
                    let value = resident(attribute, bytes).with_context(|| {
                        format!("NTFS record {index} keeps standard information outside the record")
                    })?;
                    standard =
                        standard_information(value).map_err(|error| located(error, index))?;
                }
                DATA => {
                    let name = stream_name(&bytes[attribute.name.clone()]);
                    data |= if name.is_empty() {
                        UNNAMED_DATA
                    } else if names_bad_clusters(&name) {
                        BAD_CLUSTERS_DATA
                    } else {
                        NAMED_DATA
                    };
                    if attribute_size(attribute) != 0 {
                        data |= NONZERO_DATA;
                    }
                }
                REPARSE_POINT => data |= HAS_REPARSE,
                _ => {}
            }
            Ok(())
        })?;
        if !in_use {
            self.names.truncate(first_name);
            self.text.truncate(first_text);
            return Ok(NO_SLOT);
        }
        self.infos.push(Info {
            record: index as u32,
            standard,
            data,
            first_item: NO_ITEM,
        });
        Ok((self.infos.len() - 1) as u32)
    }

    // EXTERNAL(7-Zip NtfsHandler CDatabase::Open): an extension record in use joins its base
    // record when the base is in use, has the sequence number it names and is no extension
    // itself; when both are in use and it does not, the volume is refused.
    fn merge_extensions(&mut self, pending: Vec<(u32, u64)>) -> Result<()> {
        let mut moved = HashMap::new();
        for (index, base) in pending {
            let target = (base & REFERENCE_INDEX) as u32;
            let state = self.states[target as usize];
            if state.flags & FILE_RECORD == 0 || state.flags & RECORD_IN_USE == 0 {
                continue;
            }
            ensure!(
                state.sequence == (base >> SEQUENCE_SHIFT) as u16 && state.flags & EXTENSION == 0,
                "NTFS record {index} extends record {target}, which does not take it"
            );
            let from = self.states[index as usize].slot;
            let into = state.slot;
            self.infos[into as usize].data |= self.infos[from as usize].data & !HAS_REPARSE;
            moved.insert(index, target);
            self.extensions.push((target, index));
        }
        for name in &mut self.names {
            if let Some(&target) = moved.get(&name.owner) {
                name.owner = target;
            }
        }
        self.names
            .sort_by_key(|name| (name.owner, name.owner != name.source));
        self.extensions.sort_unstable();
        Ok(())
    }

    fn names_of(&self, record: u32) -> Range<u32> {
        let start = self.names.partition_point(|name| name.owner < record);
        let end = start
            + self.names[start..]
                .iter()
                .take_while(|name| name.owner == record)
                .count();
        start as u32..end as u32
    }

    fn is_base_in_use(&self, index: usize) -> bool {
        let flags = self.states[index].flags;
        flags & (FILE_RECORD | RECORD_IN_USE | EXTENSION) == FILE_RECORD | RECORD_IN_USE
    }

    // EXTERNAL(7-Zip NtfsHandler CDatabase::Open): a DOS name is left out when a Win32 name has
    // the same parent. A record in use without names is shown as `[NONAME]-N` in `[LOST]`, or
    // not at all when it is a system record holding no data.
    fn list_items(&mut self) -> Result<()> {
        for slot in 0..self.infos.len() {
            let record = self.infos[slot].record;
            if !self.is_base_in_use(record as usize) {
                continue;
            }
            let names = self.names_of(record);
            if names.is_empty() {
                if record >= SYSTEM_RECORDS || self.infos[slot].data & NONZERO_DATA != 0 {
                    self.push_item(slot, record, NO_NAME)?;
                }
                continue;
            }
            for index in names.clone() {
                let name = &self.names[index as usize];
                let shadowed = name.kind == DOS_NAME
                    && self.names[names.start as usize..names.end as usize]
                        .iter()
                        .any(|other| other.kind == WIN32_NAME && other.parent == name.parent);
                if !shadowed {
                    self.push_item(slot, record, index)?;
                }
            }
        }
        Ok(())
    }

    fn push_item(&mut self, slot: usize, record: u32, name: u32) -> Result<()> {
        ensure!(
            self.items.len() < tree::MAX_ENTRIES,
            "NTFS volume has more than {} entries",
            tree::MAX_ENTRIES
        );
        let info = &mut self.infos[slot];
        if info.first_item == NO_ITEM {
            info.first_item = self.items.len() as u32;
        } else {
            ensure!(
                self.states[record as usize].flags & RECORD_IS_DIRECTORY == 0,
                "NTFS folder record {record} has more than one name"
            );
        }
        self.items.push(Item { record, name });
        Ok(())
    }

    fn info(&self, record: u32) -> &Info {
        &self.infos[self.states[record as usize].slot as usize]
    }

    fn is_directory(&self, record: u32) -> bool {
        self.states[record as usize].flags & RECORD_IS_DIRECTORY != 0
    }

    fn name_text(&self, item: &Item) -> String {
        match item.name {
            NO_NAME => format!("[NONAME]-{}", item.record),
            index => {
                let name = &self.names[index as usize];
                self.text[name.text.start as usize..name.text.end as usize].to_owned()
            }
        }
    }

    // EXTERNAL(7-Zip NtfsHandler CDatabase::Open): a system record sits in `[SYSTEM]`, a child
    // of the root at the top, and any other in the folder record its name names when that
    // record is a folder in use with the same sequence number, or in `[LOST]`.
    fn parent(&self, item: &Item) -> Parent {
        if item.record < SYSTEM_RECORDS {
            return Parent::System;
        }
        self.named_parent(item)
    }

    // EXTERNAL(7-Zip NtfsHandler CDatabase::Open): 7-Zip makes `[LOST]` when any name's folder
    // is missing, a system record's name too, though a system record stays in `[SYSTEM]`.
    fn named_parent(&self, item: &Item) -> Parent {
        if item.name == NO_NAME {
            return Parent::Lost;
        }
        let reference = self.names[item.name as usize].parent;
        let index = reference & REFERENCE_INDEX;
        if index == ROOT_RECORD {
            return Parent::Top;
        }
        if index >= self.states.len() as u64 {
            return Parent::Lost;
        }
        let state = self.states[index as usize];
        let shown = state.slot != NO_SLOT
            && self.is_base_in_use(index as usize)
            && self.infos[state.slot as usize].first_item != NO_ITEM;
        if state.flags & RECORD_IS_DIRECTORY != 0
            && shown
            && state.sequence == (reference >> SEQUENCE_SHIFT) as u16
        {
            Parent::Folder(index as u32)
        } else {
            Parent::Lost
        }
    }

    fn checked_name(&self, item: &Item) -> Result<String> {
        if item.name != NO_NAME {
            let name = &self.names[item.name as usize];
            match name.spelling {
                Spelling::Valid => {}
                Spelling::NotUtf16 => {
                    bail!(
                        "NTFS record {} has a name that is not valid UTF-16; {BACKEND_HINT}",
                        item.record
                    )
                }
                Spelling::HasNul => {
                    bail!("NTFS record {} has a name with a NUL in it", item.record)
                }
            }
        }
        let text = self.name_text(item);
        ensure!(
            !text.is_empty(),
            "NTFS record {} has an empty name",
            item.record
        );
        Ok(text)
    }
}

struct Folder {
    parent: Parent,
    name: String,
    depth: usize,
}

#[derive(Default)]
struct Layout {
    folders: HashMap<u32, Folder>,
    lost: bool,
}

impl Layout {
    fn path(&self, parent: Parent, name: &str) -> String {
        let mut parts = vec![name];
        let mut next = parent;
        while let Parent::Folder(record) = next {
            let folder = &self.folders[&record];
            parts.push(&folder.name);
            next = folder.parent;
        }
        match next {
            Parent::System => parts.push(SYSTEM_FOLDER),
            Parent::Lost => parts.push(LOST_FOLDER),
            Parent::Top | Parent::Folder(_) => {}
        }
        parts.reverse();
        parts.join("/")
    }

    fn top(&self, mut parent: Parent) -> Parent {
        while let Parent::Folder(record) = parent {
            parent = self.folders[&record].parent;
        }
        parent
    }

    fn folders_above(&self, parent: Parent) -> usize {
        let nested = match parent {
            Parent::Folder(record) => self.folders[&record].depth + 1,
            _ => 0,
        };
        nested + usize::from(self.top(parent) != Parent::Top)
    }
}

fn file_mode(attributes: u32) -> u32 {
    if attributes & READ_ONLY != 0 {
        READ_ONLY_MODE
    } else {
        FILE_MODE
    }
}

fn symlink_target(tag: u32, data: &[u8], path: &str, depth: usize) -> Result<String> {
    let unsupported = || {
        anyhow!(
            "NTFS entry {path:?} is a reparse point of tag {tag:#010x}, which Zipper does not restore; {BACKEND_HINT}"
        )
    };
    let damaged = || anyhow!("NTFS link {path:?} has damaged reparse data");
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
                offset.is_multiple_of(2)
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
        "NTFS link {path:?} points to {target:?}, outside the volume"
    );
    let mut parts: Vec<&str> = Vec::new();
    for part in rooted.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(
                    parts.pop().is_some(),
                    "NTFS link {path:?} points outside the volume"
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Folder,
    File,
    Link,
}

struct Plan {
    item: u32,
    record: u32,
    parent: Parent,
    kind: Kind,
    folders_above: u16,
    root_folder: bool,
}

fn ensure_no_shared_clusters(mut spans: Vec<(u64, u64)>) -> Result<()> {
    spans.sort_unstable();
    for pair in spans.windows(2) {
        ensure!(
            pair[0].0 + pair[0].1 <= pair[1].0,
            "NTFS volume has two streams that share cluster {}",
            pair[1].0
        );
    }
    Ok(())
}

fn name_key(keys: &std::hash::RandomState, parent: Parent, name: &str) -> u64 {
    std::hash::BuildHasher::hash_one(keys, (parent, name))
}

struct Writer<'o> {
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    prefix: &'o str,
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }
}

impl Ntfs<'_> {
    fn layout(&self) -> Result<Layout> {
        let mut layout = Layout::default();
        for item in &self.items {
            let parent = self.parent(item);
            layout.lost |= self.named_parent(item) == Parent::Lost;
            if self.is_directory(item.record) {
                layout.folders.insert(
                    item.record,
                    Folder {
                        parent,
                        name: self.checked_name(item)?,
                        depth: 0,
                    },
                );
            }
        }
        let mut depths: HashMap<u32, usize> = HashMap::new();
        let records: Vec<u32> = layout.folders.keys().copied().collect();
        for record in records {
            let mut chain = Vec::new();
            let mut next = record;
            let base = loop {
                if let Some(&depth) = depths.get(&next) {
                    break depth + 1;
                }
                ensure!(
                    !chain.contains(&next),
                    "NTFS folder record {record} lies inside itself"
                );
                ensure!(
                    chain.len() <= MAX_DEPTH,
                    "NTFS folder record {record} lies more than {MAX_DEPTH} folders deep"
                );
                chain.push(next);
                match layout.folders[&next].parent {
                    Parent::Folder(parent) => next = parent,
                    _ => break 0,
                }
            };
            for (offset, &folder) in chain.iter().rev().enumerate() {
                ensure!(
                    base + offset <= MAX_DEPTH,
                    "NTFS folder record {record} lies more than {MAX_DEPTH} folders deep"
                );
                depths.insert(folder, base + offset);
            }
        }
        for (record, folder) in &mut layout.folders {
            folder.depth = depths[record];
        }
        Ok(layout)
    }

    fn has_alternate_streams(&self, record: u32) -> bool {
        let data = self.info(record).data;
        data & NAMED_DATA != 0
            || data & BAD_CLUSTERS_DATA != 0 && record != BAD_CLUSTERS_RECORD
            || self.is_directory(record) && data & UNNAMED_DATA != 0
    }

    // EXTERNAL(7-Zip NtfsHandler GetItemPath): the root folder, the system record named `.`,
    // is `[SYSTEM]` itself.
    fn path(&self, layout: &Layout, plan: &Plan, prefix: &str) -> String {
        if plan.root_folder {
            return format!("{prefix}{SYSTEM_FOLDER}");
        }
        let item = &self.items[plan.item as usize];
        let path = match layout.folders.get(&item.record) {
            Some(folder) => layout.path(plan.parent, &folder.name),
            None => layout.path(plan.parent, &self.name_text(item)),
        };
        format!("{prefix}{path}")
    }

    fn plan(&self, layout: &Layout) -> Result<Vec<Plan>> {
        let keys = std::hash::RandomState::new();
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            let parent = self.parent(item);
            let name = match layout.folders.get(&item.record) {
                Some(folder) => folder.name.clone(),
                None => self.checked_name(item)?,
            };
            let root_folder = item.record < SYSTEM_RECORDS && name == ".";
            let path = if root_folder {
                SYSTEM_FOLDER.to_owned()
            } else {
                ensure!(
                    name != "." && name != "..",
                    "unsafe archive path: NTFS entry named {name:?}"
                );
                ensure!(
                    parent != Parent::Top
                        || name != SYSTEM_FOLDER && !(layout.lost && name == LOST_FOLDER),
                    "NTFS entry {name:?} has the name of a folder Zipper makes, as 7-Zip does"
                );
                ensure!(
                    seen.insert(name_key(&keys, parent, &name)),
                    "NTFS folder {:?} holds two entries named {name:?}",
                    layout.path(parent, "")
                );
                layout.path(parent, &name)
            };
            if let Parent::Folder(folder) = parent {
                ensure!(
                    layout.folders[&folder].depth < MAX_DEPTH,
                    "NTFS entry {path:?} lies more than {MAX_DEPTH} folders deep"
                );
                ensure!(
                    self.info(folder).data & HAS_REPARSE == 0,
                    "NTFS link {:?} has entries under it",
                    layout.path(
                        layout.folders[&folder].parent,
                        &layout.folders[&folder].name
                    )
                );
            }
            let info = self.info(item.record);
            let system = item.record < SYSTEM_RECORDS || layout.top(parent) == Parent::System;
            ensure!(
                system || !self.has_alternate_streams(item.record),
                "NTFS entry {path:?} has an alternate data stream, which Zipper does not write; {BACKEND_HINT}"
            );
            let reparse = info.data & HAS_REPARSE != 0;
            ensure!(
                !reparse || item.record >= SYSTEM_RECORDS,
                "NTFS system record {} is a reparse point",
                item.record
            );
            let kind = match (reparse, self.is_directory(item.record)) {
                (true, _) => Kind::Link,
                (false, true) => Kind::Folder,
                (false, false) => Kind::File,
            };
            ensure!(
                kind != Kind::File || info.standard.attributes & ENCRYPTED == 0,
                "NTFS file {path:?} is encrypted, which Zipper does not read; {BACKEND_HINT}"
            );
            plans.push(Plan {
                item: index as u32,
                record: item.record,
                parent,
                kind,
                folders_above: layout.folders_above(parent) as u16,
                root_folder,
            });
        }
        Ok(plans)
    }

    fn file_data(&mut self, record: u32, path: &str) -> Result<Data> {
        if self.info(record).data & UNNAMED_DATA == 0 {
            return Ok(Data::Resident(Vec::new()));
        }
        let mut pieces = Vec::new();
        let mut bytes = vec![0u8; self.record_size()];
        let first = self.extensions.partition_point(|&(base, _)| base < record);
        let extensions: Vec<u32> = self.extensions[first..]
            .iter()
            .take_while(|&&(base, _)| base == record)
            .map(|&(_, extension)| extension)
            .collect();
        for index in std::iter::once(record).chain(extensions) {
            let Parsed::File(header) = self.read_parsed(u64::from(index), &mut bytes)? else {
                bail!("NTFS record {index} changed while it was read");
            };
            walk_attributes(&bytes, &header, u64::from(index), |attribute, value| {
                if attribute.kind == DATA && stream_name(&value[attribute.name.clone()]).is_empty()
                {
                    pieces.push(raw_piece(attribute, value));
                }
                Ok(())
            })?;
        }
        build_stream(pieces, self.geometry, &format!("{path:?}"))
    }

    fn reparse_data(&mut self, record: u32, path: &str) -> Result<Vec<u8>> {
        let mut bytes = vec![0u8; self.record_size()];
        let Parsed::File(header) = self.read_parsed(u64::from(record), &mut bytes)? else {
            bail!("NTFS record {record} changed while it was read");
        };
        let mut found = None;
        walk_attributes(&bytes, &header, u64::from(record), |attribute, value| {
            if attribute.kind == REPARSE_POINT {
                found = Some(resident(attribute, value).map(<[u8]>::to_vec));
            }
            Ok(())
        })?;
        let data = found.flatten().with_context(|| {
            format!("NTFS link {path:?} keeps its reparse data outside its record")
        })?;
        ensure!(
            data.len() >= REPARSE_HEAD && data.len() <= MAX_REPARSE,
            "NTFS link {path:?} has damaged reparse data"
        );
        Ok(data)
    }

    fn ensure_streams_apart(&mut self, layout: &Layout, plans: &[Plan]) -> Result<()> {
        let mut spans = Vec::new();
        let mut last = None;
        for plan in plans {
            if plan.kind != Kind::File || last == Some(plan.record) {
                continue;
            }
            last = Some(plan.record);
            let path = self.path(layout, plan, "");
            let data = self.file_data(plan.record, &path)?;
            spans.extend(data.placed_clusters());
            ensure!(
                spans.len() <= MAX_RUNS,
                "NTFS volume has more than {MAX_RUNS} runs"
            );
        }
        ensure_no_shared_clusters(spans)
    }

    fn write(&mut self, tree: &mut tree::Tree, writer: &Writer) -> Result<()> {
        let layout = self.layout()?;
        let plans = self.plan(&layout)?;
        self.ensure_streams_apart(&layout, &plans)?;
        tree.check()?;
        let prefix = writer.prefix;
        if !plans.iter().any(|plan| plan.root_folder) {
            tree.directory(&format!("{prefix}{SYSTEM_FOLDER}"), DIRECTORY_MODE, None)?;
        }
        if layout.lost {
            tree.directory(&format!("{prefix}{LOST_FOLDER}"), DIRECTORY_MODE, None)?;
        }
        let mut folders: Vec<&Plan> = plans
            .iter()
            .filter(|plan| plan.kind == Kind::Folder)
            .collect();
        folders.sort_by_key(|plan| plan.folders_above);
        for plan in folders {
            tree.check()?;
            let time = modified(self.info(plan.record).standard.modified);
            tree.directory(&self.path(&layout, plan, prefix), DIRECTORY_MODE, time)?;
        }
        let mut start = 0;
        while start < plans.len() {
            let record = plans[start].record;
            let end = start
                + plans[start..]
                    .iter()
                    .take_while(|plan| plan.record == record)
                    .count();
            let group: Vec<(String, u16)> = plans[start..end]
                .iter()
                .map(|plan| (self.path(&layout, plan, prefix), plan.folders_above))
                .collect();
            match plans[start].kind {
                Kind::Folder => {}
                Kind::Link => {
                    for (path, folders_above) in &group {
                        tree.check()?;
                        let data = self.reparse_data(record, path)?;
                        let target = symlink_target(
                            u32_at(&data, 0),
                            &data[REPARSE_HEAD..],
                            path,
                            usize::from(*folders_above),
                        )?;
                        tree.symlink(path, target)?;
                    }
                }
                Kind::File => self.write_file(tree, writer, record, &group)?,
            }
            start = end;
        }
        Ok(())
    }

    fn write_file(
        &mut self,
        tree: &mut tree::Tree,
        writer: &Writer,
        record: u32,
        group: &[(String, u16)],
    ) -> Result<()> {
        let standard = self.info(record).standard;
        let data = self.file_data(record, &group[0].0)?;
        let geometry = self.geometry;
        let mut leader: Option<&str> = None;
        for (path, _) in group {
            tree.check()?;
            if let Some(leader) = leader
                && writer.writes(path)
            {
                tree.hard_link(path, leader)?;
                continue;
            }
            let mut reader = DataReader::new(&mut self.disk, &data, geometry);
            tree.file(
                path,
                data.size(),
                file_mode(standard.attributes),
                modified(standard.modified),
                &mut reader,
            )?;
            if writer.writes(path) {
                leader = Some(path);
            }
        }
        Ok(())
    }
}

pub(crate) fn is_boot_sector(bytes: &[u8; BOOT_SECTOR]) -> bool {
    boot_sector(bytes).is_ok()
}

/// Returns whether the file starts with an NTFS boot sector that 7-Zip
/// would open.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut bytes = [0u8; BOOT_SECTOR];
    let mut file = crate::volumes::open(path)?;
    let mut read = 0;
    while read < BOOT_SECTOR {
        let count = file.read(&mut bytes[read..])?;
        if count == 0 {
            return Ok(false);
        }
        read += count;
    }
    Ok(is_boot_sector(&bytes))
}

#[allow(dead_code)]
pub(crate) fn read_partition_into(
    tree: &mut tree::Tree,
    volume: &mut (impl Read + Seek),
    options: ReadOptions,
    prefix: &str,
) -> Result<()> {
    let mut ntfs = Ntfs::open(volume)?;
    let writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        prefix,
    };
    ntfs.write(tree, &writer)
}

/// Verify and optionally extract the NTFS volume that `volume` holds, from
/// its first byte to the end it seeks to, into a new directory. `archive`
/// names the file the volume comes from, for the destination checks. It
/// writes the tree `7zz x` writes: system files in `[SYSTEM]`, files whose
/// folder is missing in `[LOST]`, hard links linked, LZNT1 and sparse data
/// decoded, and links that stay inside the volume created. Alternate data
/// streams outside `[SYSTEM]` and encrypted files are refused, and every
/// check of `docs/reference/ntfs-reader.md` holds before anything is
/// published.
pub fn read_volume<V: Read + Seek>(
    mut volume: V,
    archive: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut ntfs = Ntfs::open(&mut volume)?;
    let writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        prefix: "",
    };
    tree::extract("NTFS", archive, destination, options, |tree| {
        ntfs.write(tree, &writer)
    })
}

/// Verify and optionally extract a raw NTFS volume image into a new
/// directory, as [`read_volume`] describes.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let file = crate::volumes::open(path)?;
    read_volume(file, path, destination, options)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn encode(runs: &[(u64, Option<u64>)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut previous = 0i128;
        for &(length, lcn) in runs {
            let length_bytes = (1..=8usize)
                .find(|&n| n == 8 || length >> (8 * n) == 0)
                .unwrap();
            let delta = lcn.map(|lcn| i128::from(lcn) - previous);
            let offset_bytes = delta.map_or(0, |delta| {
                (1..=8usize)
                    .find(|&n| {
                        n == 8 || delta >= -(1i128 << (8 * n - 1)) && delta < 1i128 << (8 * n - 1)
                    })
                    .unwrap()
            });
            bytes.push((offset_bytes << 4 | length_bytes) as u8);
            bytes.extend_from_slice(&length.to_le_bytes()[..length_bytes]);
            if let Some(delta) = delta {
                bytes.extend_from_slice(&(delta as i64).to_le_bytes()[..offset_bytes]);
                previous = i128::from(lcn.unwrap());
            }
        }
        bytes
    }

    #[test]
    fn run_lists_read_back_every_run_they_encode() {
        let mut random = Random(0x5EED);
        for _ in 0..20_000 {
            let clusters = 1 + random.next() % (1 << (random.next() % 40));
            let count = random.next() % 6;
            let mut runs = Vec::new();
            let mut vcn = 0u64;
            for _ in 0..count {
                let length = 1 + random.next() % clusters.min(1 << 20);
                let lcn = (!random.next().is_multiple_of(4)).then(|| random.next() % clusters);
                runs.push((length, lcn));
                vcn += length;
            }
            let fits = runs
                .iter()
                .all(|&(length, lcn)| lcn.is_none_or(|lcn| lcn + length <= clusters));
            let sparse = random.next().is_multiple_of(2);
            let has_holes = runs.iter().any(|run| run.1.is_none());
            let mut extents = vec![Extent { vcn: 0, lcn: None }];
            let result = runs_list(&encode(&runs), vcn, clusters, sparse, &mut extents);
            if fits && (sparse || !has_holes) {
                result.unwrap();
                let mut at = 0;
                for (extent, &(length, lcn)) in extents.iter().zip(&runs) {
                    assert_eq!(*extent, Extent { vcn: at, lcn });
                    at += length;
                }
                assert_eq!(extents.last(), Some(&Extent { vcn, lcn: None }));
            } else {
                assert!(result.is_err());
            }
        }
    }

    fn runs_list(
        list: &[u8],
        clusters_in_attribute: u64,
        clusters: u64,
        sparse: bool,
        extents: &mut Vec<Extent>,
    ) -> Field<()> {
        runs(
            list,
            0,
            clusters_in_attribute.wrapping_sub(1),
            clusters,
            sparse,
            extents,
        )
    }

    #[test]
    fn an_empty_attribute_has_a_high_vcn_of_minus_one() {
        let mut extents = vec![Extent { vcn: 0, lcn: None }];
        runs(&[0], 0, u64::MAX, 10, false, &mut extents).unwrap();
        assert_eq!(extents, [Extent { vcn: 0, lcn: None }]);
    }

    #[test]
    fn the_fixup_stride_is_512_bytes_or_the_sector() {
        for (sector, record, entries, stride) in [
            (512, 1024, 3, 512),
            (4096, 4096, 9, 512),
            (4096, 4096, 2, 4096),
            (1024, 1024, 2, 1024),
        ] {
            let mut bytes = vec![0u8; record];
            bytes[6..8].copy_from_slice(&(entries as u16).to_le_bytes());
            assert_eq!(fixup_stride(&bytes, sector), stride);
        }
    }

    #[test]
    fn times_from_1601_on_convert_to_system_times() {
        assert_eq!(
            modified(WINDOWS_TO_UNIX_SECONDS * TICKS_PER_SECOND),
            Some(std::time::UNIX_EPOCH)
        );
        let before = modified((WINDOWS_TO_UNIX_SECONDS - 1) * TICKS_PER_SECOND + 5).unwrap();
        assert_eq!(
            std::time::UNIX_EPOCH.duration_since(before).unwrap(),
            std::time::Duration::new(0, 999_999_500)
        );
        assert!(modified(u64::MAX).is_some());
        assert_eq!(
            modified(0),
            std::time::UNIX_EPOCH
                .checked_sub(std::time::Duration::from_secs(WINDOWS_TO_UNIX_SECONDS))
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn prefix<const N: usize>(bytes: &[u8; N]) -> &[u8] {
        let length: usize = kani::any();
        kani::assume(length <= N);
        &bytes[..length]
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn boot_sectors_never_panic_and_keep_the_volume_inside_62_bits() {
        let bytes: [u8; BOOT_SECTOR] = kani::any();
        if let Ok(boot) = boot_sector(&bytes) {
            assert!((SMALLEST_SECTOR_LOG..=LARGEST_SECTOR_LOG).contains(&boot.sector_log));
            assert!(boot.cluster_log >= boot.sector_log && boot.cluster_log <= LARGEST_CLUSTER_LOG);
            let bytes = u128::from(boot.clusters) << boot.cluster_log;
            assert!(bytes < 1 << SECTOR_COUNT_BITS);
            assert!(bytes == u128::from(boot.clusters << boot.cluster_log));
        }
    }

    fn check_fixups<const N: usize>(stride: usize) {
        let original: [u8; N] = kani::any();
        let mut record = original;
        let index: u64 = kani::any();
        if apply_fixups(&mut record, stride, index).is_ok() {
            let offset = usize::from(u16_at(&original, 4));
            for at in 0..N {
                let sector_end = at % stride >= stride - 2;
                if !sector_end {
                    assert!(record[at] == original[at]);
                } else {
                    let sector = at / stride + 1;
                    let saved = offset + 2 * sector + at % stride - (stride - 2);
                    assert!(saved + 1 < stride);
                    assert!(record[at] == original[saved]);
                }
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(25)]
    fn fixups_never_panic_and_change_only_the_end_of_each_stride() {
        check_fixups::<24>(8);
    }

    #[kani::proof]
    #[kani::unwind(4)]
    fn fixups_with_a_record_number_never_panic() {
        let mut record: [u8; 64] = kani::any();
        let index: u64 = kani::any();
        if apply_fixups(&mut record, 64, index).is_ok()
            && u16_at(&record, 4) as usize >= NUMBERED_FIXUPS
        {
            let number = u32_at(&record, 0x2C);
            assert!(number == index as u32 || number == 0);
        }
    }

    #[kani::proof]
    fn record_headers_never_panic_and_keep_their_attributes_inside() {
        let bytes: [u8; 64] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(header) = record_header(bytes) {
            assert!(header.attributes.start < bytes.len());
            assert!(header.attributes.end <= bytes.len());
            assert!(header.attributes.start % ATTRIBUTE_ALIGN == 0);
            assert!(header.attributes.end % ATTRIBUTE_ALIGN == 0);
        }
    }

    #[kani::proof]
    fn attribute_headers_never_panic_and_stay_inside_their_bytes() {
        let bytes: [u8; 88] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(Some(found)) = attribute(bytes) {
            assert!(found.length <= bytes.len());
            assert!(found.length >= ATTRIBUTE_HEAD && found.length % ATTRIBUTE_ALIGN == 0);
            assert!(found.name.end <= found.length && found.name.len() % 2 == 0);
            let value = match &found.value {
                Value::Resident(range) => range.clone(),
                Value::NonResident(header, range) => {
                    assert!(found.length >= NON_RESIDENT_HEAD);
                    assert!(header.packed.is_some() == (header.unit != 0));
                    range.clone()
                }
            };
            assert!(value.start <= value.end && value.end <= found.length);
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn every_run_stays_inside_its_piece_and_the_volume() {
        let list: [u8; 17] = kani::any();
        let list = prefix(&list);
        let lcn: u64 = kani::any();
        let room: u64 = kani::any();
        let clusters: u64 = kani::any();
        let sparse: bool = kani::any();
        if let Ok(Some(found)) = run(list, lcn, room, clusters, sparse) {
            assert!(found.bytes <= list.len());
            assert!(found.length > 0 && found.length <= room);
            match found.lcn {
                Some(start) => {
                    assert!(u128::from(start) + u128::from(found.length) <= u128::from(clusters))
                }
                None => assert!(sparse),
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn run_lists_never_panic_and_fill_their_piece_exactly() {
        let list: [u8; 4] = kani::any();
        let list = prefix(&list);
        let low_vcn: u64 = kani::any();
        let high_vcn: u64 = kani::any();
        let clusters: u64 = kani::any();
        let sparse: bool = kani::any();
        let mut extents = vec![Extent {
            vcn: low_vcn,
            lcn: None,
        }];
        if runs(list, low_vcn, high_vcn, clusters, sparse, &mut extents).is_ok() {
            let last = extents[extents.len() - 1];
            assert!(last.lcn.is_none() && last.vcn == high_vcn.wrapping_add(1));
            assert!(extents[0].vcn == low_vcn);
            for index in 0..extents.len() - 1 {
                let extent = extents[index];
                let length = extents[index + 1].vcn - extent.vcn;
                assert!(length > 0);
                if let Some(lcn) = extent.lcn {
                    assert!(u128::from(lcn) + u128::from(length) <= u128::from(clusters));
                }
            }
        }
    }

    #[kani::proof]
    fn file_names_never_panic_and_keep_their_name_inside() {
        let bytes: [u8; 0x48] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(fields) = file_name(bytes) {
            assert!(fields.name.start == FILE_NAME_HEAD);
            assert!(fields.name.end <= bytes.len() && fields.name.len() % 2 == 0);
        }
    }

    #[kani::proof]
    fn standard_information_never_panics() {
        let bytes: [u8; 0x30] = kani::any();
        let bytes = prefix(&bytes);
        if standard_information(bytes).is_ok() {
            assert!(bytes.len() >= STANDARD_INFORMATION_HEAD);
        }
    }
}
