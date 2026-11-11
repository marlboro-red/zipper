//! In-process extraction of Android logical partition images (`super.img`),
//! as 7-Zip extracts them: one file for each logical partition. See
//! `docs/reference/lp-reader.md`.
use crate::tree;
pub use crate::tree::ReadOptions;
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, ensure};
use sha2::Digest;
use std::{io::Read, path::Path};

pub(crate) const SECTOR: u64 = 512;
const GEOMETRY_AT: u64 = 4096;
const GEOMETRY_AREA: u64 = 4096;
const METADATA_AT: u64 = GEOMETRY_AT + 2 * GEOMETRY_AREA;
pub(crate) const GEOMETRY: usize = 52;
const GEOMETRY_MAGIC: u32 = 0x616C_4467;
const CHECKSUM: std::ops::Range<usize> = 8..40;
pub(crate) const HEADER: usize = 128;
const HEADER_V1_2: usize = 256;
const HEADER_MAGIC: u32 = 0x414C_5030;
const MAJOR_VERSION: u16 = 10;
const HEADER_CHECKSUM: std::ops::Range<usize> = 12..44;
const TABLES_CHECKSUM: std::ops::Range<usize> = 48..80;
const DESCRIPTORS_AT: usize = 80;
const DESCRIPTOR: usize = 12;
pub(crate) const PARTITION: usize = 52;
pub(crate) const EXTENT: usize = 24;
const GROUP: usize = 48;
const BLOCK_DEVICE: usize = 64;
const ENTRY_SIZES: [usize; 4] = [PARTITION, EXTENT, GROUP, BLOCK_DEVICE];
const NAME: usize = 36;
const STRUCT_SIZE_AT: usize = 4;
const METADATA_MAX_SIZE_AT: usize = 40;
const METADATA_SLOT_COUNT_AT: usize = 44;
const SLOT_COPIES: u64 = 2;
const MAJOR_VERSION_AT: usize = 4;
const HEADER_SIZE_AT: usize = 8;
const TABLES_SIZE_AT: usize = 44;
const FIRST_EXTENT_INDEX_AT: usize = 40;
const NUM_EXTENTS_AT: usize = 44;
const GROUP_INDEX_AT: usize = 48;
const TARGET_TYPE_AT: usize = 8;
const TARGET_DATA_AT: usize = 12;
const TARGET_SOURCE_AT: usize = 20;
const DEVICE_SIZE_AT: usize = 16;
const GROUP_MAXIMUM_AT: usize = 40;
const LINEAR: u32 = 0;
const ZERO: u32 = 1;
const MAX_TABLES: u32 = 64 << 20;
const FILE_MODE: u32 = 0o644;
const IMAGE_SUFFIX: &str = ".img";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) metadata_max_size: u32,
    pub(crate) metadata_end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) offset: u32,
    pub(crate) count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) size: usize,
    pub(crate) tables_size: u32,
    pub(crate) tables: [Table; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Partition {
    pub(crate) name: [u8; NAME],
    pub(crate) first_extent: u32,
    pub(crate) extents: u32,
    pub(crate) group: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extent {
    Linear { at: u64, length: u64 },
    Zero { length: u64 },
}

impl Extent {
    fn length(self) -> u64 {
        match self {
            Extent::Linear { length, .. } | Extent::Zero { length } => length,
        }
    }
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn checksum_holds(bytes: &[u8], field: std::ops::Range<usize>) -> bool {
    let mut hasher = sha2::Sha256::new();
    hasher.update(&bytes[..field.start]);
    hasher.update([0u8; 32]);
    hasher.update(&bytes[field.end..]);
    hasher.finalize().as_slice() == &bytes[field]
}

pub(crate) fn geometry(bytes: &[u8; GEOMETRY]) -> Field<Geometry> {
    field_check(le32(bytes, 0) == GEOMETRY_MAGIC, "not an LP image")?;
    field_check(
        le32(bytes, STRUCT_SIZE_AT) == GEOMETRY as u32,
        "the LP geometry has another size",
    )?;
    let metadata_max_size = le32(bytes, METADATA_MAX_SIZE_AT);
    field_check(
        metadata_max_size > 0 && u64::from(metadata_max_size).is_multiple_of(SECTOR),
        "the LP metadata size is not a whole number of sectors",
    )?;
    let metadata_slots = le32(bytes, METADATA_SLOT_COUNT_AT);
    field_check(metadata_slots > 0, "the LP image has no metadata slot")?;
    Ok(Geometry {
        metadata_max_size,
        metadata_end: u64::from(metadata_slots)
            .saturating_mul(SLOT_COPIES * u64::from(metadata_max_size))
            .saturating_add(METADATA_AT),
    })
}

// EXTERNAL(7-Zip 26.05): a header of 128 or 256 bytes is read with any minor version, where liblp
// ties 256 bytes to version 10.2.
pub(crate) fn header(bytes: &[u8; HEADER], metadata_max_size: u32) -> Field<Header> {
    field_check(
        le32(bytes, 0) == HEADER_MAGIC,
        "the LP metadata header is missing",
    )?;
    field_check(
        le16(bytes, MAJOR_VERSION_AT) == MAJOR_VERSION,
        "the LP metadata has a major version other than 10",
    )?;
    let size = match le32(bytes, HEADER_SIZE_AT) {
        128 => HEADER,
        256 => HEADER_V1_2,
        _ => {
            return Err(BadField(
                "the LP metadata header is neither 128 nor 256 bytes",
            ));
        }
    };
    let tables_size = le32(bytes, TABLES_SIZE_AT);
    field_check(
        u64::from(tables_size) + size as u64 <= u64::from(metadata_max_size)
            && tables_size <= MAX_TABLES,
        "the LP metadata tables do not fit their slot",
    )?;
    let mut tables = [Table {
        offset: 0,
        count: 0,
    }; 4];
    for (index, entry_size) in ENTRY_SIZES.into_iter().enumerate() {
        let at = DESCRIPTORS_AT + DESCRIPTOR * index;
        let table = Table {
            offset: le32(bytes, at),
            count: le32(bytes, at + 4),
        };
        field_check(
            le32(bytes, at + 8) == entry_size as u32,
            "an LP metadata table has another entry size",
        )?;
        field_check(
            u64::from(table.offset) + u64::from(table.count) * entry_size as u64
                <= u64::from(tables_size),
            "an LP metadata table runs past the tables",
        )?;
        tables[index] = table;
    }
    Ok(Header {
        size,
        tables_size,
        tables,
    })
}

pub(crate) fn partition(bytes: &[u8; PARTITION]) -> Partition {
    Partition {
        name: bytes[..NAME].try_into().unwrap(),
        first_extent: le32(bytes, FIRST_EXTENT_INDEX_AT),
        extents: le32(bytes, NUM_EXTENTS_AT),
        group: le32(bytes, GROUP_INDEX_AT),
    }
}

pub(crate) fn extent(bytes: &[u8; EXTENT], devices: u32) -> Field<Extent> {
    let length = le64(bytes, 0)
        .checked_mul(SECTOR)
        .ok_or(BadField("an LP extent is too long"))?;
    match le32(bytes, TARGET_TYPE_AT) {
        LINEAR => {
            field_check(
                le32(bytes, TARGET_SOURCE_AT) < devices,
                "an LP extent names a block device the image does not hold",
            )?;
            let at = le64(bytes, TARGET_DATA_AT)
                .checked_mul(SECTOR)
                .filter(|at| at.checked_add(length).is_some())
                .ok_or(BadField("an LP extent runs past the largest image"))?;
            Ok(Extent::Linear { at, length })
        }
        ZERO => {
            field_check(
                le64(bytes, TARGET_DATA_AT) == 0,
                "an LP zero extent names a target sector",
            )?;
            Ok(Extent::Zero { length })
        }
        _ => Err(BadField(
            "an LP extent has a target type 7-Zip does not read",
        )),
    }
}

fn linear_extent_holds(bytes: &[u8; EXTENT], devices: u32, length: u64) -> Field<()> {
    if let Extent::Linear { at, length: size } = extent(bytes, devices)? {
        field_check(
            at + size <= length,
            "an LP extent runs past the end of the image",
        )?;
    }
    Ok(())
}

pub(crate) fn partition_extents(
    partition: &Partition,
    extents: u32,
) -> Field<std::ops::Range<usize>> {
    let end = u64::from(partition.first_extent) + u64::from(partition.extents);
    field_check(
        end <= u64::from(extents),
        "an LP partition names extents past the table",
    )?;
    Ok(partition.first_extent as usize..end as usize)
}

fn output_name(partition: &Partition, index: usize) -> String {
    let length = partition
        .name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(NAME);
    let mut name: String = partition.name[..length]
        .iter()
        .map(|&byte| char::from(byte))
        .collect();
    if name.is_empty() {
        name = index.to_string();
    }
    if partition.extents > 0 {
        name.push_str(IMAGE_SUFFIX);
    }
    name
}

struct Image {
    extents: Vec<Extent>,
    partitions: Vec<(String, std::ops::Range<usize>)>,
}

fn read_exact_at(file: &ArchiveFile, length: u64, buffer: &mut [u8], at: u64) -> Result<()> {
    ensure!(
        at.checked_add(buffer.len() as u64)
            .is_some_and(|end| end <= length),
        "the LP image is truncated"
    );
    let mut done = 0;
    while done < buffer.len() {
        let read = file
            .read_at(&mut buffer[done..], at + done as u64)
            .context("cannot read the LP image")?;
        ensure!(read > 0, "the LP image is truncated");
        done += read;
    }
    Ok(())
}

fn entries<const SIZE: usize>(tables: &[u8], table: Table) -> impl Iterator<Item = &[u8; SIZE]> {
    tables[table.offset as usize..][..table.count as usize * SIZE]
        .chunks_exact(SIZE)
        .map(|chunk| chunk.try_into().unwrap())
}

fn open(file: &ArchiveFile) -> Result<Image> {
    let length = file.len()?;
    let mut geometry_bytes = [0u8; GEOMETRY];
    read_exact_at(file, length, &mut geometry_bytes, GEOMETRY_AT)?;
    let found = geometry(&geometry_bytes)?;
    ensure!(
        checksum_holds(&geometry_bytes, CHECKSUM),
        "the LP geometry checksum does not match"
    );
    ensure!(
        found.metadata_end <= length,
        "the LP image ends inside its metadata slots"
    );
    let mut head = [0u8; HEADER_V1_2];
    read_exact_at(file, length, &mut head[..HEADER], METADATA_AT)?;
    let parsed = header(head[..HEADER].try_into().unwrap(), found.metadata_max_size)?;
    read_exact_at(
        file,
        length,
        &mut head[HEADER..parsed.size],
        METADATA_AT + HEADER as u64,
    )?;
    ensure!(
        checksum_holds(&head[..parsed.size], HEADER_CHECKSUM),
        "the LP metadata header checksum does not match"
    );
    let mut tables = vec![0u8; parsed.tables_size as usize];
    read_exact_at(file, length, &mut tables, METADATA_AT + parsed.size as u64)?;
    ensure!(
        sha2::Sha256::digest(&tables).as_slice() == &head[TABLES_CHECKSUM],
        "the LP metadata tables checksum does not match"
    );
    let [partitions, extents, groups, devices] = parsed.tables;
    ensure!(devices.count > 0, "the LP image has no block device");
    let largest_group = entries::<GROUP>(&tables, groups)
        .map(|group| le64(group, GROUP_MAXIMUM_AT))
        .max()
        .unwrap_or(0);
    for device in entries::<BLOCK_DEVICE>(&tables, devices) {
        ensure!(
            le64(device, 0).wrapping_mul(SECTOR) >= found.metadata_end,
            "an LP block device's first logical sector lies inside the metadata"
        );
        ensure!(
            le64(device, DEVICE_SIZE_AT).max(largest_group) <= length,
            "the LP image ends before a block device or a partition group does"
        );
    }
    let mut image = Image {
        extents: vec![Extent::Zero { length: 0 }; extents.count as usize],
        partitions: Vec::with_capacity(partitions.count as usize),
    };
    for (index, bytes) in entries::<PARTITION>(&tables, partitions).enumerate() {
        let partition = partition(bytes);
        ensure!(
            partition.group < groups.count,
            "an LP partition names a group the image does not hold"
        );
        let range = partition_extents(&partition, extents.count)?;
        image
            .partitions
            .push((output_name(&partition, index), range));
    }
    let raw: Vec<&[u8; EXTENT]> = entries::<EXTENT>(&tables, extents).collect();
    for bytes in raw
        .iter()
        .filter(|bytes| le32(&bytes[..], TARGET_TYPE_AT) == LINEAR)
    {
        linear_extent_holds(bytes, devices.count, length)?;
    }
    let mut used: Vec<std::ops::Range<usize>> = image
        .partitions
        .iter()
        .map(|(_, range)| range.clone())
        .collect();
    used.sort_unstable_by_key(|range| range.start);
    let mut checked = 0;
    for range in used {
        for index in range.start.max(checked)..range.end {
            image.extents[index] = extent(raw[index], devices.count)?;
        }
        checked = checked.max(range.end);
    }
    Ok(image)
}

struct Extents<'f> {
    file: &'f ArchiveFile,
    extents: &'f [Extent],
    within: u64,
}

impl tree::Holes for Extents<'_> {
    fn skip_hole(&mut self, most: u64) -> std::io::Result<u64> {
        self.skip_finished();
        let Some(&Extent::Zero { length }) = self.extents.first() else {
            return Ok(0);
        };
        let skipped = (length - self.within).min(most);
        self.within += skipped;
        Ok(skipped)
    }
}

impl Extents<'_> {
    fn skip_finished(&mut self) {
        while let Some(extent) = self.extents.first() {
            if extent.length() > self.within {
                return;
            }
            self.extents = &self.extents[1..];
            self.within = 0;
        }
    }
}

impl Read for Extents<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while let Some(&extent) = self.extents.first() {
            let left = extent.length() - self.within;
            if left == 0 {
                self.extents = &self.extents[1..];
                self.within = 0;
                continue;
            }
            let want = buffer
                .len()
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            let read = match extent {
                Extent::Zero { .. } => {
                    buffer[..want].fill(0);
                    want
                }
                Extent::Linear { at, .. } => {
                    self.file.read_at(&mut buffer[..want], at + self.within)?
                }
            };
            self.within += read as u64;
            return Ok(read);
        }
        Ok(0)
    }
}

fn plain(name: &str) -> bool {
    crate::safe_path(name).is_ok()
}

/// Returns whether the file holds an LP geometry, the record a logical
/// partition image starts with after its 4 KiB of reserved bytes.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut magic = [0u8; 4];
    Ok(file.len()? >= GEOMETRY_AT + GEOMETRY as u64
        && read_exact_at(&file, GEOMETRY_AT + 4, &mut magic, GEOMETRY_AT).is_ok()
        && u32::from_le_bytes(magic) == GEOMETRY_MAGIC)
}

/// Returns whether the default engine should send the image to 7-Zip: one
/// whose partition names are not plain relative paths, which 7-Zip rewrites.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    Ok(open(&file).is_ok_and(|image| image.partitions.iter().any(|(name, _)| !plain(name))))
}

/// Verify and optionally extract an LP image into a new directory, in the
/// tree `7zz x` writes: each logical partition as one file. Every check of
/// `docs/reference/lp-reader.md` holds before anything is published.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let file = crate::volumes::open(path)?;
    let image = open(&file)?;
    tree::extract("LP", path, destination, options, |tree| {
        for (name, range) in &image.partitions {
            tree.check()?;
            let extents = &image.extents[range.clone()];
            let size = extents
                .iter()
                .try_fold(0u64, |sum, extent| sum.checked_add(extent.length()))
                .context("an LP partition is larger than 2^64 bytes")?;
            let mut data = Extents {
                file: &file,
                extents,
                within: 0,
            };
            tree.sparse_file(name, size, FILE_MODE, None, &mut data)?;
        }
        Ok(())
    })
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn geometries_never_panic_and_keep_whole_sectors() {
        let bytes: [u8; GEOMETRY] = kani::any();
        if let Ok(found) = geometry(&bytes) {
            assert!(found.metadata_max_size > 0);
            assert!(u64::from(found.metadata_max_size) % SECTOR == 0);
            assert!(found.metadata_end >= METADATA_AT + 2 * u64::from(found.metadata_max_size));
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn headers_never_panic_and_keep_every_table_inside_the_slot() {
        let bytes: [u8; HEADER] = kani::any();
        let max: u32 = kani::any();
        if let Ok(found) = header(&bytes, max) {
            assert!(found.size == HEADER || found.size == HEADER_V1_2);
            assert!(found.size as u64 + u64::from(found.tables_size) <= u64::from(max));
            for (table, size) in found.tables.iter().zip(ENTRY_SIZES) {
                let end = u64::from(table.offset) + u64::from(table.count) * size as u64;
                assert!(end <= u64::from(found.tables_size));
            }
        }
    }

    #[kani::proof]
    fn extents_never_panic_and_end_inside_64_bits() {
        let bytes: [u8; EXTENT] = kani::any();
        match extent(&bytes, kani::any()) {
            Ok(Extent::Linear { at, length }) => {
                assert!(at.checked_add(length).is_some());
                assert!(at % SECTOR == 0 && length % SECTOR == 0);
            }
            Ok(Extent::Zero { length }) => assert!(length % SECTOR == 0),
            Err(_) => {}
        }
    }

    #[kani::proof]
    fn partitions_name_only_extents_the_table_holds() {
        let bytes: [u8; PARTITION] = kani::any();
        let count: u32 = kani::any();
        let found = partition(&bytes);
        if let Ok(range) = partition_extents(&found, count) {
            assert!(range.start <= range.end && range.end as u64 <= u64::from(count));
            assert!(range.end - range.start == found.extents as usize);
        }
    }
}
