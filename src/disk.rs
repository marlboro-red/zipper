//! In-process reader for VHD and VHDX disk images and MBR and GPT partition
//! tables, with the FAT file systems on them. A disk is read as a byte range,
//! each partition is a bounded `Read + Seek` view of it, and a file system
//! reader takes one view. `docs/reference/disk-reader.md` gives the rules.

pub use crate::native_cpio::ReadOptions;
use crate::{
    BadField, Entry, Field, field_check,
    native_cpio::tree::{self, BACKEND_HINT},
    native_fat,
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

const SECTOR: usize = 512;
const SECTOR_LOG: u32 = 9;
const FOUR_KIB_SECTOR_LOG: u32 = 12;
const BOOT_SIGNATURE_AT: usize = 0x1FE;
const RAW_FILE_MODE: u32 = 0o644;
const NESTING_LIMIT: u32 = 32;

const VHD_FOOTER: usize = 512;
const VHD_SPARSE_HEADER: usize = 1024;
const VHD_SIGNATURE: &[u8; 11] = b"conectix\0\0\0";
const VHD_SPARSE_SIGNATURE: &[u8; 8] = b"cxsparse";
const VHD_FIXED: u32 = 2;
const VHD_DIFFERENCING: u32 = 4;
const VHD_UNUSED_BLOCK: u32 = u32::MAX;
const VHD_LARGEST_DISK: u64 = 1 << 41;
const VHD_DATA_OFFSET_LIMIT: u64 = 1 << 42;
const VHD_BLOCK_LIMIT: u32 = 1 << 31;
const VHD_SMALLEST_BLOCK_LOG: u32 = 19;
const VHD_LARGEST_BLOCK_LOG: u32 = 31;
const VHD_LOCATORS_AT: usize = 0x240;
const VHD_LOCATOR: usize = 24;
const VHD_LOCATORS: usize = 8;
const VHD_SECTORS_PER_BITMAP_SECTOR: u32 = 512 * 8;
const VHD_FOOTER_FIELDS: usize = 0x40;
const VHD_FOOTER_CHECKSUM: usize = 0x40;
const VHD_FOOTER_ZEROS: usize = 0x55;
const VHD_SPARSE_FIELDS: usize = VHD_LOCATORS_AT + VHD_LOCATORS * VHD_LOCATOR;
const VHD_SPARSE_CHECKSUM: usize = 0x24;
const VHD_TABLE_BATCH: usize = 1 << 16;

const VHDX_SIGNATURE: &[u8; 8] = b"vhdxfile";
const VHDX_HEADER: usize = 4096;
const VHDX_HEADER_FIELDS: usize = 0x50;
const VHDX_HEADERS_AT: [u64; 2] = [1 << 16, 2 << 16];
const VHDX_HEADER_SIGNATURE: u32 = 0x6461_6568;
const VHDX_REGION_SIGNATURE: u32 = 0x6967_6572;
const VHDX_REGIONS_AT: u64 = 3 << 16;
const VHDX_REGION_TABLE: usize = 1 << 16;
const VHDX_REGION_ENTRY: usize = 32;
const VHDX_REGION_ENTRIES: u32 = (1 << 11) - 1;
const VHDX_METADATA_SIGNATURE: &[u8; 8] = b"metadata";
const VHDX_METADATA_ENTRY: usize = 32;
const VHDX_METADATA_ENTRIES: usize = (1 << 11) - 1;
const VHDX_METADATA_TABLE: usize = VHDX_METADATA_ENTRY * (1 + VHDX_METADATA_ENTRIES);
const VHDX_METADATA_ITEM_LIMIT: u32 = 1 << 20;
const VHDX_ALIGNMENT_MASK: u64 = (1 << 20) - 1;
const VHDX_FIRST_REGION_END: u64 = 1 << 20;
const VHDX_BITMAP_LOG: u32 = 20;
const VHDX_SMALLEST_BLOCK_LOG: u32 = 20;
const VHDX_LARGEST_BLOCK_LOG: u32 = 28;
const VHDX_LARGEST_DISK: u64 = 1 << 46;
const VHDX_PHYSICAL_LIMIT: u64 = 1 << 62;
const VHDX_BAT_STATE_MASK: u64 = 7;
const VHDX_BAT_RESERVED_MASK: u64 = 0xFFFF8;
const VHDX_BAT_OFFSET_MASK: u64 = !0xFFFFF;
const VHDX_BAT_BATCH: u64 = 8192;
const BLOCK_NOT_PRESENT: u64 = 0;
const BLOCK_UNDEFINED: u64 = 1;
const BLOCK_ZERO: u64 = 2;
const BLOCK_UNMAPPED: u64 = 3;
const BLOCK_FULLY_PRESENT: u64 = 6;
const BLOCK_PARTIALLY_PRESENT: u64 = 7;
const HAS_PARENT: u32 = 2;
const METADATA_REQUIRED: u32 = 4;
const BAT_GUID: [u8; 16] = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
const METADATA_GUID: [u8; 16] = [
    0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B, 0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E,
];
const FILE_PARAMETERS_GUID: [u8; 16] = [
    0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D, 0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B,
];
const VIRTUAL_DISK_SIZE_GUID: [u8; 16] = [
    0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48, 0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8,
];
const VIRTUAL_DISK_ID_GUID: [u8; 16] = [
    0xAB, 0x12, 0xCA, 0xBE, 0xE6, 0xB2, 0x23, 0x45, 0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46,
];
const LOGICAL_SECTOR_GUID: [u8; 16] = [
    0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47, 0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F,
];
const PHYSICAL_SECTOR_GUID: [u8; 16] = [
    0xC7, 0x48, 0xA3, 0xCD, 0x5D, 0x44, 0x71, 0x44, 0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56,
];
const PARENT_LOCATOR_GUID: [u8; 16] = [
    0x2D, 0x5F, 0xD3, 0xA8, 0x0B, 0xB3, 0x4D, 0x45, 0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C,
];

const MBR_TABLE_AT: usize = 0x1BE;
const MBR_ENTRY: usize = 16;
const MBR_ENTRIES: usize = 4;
const MBR_DEPTH_LIMIT: u32 = 128;
const MBR_ITEM_LIMIT: usize = 128;
const MBR_FIRST_READ: usize = 1 << 12;
const MBR_NTFS: u8 = 7;
const MBR_TYPES: [(u8, Option<&str>); 26] = [
    (0x01, Some("fat")),
    (0x04, Some("fat")),
    (0x05, None),
    (0x06, Some("fat")),
    (0x07, Some("ntfs")),
    (0x0B, Some("fat")),
    (0x0C, Some("fat")),
    (0x0E, Some("fat")),
    (0x0F, None),
    (0x11, Some("fat")),
    (0x14, Some("fat")),
    (0x16, Some("fat")),
    (0x1B, Some("fat")),
    (0x1C, Some("fat")),
    (0x1E, Some("fat")),
    (0x27, Some("ntfs")),
    (0x82, None),
    (0x83, None),
    (0x8E, Some("lvm")),
    (0xA5, None),
    (0xBE, None),
    (0xBF, None),
    (0xC2, None),
    (0xC3, None),
    (0xEE, None),
    (0xEE, None),
];

const GPT_SIGNATURE: &[u8; 12] = b"EFI PART\0\0\x01\0";
const GPT_HEAD_READ: usize = 2 << 12;
const GPT_ENTRY_LIMIT: u32 = 1 << 16;
const GPT_SMALLEST_ENTRY: u32 = 128;
const GPT_LARGEST_ENTRY: u32 = 1 << 12;
const GPT_NAME_UNITS: usize = 36;
const GPT_ENTRY_READ: usize = 56 + 2 * GPT_NAME_UNITS;
const GPT_TYPES: [(u32, Option<&str>, bool); 23] = [
    (0x2168_6148, None, false),
    (0xC12A_7328, None, false),
    (0x024D_EE41, None, false),
    (0xE3C9_E316, None, true),
    (0xEBD0_A0A2, None, true),
    (0x5808_C8AA, None, true),
    (0xAF9B_60A0, None, true),
    (0xDE94_BBA4, None, true),
    (0x0FC6_3DAF, None, false),
    (0x0657_FD6D, None, false),
    (0x4447_9540, None, false),
    (0x4F68_BCE3, None, false),
    (0x69DA_D710, None, false),
    (0xB921_B045, None, false),
    (0x993D_8D3D, None, false),
    (0x83BD_6B9D, None, false),
    (0x516E_7CB4, None, false),
    (0x516E_7CB5, None, false),
    (0x516E_7CB6, Some("ufs"), false),
    (0x516E_7CB8, None, false),
    (0x516E_7CB8, Some("zfs"), false),
    (0x4846_5300, Some("hfsx"), false),
    (0x7C34_57EF, Some("apfs"), false),
];

const NTFS_SIGNATURE: &[u8; 9] = b"NTFS    \0";
const EXFAT_SIGNATURE: &[u8; 8] = b"EXFAT   ";
const UDF_AT: u64 = 0x8000;
const UDF_SIGNATURE: &[u8; 8] = b"\0BEA01\x01\0";
const DMG_SIGNATURE: &[u8; 4] = b"koly";
const HFS_AT: usize = 1024;
const EXT_SIGNATURE_AT: usize = 1080;
const SNIFF_READ: usize = 2 << 12;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn guid(bytes: &[u8], at: usize) -> [u8; 16] {
    bytes[at..at + 16].try_into().unwrap()
}

const fn crc32c_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0x82F6_3B78 & (0u32.wrapping_sub(value & 1)));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

static CRC32C: [u32; 256] = crc32c_table();

fn crc32c(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(u32::MAX, |crc, byte| {
        CRC32C[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

fn crc_without_field(bytes: &[u8], at: usize, covered: usize, crc: impl Fn(&[u8]) -> u32) -> bool {
    let mut copy = bytes.to_vec();
    copy[at..at + 4].fill(0);
    crc(&copy[..covered]) == le32(bytes, at)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VhdFooter {
    pub(crate) kind: u32,
    pub(crate) data_offset: u64,
    pub(crate) size: u64,
}

fn vhd_checksum_holds(bytes: &[u8], at: usize) -> bool {
    let mut sum = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        if index < at || index >= at + 4 {
            sum = sum.wrapping_add(u32::from(*byte));
        }
    }
    !sum == be32(bytes, at)
}

pub(crate) fn vhd_footer(p: &[u8; VHD_FOOTER]) -> Field<VhdFooter> {
    let footer = vhd_footer_fields(p[..VHD_FOOTER_FIELDS].try_into().unwrap())?;
    field_check(
        vhd_checksum_holds(p, VHD_FOOTER_CHECKSUM)
            && p[VHD_FOOTER_ZEROS..].iter().all(|byte| *byte == 0),
        "the VHD footer checksum does not match",
    )?;
    Ok(footer)
}

pub(crate) fn vhd_footer_fields(p: &[u8; VHD_FOOTER_FIELDS]) -> Field<VhdFooter> {
    field_check(&p[..11] == VHD_SIGNATURE, "not a VHD footer")?;
    field_check(p[11] & 0xFE == 2, "the VHD footer names unknown features")?;
    let footer = VhdFooter {
        kind: be32(p, 0x3C),
        data_offset: be64(p, 0x10),
        size: be64(p, 0x30),
    };
    field_check(
        footer.size <= VHD_LARGEST_DISK,
        "the VHD disk is larger than 2 TiB",
    )?;
    field_check(
        (VHD_FIXED..=VHD_DIFFERENCING).contains(&footer.kind),
        "the VHD footer names an unknown disk type",
    )?;
    if footer.kind == VHD_FIXED {
        field_check(footer.data_offset == u64::MAX, "a fixed VHD names a header")?;
    } else {
        field_check(
            footer.data_offset < VHD_DATA_OFFSET_LIMIT,
            "the VHD header lies too far into the file",
        )?;
    }
    Ok(footer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VhdSparse {
    pub(crate) table: u64,
    pub(crate) blocks: u32,
    pub(crate) block_log: u32,
}

pub(crate) fn vhd_sparse_header(p: &[u8; VHD_SPARSE_HEADER]) -> Field<VhdSparse> {
    let sparse = vhd_sparse_fields(p[..VHD_SPARSE_FIELDS].try_into().unwrap())?;
    field_check(
        vhd_checksum_holds(p, VHD_SPARSE_CHECKSUM)
            && p[VHD_SPARSE_FIELDS..].iter().all(|byte| *byte == 0),
        "the dynamic VHD header checksum does not match",
    )?;
    Ok(sparse)
}

pub(crate) fn vhd_sparse_fields(p: &[u8; VHD_SPARSE_FIELDS]) -> Field<VhdSparse> {
    field_check(
        &p[..8] == VHD_SPARSE_SIGNATURE,
        "the dynamic VHD header lacks its signature",
    )?;
    field_check(
        p[8..16].iter().all(|byte| *byte == 0xFF),
        "the dynamic VHD header names a next header",
    )?;
    let block_log = (VHD_SMALLEST_BLOCK_LOG..=VHD_LARGEST_BLOCK_LOG)
        .find(|log| 1u32 << log == be32(p, 0x20))
        .ok_or(BadField(
            "the VHD block size is not a power of two of 512 KiB or more",
        ))?;
    field_check(
        be32(p, 0x3C) == 0,
        "the dynamic VHD header's reserved field is set",
    )?;
    for locator in 0..VHD_LOCATORS {
        field_check(
            be32(p, VHD_LOCATORS_AT + locator * VHD_LOCATOR + 0x0C) == 0,
            "a VHD parent locator's reserved field is set",
        )?;
    }
    Ok(VhdSparse {
        table: be64(p, 0x10),
        blocks: be32(p, 0x1C),
        block_log,
    })
}

pub(crate) fn vhd_bitmap_bytes(block_log: u32) -> u32 {
    let sectors = (1u64 << block_log).div_ceil(u64::from(VHD_SECTORS_PER_BITMAP_SECTOR) * 512);
    (sectors as u32) << SECTOR_LOG
}

pub(crate) fn vhd_blocks_fit(size: u64, blocks: u32, block_log: u32) -> bool {
    blocks < VHD_BLOCK_LIMIT
        && if size == 0 {
            blocks == 0
        } else {
            ((size - 1) >> block_log) + 1 == u64::from(blocks)
        }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VhdxHeader {
    pub(crate) sequence: u64,
    pub(crate) guids: [[u8; 16]; 3],
    pub(crate) log_length: u32,
    pub(crate) log_offset: u64,
}

pub(crate) fn vhdx_header_fields(p: &[u8; VHDX_HEADER_FIELDS]) -> Field<VhdxHeader> {
    field_check(
        le32(p, 0) == VHDX_HEADER_SIGNATURE,
        "a VHDX header lacks its signature",
    )?;
    let header = VhdxHeader {
        sequence: le64(p, 8),
        guids: [guid(p, 0x10), guid(p, 0x20), guid(p, 0x30)],
        log_length: le32(p, 0x44),
        log_offset: le64(p, 0x48),
    };
    field_check(le16(p, 0x42) == 1, "a VHDX header has an unknown version")?;
    field_check(
        u64::from(header.log_length) & VHDX_ALIGNMENT_MASK == 0
            && header.log_offset & VHDX_ALIGNMENT_MASK == 0,
        "the VHDX log is not aligned to 1 MiB",
    )?;
    Ok(header)
}

fn vhdx_header(p: &[u8]) -> Field<VhdxHeader> {
    field_check(
        crc_without_field(&p[..VHDX_HEADER], 4, VHDX_HEADER, crc32c),
        "a VHDX header checksum does not match",
    )?;
    vhdx_header_fields(p[..VHDX_HEADER_FIELDS].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VhdxRegion {
    pub(crate) guid: [u8; 16],
    pub(crate) offset: u64,
    pub(crate) length: u32,
    pub(crate) required: bool,
}

pub(crate) fn vhdx_region(p: &[u8; VHDX_REGION_ENTRY]) -> Field<VhdxRegion> {
    let region = VhdxRegion {
        guid: guid(p, 0),
        offset: le64(p, 0x10),
        length: le32(p, 0x18),
        required: le32(p, 0x1C) != 0,
    };
    field_check(
        region.offset & VHDX_ALIGNMENT_MASK == 0
            && u64::from(region.length) & VHDX_ALIGNMENT_MASK == 0,
        "a VHDX region is not aligned to 1 MiB",
    )?;
    field_check(
        region
            .offset
            .checked_add(u64::from(region.length))
            .is_some(),
        "a VHDX region ends past 2^64",
    )?;
    Ok(region)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VhdxItem {
    pub(crate) guid: [u8; 16],
    pub(crate) offset: u32,
    pub(crate) length: u32,
    pub(crate) required: bool,
}

pub(crate) fn vhdx_item(
    p: &[u8; VHDX_METADATA_ENTRY],
    metadata_region_length: u32,
) -> Field<VhdxItem> {
    let item = VhdxItem {
        guid: guid(p, 0),
        offset: le32(p, 0x10),
        length: le32(p, 0x14),
        required: le32(p, 0x18) & METADATA_REQUIRED != 0,
    };
    field_check(
        item.offset == 0 || item.offset >= 1 << 16,
        "a VHDX metadata item overlaps the metadata table",
    )?;
    field_check(
        item.length <= VHDX_METADATA_ITEM_LIMIT,
        "a VHDX metadata item is over 1 MiB",
    )?;
    field_check(
        item.length != 0 || item.offset == 0,
        "an empty VHDX metadata item has a place",
    )?;
    field_check(
        le32(p, 0x18) >> 3 == 0,
        "a VHDX metadata item sets reserved flags",
    )?;
    field_check(
        le32(p, 0x1C) & 3 == 0,
        "a VHDX metadata item sets reserved flags",
    )?;
    field_check(
        item.offset <= metadata_region_length
            && item.length <= metadata_region_length - item.offset,
        "a VHDX metadata item lies past its region",
    )?;
    Ok(item)
}

pub(crate) fn vhdx_bat_entry(value: u64, sector_bitmap: bool) -> Field<(u64, u64)> {
    field_check(
        value & VHDX_BAT_RESERVED_MASK == 0,
        "a VHDX block table entry sets reserved bits",
    )?;
    let state = value & VHDX_BAT_STATE_MASK;
    let offset = value & VHDX_BAT_OFFSET_MASK;
    if sector_bitmap {
        field_check(
            state == BLOCK_NOT_PRESENT,
            "a VHDX without a parent names a sector bitmap",
        )?;
    } else {
        match state {
            BLOCK_FULLY_PRESENT => field_check(offset != 0, "a VHDX block lies at offset 0")?,
            BLOCK_NOT_PRESENT | BLOCK_UNDEFINED | BLOCK_ZERO | BLOCK_UNMAPPED => {}
            BLOCK_PARTIALLY_PRESENT => {
                return Err(BadField(
                    "a VHDX without a parent has a partly present block",
                ));
            }
            _ => return Err(BadField("a VHDX block table entry has an unknown state")),
        }
    }
    Ok((state, offset))
}

pub(crate) fn vhdx_bat_index(block: u64, chunk_ratio_log: u32) -> u64 {
    let chunk = block >> chunk_ratio_log;
    chunk * ((1 << chunk_ratio_log) + 1) + (block & ((1 << chunk_ratio_log) - 1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MbrEntry {
    pub(crate) kind: u8,
    pub(crate) lba: u32,
    pub(crate) blocks: u32,
}

impl MbrEntry {
    fn empty(&self) -> bool {
        self.kind == 0
    }

    fn extended(&self) -> bool {
        matches!(self.kind, 0x05 | 0x0F)
    }

    fn limit(&self) -> u32 {
        self.lba.wrapping_add(self.blocks)
    }

    fn fits(&self) -> bool {
        u32::MAX - self.lba >= self.blocks
    }
}

fn chs_holds(chs: &[u8]) -> bool {
    chs[0] | chs[1] | chs[2] == 0 || chs[1] & 0x3F > 0
}

pub(crate) fn mbr_entry(p: &[u8; MBR_ENTRY]) -> Field<MbrEntry> {
    let entry = MbrEntry {
        kind: p[4],
        lba: le32(p, 8),
        blocks: le32(p, 12),
    };
    if entry.empty() {
        return Ok(entry);
    }
    field_check(
        matches!(p[0], 0 | 0x80),
        "an MBR partition has an unknown status",
    )?;
    field_check(
        chs_holds(&p[1..4]) && chs_holds(&p[5..8]) && entry.blocks > 0 && entry.fits(),
        "an MBR partition entry is malformed",
    )?;
    Ok(entry)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GptHeader {
    pub(crate) table_lba: u64,
    pub(crate) entries: u32,
    pub(crate) entry_size: u32,
    pub(crate) entries_crc: u32,
    pub(crate) backup_lba: u64,
    pub(crate) header_size: u32,
}

pub(crate) fn gpt_header(p: &[u8; 0x5C], sector_log: u32) -> Field<GptHeader> {
    let header = GptHeader {
        header_size: le32(p, 12),
        table_lba: le64(p, 0x48),
        entries: le32(p, 0x50),
        entry_size: le32(p, 0x54),
        entries_crc: le32(p, 0x58),
        backup_lba: le64(p, 0x20),
    };
    field_check(
        header.header_size <= 1 << sector_log,
        "the GPT header is larger than its sector",
    )?;
    field_check(
        le64(p, 0x18) == 1,
        "the GPT header does not place itself at sector 1",
    )?;
    field_check(
        header.table_lba >= 2 && header.table_lba >> (63 - sector_log) == 0,
        "the GPT table lies outside the disk",
    )?;
    field_check(
        header.entries <= GPT_ENTRY_LIMIT,
        "the GPT holds more than 65,536 entries",
    )?;
    field_check(
        (GPT_SMALLEST_ENTRY..=GPT_LARGEST_ENTRY).contains(&header.entry_size),
        "a GPT entry is not 128 to 4,096 bytes",
    )?;
    Ok(header)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GptEntry {
    pub(crate) kind: u32,
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) name: [u16; GPT_NAME_UNITS],
}

pub(crate) fn gpt_entry(p: &[u8; GPT_ENTRY_READ], sector_log: u32) -> Field<Option<GptEntry>> {
    if p[..16].iter().all(|byte| *byte == 0) {
        return Ok(None);
    }
    let mut name = [0u16; GPT_NAME_UNITS];
    for (index, unit) in name.iter_mut().enumerate() {
        *unit = le16(p, 56 + 2 * index);
    }
    let entry = GptEntry {
        kind: le32(p, 0),
        first: le64(p, 32),
        last: le64(p, 40),
        name,
    };
    field_check(
        entry.last >= entry.first,
        "a GPT partition ends before it starts",
    )?;
    field_check(
        entry.last >> (63 - sector_log) == 0,
        "a GPT partition lies outside the disk",
    )?;
    Ok(Some(entry))
}

enum Mapping {
    Plain {
        start: u64,
    },
    Vhd {
        table: u64,
        table_bytes: u64,
        block_log: u32,
        bitmap_bytes: u32,
        limit: u64,
    },
    Vhdx {
        table: u64,
        table_bytes: u64,
        block_log: u32,
        chunk_ratio_log: u32,
        limit: u64,
    },
}

const TABLE_PAGE: u64 = 4096;
const TABLE_PAGES_CACHED: usize = 8;

pub(crate) struct Disk {
    file: ArchiveFile,
    mapping: Mapping,
    size: u64,
    table_pages: Vec<(u64, Vec<u8>)>,
    next_victim: usize,
    bitmap: Option<(u64, Vec<u8>)>,
}

fn read_file_exact(file: &ArchiveFile, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

fn read_at(
    file: &ArchiveFile,
    length: u64,
    offset: u64,
    bytes: usize,
    what: &str,
) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; bytes];
    ensure!(
        offset
            .checked_add(bytes as u64)
            .is_some_and(|end| end <= length),
        "the image ends inside its {what}"
    );
    read_file_exact(file, &mut buffer, offset)
        .with_context(|| format!("cannot read the image's {what}"))?;
    Ok(buffer)
}

impl Disk {
    fn plain(file: ArchiveFile, start: u64, size: u64) -> Disk {
        Disk {
            file,
            mapping: Mapping::Plain { start },
            size,
            table_pages: Vec::new(),
            next_victim: 0,
            bitmap: None,
        }
    }

    fn table_bytes(
        &mut self,
        table: u64,
        table_bytes: u64,
        offset: u64,
        into: &mut [u8],
    ) -> io::Result<()> {
        for (place, byte) in into.iter_mut().enumerate() {
            let at = offset + place as u64;
            let page = at / TABLE_PAGE;
            let within = (at % TABLE_PAGE) as usize;
            if let Some((_, bytes)) = self.table_pages.iter().find(|(index, _)| *index == page) {
                *byte = bytes[within];
                continue;
            }
            let from = page * TABLE_PAGE;
            let length = (table_bytes - from).min(TABLE_PAGE) as usize;
            let mut bytes = vec![0u8; length];
            read_file_exact(&self.file, &mut bytes, table + from)?;
            *byte = bytes[within];
            if self.table_pages.len() < TABLE_PAGES_CACHED {
                self.table_pages.push((page, bytes));
            } else {
                self.table_pages[self.next_victim] = (page, bytes);
                self.next_victim = (self.next_victim + 1) % TABLE_PAGES_CACHED;
            }
        }
        Ok(())
    }

    fn read_limited(&self, limit: u64, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if offset > limit || buffer.len() as u64 > limit - offset {
            return Err(io::Error::other(
                "a disk block lies past the end of the image",
            ));
        }
        read_file_exact(&self.file, buffer, offset)
    }

    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size || buffer.is_empty() {
            return Ok(0);
        }
        let left = self.size - offset;
        match self.mapping {
            Mapping::Plain { start } => {
                let want = (buffer.len() as u64).min(left) as usize;
                self.file.read_at(&mut buffer[..want], start + offset)
            }
            Mapping::Vhd {
                table,
                table_bytes,
                block_log,
                bitmap_bytes,
                limit,
            } => {
                let block = offset >> block_log;
                let within = offset & ((1 << block_log) - 1);
                let want = (buffer.len() as u64)
                    .min(left)
                    .min((1 << block_log) - within) as usize;
                let mut entry = [0u8; 4];
                self.table_bytes(table, table_bytes, 4 * block, &mut entry)?;
                let entry = u32::from_be_bytes(entry);
                if entry == VHD_UNUSED_BLOCK {
                    buffer[..want].fill(0);
                    return Ok(want);
                }
                let place = u64::from(entry) << SECTOR_LOG;
                if self.bitmap.as_ref().is_none_or(|(held, _)| *held != block) {
                    let mut bitmap = vec![0u8; bitmap_bytes as usize];
                    self.read_limited(limit, place, &mut bitmap)?;
                    self.bitmap = Some((block, bitmap));
                }
                self.read_limited(
                    limit,
                    place + u64::from(bitmap_bytes) + within,
                    &mut buffer[..want],
                )?;
                let bitmap = &self.bitmap.as_ref().unwrap().1;
                let mut done = 0usize;
                while done < want {
                    let at = within + done as u64;
                    let sector = (at >> SECTOR_LOG) as usize;
                    let piece =
                        ((SECTOR as u64 - (at & (SECTOR as u64 - 1))) as usize).min(want - done);
                    let present = bitmap[sector >> 3] >> (7 ^ (sector & 7)) & 1 != 0;
                    if !present && buffer[done..done + piece].iter().any(|byte| *byte != 0) {
                        return Err(io::Error::other(
                            "a VHD block holds data in a sector its bitmap marks absent",
                        ));
                    }
                    done += piece;
                }
                Ok(want)
            }
            Mapping::Vhdx {
                table,
                table_bytes,
                block_log,
                chunk_ratio_log,
                limit,
            } => {
                let block = offset >> block_log;
                let within = offset & ((1 << block_log) - 1);
                let want = (buffer.len() as u64)
                    .min(left)
                    .min((1 << block_log) - within) as usize;
                let mut entry = [0u8; 8];
                self.table_bytes(
                    table,
                    table_bytes,
                    8 * vhdx_bat_index(block, chunk_ratio_log),
                    &mut entry,
                )?;
                let entry = u64::from_le_bytes(entry);
                if entry & VHDX_BAT_STATE_MASK != BLOCK_FULLY_PRESENT {
                    buffer[..want].fill(0);
                    return Ok(want);
                }
                let place = (entry & VHDX_BAT_OFFSET_MASK) + within;
                if place > limit || want as u64 > limit - place {
                    return Err(io::Error::other(
                        "a VHDX block lies past the image's last block",
                    ));
                }
                self.file.read_at(&mut buffer[..want], place)
            }
        }
    }
}

pub(crate) struct View<'d> {
    disk: &'d mut Disk,
    start: u64,
    length: u64,
    end: u64,
    position: u64,
}

impl<'d> View<'d> {
    fn new(disk: &'d mut Disk, start: u64, length: u64, end: u64) -> View<'d> {
        View {
            disk,
            start,
            length,
            end,
            position: 0,
        }
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.read_exact(buffer)
    }

    fn read_if_there(&mut self, offset: u64, buffer: &mut [u8]) -> Result<bool> {
        match self.read_exact_at(offset, buffer) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(anyhow::Error::from(error).context("cannot read the disk")),
        }
    }
}

impl Read for View<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.length {
            return Ok(0);
        }
        let Some(at) = self
            .start
            .checked_add(self.position)
            .filter(|at| *at < self.end)
        else {
            return Ok(0);
        };
        let want = (buffer.len() as u64)
            .min(self.length - self.position)
            .min(self.end - at) as usize;
        let read = self.disk.read(at, &mut buffer[..want])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for View<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.length.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position =
            target.ok_or_else(|| io::Error::other("a seek before the start of a disk view"))?;
        Ok(self.position)
    }
}

#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    length: u64,
    end: u64,
}

impl Span {
    fn inner(self, offset: u64, length: u64) -> Span {
        let start = self.start.saturating_add(offset);
        Span {
            start,
            length,
            end: self
                .end
                .min(self.start.saturating_add(self.length))
                .min(start.saturating_add(length))
                .max(start),
        }
    }

    fn view(self, disk: &mut Disk) -> View<'_> {
        View::new(disk, self.start, self.length, self.end)
    }
}

struct RawItem {
    name: String,
    span: Span,
}

enum Plan {
    FileSystem(Span),
    Files {
        format: &'static str,
        items: Vec<RawItem>,
    },
    Elsewhere(String),
}

enum Opened {
    Disk(Disk),
    Elsewhere(String),
}

fn has_vhd_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vhd"))
}

fn open(path: &Path) -> Result<Opened> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    if length < SECTOR as u64 {
        return Ok(Opened::Disk(Disk::plain(file, 0, length)));
    }
    let head = read_at(&file, length, 0, SECTOR, "first sector")?;
    if head.starts_with(VHDX_SIGNATURE) {
        return open_vhdx(file, length);
    }
    let head: [u8; VHD_FOOTER] = head.try_into().unwrap();
    match vhd_footer(&head) {
        Ok(footer) if footer.kind == VHD_FIXED && footer.size == 0 => {
            return Ok(Opened::Disk(Disk::plain(file, 0, 0)));
        }
        Ok(footer) if footer.kind == VHD_FIXED => {}
        Ok(footer) if footer.kind == VHD_DIFFERENCING => {
            return Ok(Opened::Elsewhere(
                "the image is a differencing VHD, which needs its parent disk".to_owned(),
            ));
        }
        Ok(footer) => return open_dynamic_vhd(file, length, &head, footer),
        Err(_) if head.starts_with(VHD_SIGNATURE) => {
            bail!("the VHD footer at the start of the image is damaged")
        }
        Err(_) => {}
    }
    if has_vhd_extension(path) {
        let tail: [u8; VHD_FOOTER] = read_at(
            &file,
            length,
            length - VHD_FOOTER as u64,
            VHD_FOOTER,
            "footer",
        )?
        .try_into()
        .unwrap();
        if let Ok(footer) = vhd_footer(&tail)
            && footer.kind == VHD_FIXED
            && footer.size <= length - VHD_FOOTER as u64
        {
            let start = length - VHD_FOOTER as u64 - footer.size;
            return Ok(Opened::Disk(Disk::plain(file, start, footer.size)));
        }
    }
    Ok(Opened::Disk(Disk::plain(file, 0, length)))
}

fn open_dynamic_vhd(
    file: ArchiveFile,
    length: u64,
    head: &[u8],
    footer: VhdFooter,
) -> Result<Opened> {
    let tail = read_at(
        &file,
        length,
        length - VHD_FOOTER as u64,
        VHD_FOOTER,
        "footer",
    )?;
    ensure!(
        tail == head,
        "the VHD's footer at the end of the file does not repeat the copy at its start"
    );
    let limit = length - VHD_FOOTER as u64;
    ensure!(
        footer.data_offset <= limit && VHD_SPARSE_HEADER as u64 <= limit - footer.data_offset,
        "the dynamic VHD header lies past the end of the image"
    );
    let header: [u8; VHD_SPARSE_HEADER] = read_at(
        &file,
        length,
        footer.data_offset,
        VHD_SPARSE_HEADER,
        "dynamic header",
    )?
    .try_into()
    .unwrap();
    let sparse = vhd_sparse_header(&header).map_err(anyhow::Error::from)?;
    ensure!(
        vhd_blocks_fit(footer.size, sparse.blocks, sparse.block_log),
        "the VHD block count does not cover its disk"
    );
    let table_bytes = (u64::from(sparse.blocks) * 4).next_multiple_of(SECTOR as u64);
    ensure!(
        sparse.table <= limit && table_bytes <= limit - sparse.table,
        "the VHD block table lies past the end of the image"
    );
    let bitmap_bytes = vhd_bitmap_bytes(sparse.block_log);
    let mut batch = vec![0u8; VHD_TABLE_BATCH];
    let mut read = 0u64;
    let used = u64::from(sparse.blocks) * 4;
    while read < used {
        let piece = (used - read).min(VHD_TABLE_BATCH as u64) as usize;
        read_file_exact(&file, &mut batch[..piece], sparse.table + read)
            .context("cannot read the VHD block table")?;
        for entry in batch[..piece].chunks_exact(4) {
            let entry = be32(entry, 0);
            ensure!(
                entry == VHD_UNUSED_BLOCK
                    || (u64::from(entry) << SECTOR_LOG)
                        + u64::from(bitmap_bytes)
                        + (1 << sparse.block_log)
                        <= length,
                "a VHD block lies past the end of the image"
            );
        }
        read += piece as u64;
    }
    let mut disk = Disk::plain(file, 0, footer.size);
    disk.mapping = Mapping::Vhd {
        table: sparse.table,
        table_bytes,
        block_log: sparse.block_log,
        bitmap_bytes,
        limit,
    };
    Ok(Opened::Disk(disk))
}

struct VhdxMetadata {
    block_log: u32,
    logical_log: u32,
    size: u64,
    flags: u32,
    locator: bool,
}

fn read_vhdx_metadata(file: &ArchiveFile, length: u64, region: VhdxRegion) -> Result<VhdxMetadata> {
    ensure!(region.length != 0, "the VHDX metadata region is empty");
    ensure!(
        region.offset + u64::from(region.length) <= length,
        "the VHDX metadata region lies past the end of the image"
    );
    let table_length = (region.length as usize).min(VHDX_METADATA_TABLE);
    let table = read_at(file, length, region.offset, table_length, "VHDX metadata")?;
    ensure!(
        table.starts_with(VHDX_METADATA_SIGNATURE) && le16(&table, 8) == 0,
        "the VHDX metadata table lacks its signature"
    );
    let count = usize::from(le16(&table, 10));
    ensure!(
        count <= VHDX_METADATA_ENTRIES && table[12..32].iter().all(|byte| *byte == 0),
        "the VHDX metadata table header is malformed"
    );
    let mut found = VhdxMetadata {
        block_log: 0,
        logical_log: 0,
        size: 0,
        flags: 0,
        locator: false,
    };
    let (mut size_seen, mut physical_seen) = (false, false);
    for index in 0..count {
        let at = VHDX_METADATA_ENTRY * (index + 1);
        let item = vhdx_item(
            table[at..at + VHDX_METADATA_ENTRY].try_into().unwrap(),
            region.length,
        )
        .map_err(anyhow::Error::from)?;
        let value = |bytes: u32| -> Result<Vec<u8>> {
            ensure!(
                item.length == bytes,
                "a VHDX metadata item has the wrong length"
            );
            read_at(
                file,
                length,
                region.offset + u64::from(item.offset),
                bytes as usize,
                "VHDX metadata",
            )
        };
        match item.guid {
            FILE_PARAMETERS_GUID => {
                ensure!(found.block_log == 0, "the VHDX names its block size twice");
                let bytes = value(8)?;
                found.block_log = (VHDX_SMALLEST_BLOCK_LOG..=VHDX_LARGEST_BLOCK_LOG)
                    .find(|log| 1u32 << log == le32(&bytes, 0))
                    .context("the VHDX block size is not a power of two of 1 to 256 MiB")?;
                found.flags = le32(&bytes, 4);
                ensure!(
                    found.flags >> 2 == 0,
                    "the VHDX file parameters set reserved flags"
                );
            }
            VIRTUAL_DISK_SIZE_GUID => {
                ensure!(!size_seen, "the VHDX names its disk size twice");
                found.size = le64(&value(8)?, 0);
                size_seen = true;
            }
            VIRTUAL_DISK_ID_GUID => {
                value(16)?;
            }
            LOGICAL_SECTOR_GUID | PHYSICAL_SECTOR_GUID => {
                let logical = item.guid == LOGICAL_SECTOR_GUID;
                ensure!(
                    if logical {
                        found.logical_log == 0
                    } else {
                        !physical_seen
                    },
                    "the VHDX names a sector size twice"
                );
                let log = match le32(&value(4)?, 0) {
                    512 => SECTOR_LOG,
                    4096 => FOUR_KIB_SECTOR_LOG,
                    _ => bail!("a VHDX sector size is not 512 or 4096 bytes"),
                };
                if logical {
                    found.logical_log = log;
                } else {
                    physical_seen = true;
                }
            }
            PARENT_LOCATOR_GUID => {
                ensure!(!found.locator, "the VHDX names two parent locators");
                ensure!(item.length >= 20, "the VHDX parent locator is too short");
                found.locator = true;
            }
            _ => ensure!(
                !item.required,
                "the VHDX requires metadata Zipper does not know"
            ),
        }
    }
    ensure!(
        found.block_log != 0 && found.logical_log != 0 && size_seen,
        "the VHDX metadata lacks its block size, sector size or disk size"
    );
    ensure!(
        found.size as u32 & (1 << found.logical_log) == 0 && found.size <= VHDX_LARGEST_DISK,
        "the VHDX disk size does not fit its sectors or is larger than 64 TiB"
    );
    Ok(found)
}

#[derive(Default)]
struct Megabytes {
    pages: HashMap<u64, Box<[u64; 512]>>,
}

impl Megabytes {
    fn claim(&mut self, unit: u64) -> bool {
        let page = self
            .pages
            .entry(unit >> 15)
            .or_insert_with(|| Box::new([0; 512]));
        let bit = unit & ((1 << 15) - 1);
        let word = &mut page[(bit >> 6) as usize];
        let mask = 1u64 << (bit & 63);
        let fresh = *word & mask == 0;
        *word |= mask;
        fresh
    }
}

fn open_vhdx(file: ArchiveFile, length: u64) -> Result<Opened> {
    let mut headers = Vec::with_capacity(2);
    for at in VHDX_HEADERS_AT {
        let bytes = read_at(&file, length, at, VHDX_HEADER, "VHDX header")?;
        headers.push(vhdx_header(&bytes).map_err(anyhow::Error::from)?);
    }
    let header = match headers[0].sequence.cmp(&headers[1].sequence) {
        std::cmp::Ordering::Greater => headers[0],
        std::cmp::Ordering::Less => headers[1],
        std::cmp::Ordering::Equal => {
            ensure!(headers[0] == headers[1], "the two VHDX headers disagree");
            headers[0]
        }
    };
    let mut limit = VHDX_FIRST_REGION_END;
    if header.log_length != 0 {
        limit = limit.max(
            header
                .log_offset
                .saturating_add(u64::from(header.log_length)),
        );
        ensure!(
            header.guids[2] == [0; 16],
            "the VHDX log holds updates that were never replayed; 7-Zip reads it without them"
        );
    }
    let tables = read_at(
        &file,
        length,
        VHDX_REGIONS_AT,
        2 * VHDX_REGION_TABLE,
        "VHDX region tables",
    )?;
    let (table, copy) = tables.split_at(VHDX_REGION_TABLE);
    ensure!(table == copy, "the two VHDX region tables differ");
    ensure!(
        le32(table, 0) == VHDX_REGION_SIGNATURE
            && crc_without_field(table, 4, VHDX_REGION_TABLE, crc32c),
        "the VHDX region table is damaged"
    );
    let count = le32(table, 8);
    ensure!(
        count <= VHDX_REGION_ENTRIES && le32(table, 12) == 0,
        "the VHDX region table header is malformed"
    );
    let (mut bat, mut metadata) = (None, None);
    for index in 0..count as usize {
        let at = 16 + VHDX_REGION_ENTRY * index;
        let region = vhdx_region(table[at..at + VHDX_REGION_ENTRY].try_into().unwrap())
            .map_err(anyhow::Error::from)?;
        limit = limit.max(region.offset + u64::from(region.length));
        let slot = match region.guid {
            BAT_GUID => &mut bat,
            METADATA_GUID => &mut metadata,
            _ => {
                ensure!(
                    !region.required,
                    "the VHDX requires a region Zipper does not know"
                );
                continue;
            }
        };
        ensure!(slot.is_none(), "the VHDX names one region twice");
        *slot = Some(region);
    }
    let metadata = read_vhdx_metadata(
        &file,
        length,
        metadata.context("the VHDX has no metadata region")?,
    )?;
    let bat = bat.context("the VHDX has no block table")?;
    ensure!(bat.length != 0, "the VHDX block table is empty");
    ensure!(
        bat.offset + u64::from(bat.length) <= length,
        "the VHDX block table lies past the end of the image"
    );
    if metadata.flags & HAS_PARENT != 0 {
        return Ok(Opened::Elsewhere(
            "the image is a differencing VHDX, which needs its parent disk".to_owned(),
        ));
    }
    ensure!(
        !metadata.locator,
        "the VHDX names a parent locator but has no parent"
    );
    let chunk_ratio_log = VHDX_BITMAP_LOG + 3 + metadata.logical_log - metadata.block_log;
    let block_size = 1u64 << metadata.block_log;
    let data_blocks = metadata
        .size
        .checked_add(block_size - 1)
        .context("the VHDX disk size overflows")?
        >> metadata.block_log;
    let entries = data_blocks + (data_blocks.wrapping_sub(1) >> chunk_ratio_log);
    ensure!(
        entries <= u64::from(bat.length) / 8,
        "the VHDX block table is shorter than its disk needs"
    );
    let mut largest = 0u64;
    let mut used = Megabytes::default();
    let mut batch = vec![0u8; (VHDX_BAT_BATCH * 8) as usize];
    let mut index = 0u64;
    while index < entries {
        let count = (entries - index).min(VHDX_BAT_BATCH);
        let bytes = &mut batch[..(count * 8) as usize];
        read_file_exact(&file, bytes, bat.offset + index * 8)
            .context("cannot read the VHDX block table")?;
        for (place, value) in bytes.chunks_exact(8).enumerate() {
            let position = index + place as u64;
            let sector_bitmap = (position + 1).is_multiple_of((1 << chunk_ratio_log) + 1);
            let (state, offset) =
                vhdx_bat_entry(le64(value, 0), sector_bitmap).map_err(anyhow::Error::from)?;
            if state == BLOCK_FULLY_PRESENT {
                ensure!(offset < VHDX_PHYSICAL_LIMIT, "a VHDX block lies past 2^62");
                largest = largest.max(offset);
                for unit in 0..block_size >> VHDX_BITMAP_LOG {
                    ensure!(
                        used.claim((offset >> VHDX_BITMAP_LOG) + unit),
                        "two VHDX blocks share their bytes in the file"
                    );
                }
            }
        }
        index += count;
    }
    if largest != 0 {
        limit = limit.max(largest + block_size);
    }
    ensure!(
        limit <= length,
        "the VHDX names blocks or regions past the end of the image"
    );
    let mut disk = Disk::plain(file, 0, metadata.size);
    disk.mapping = Mapping::Vhdx {
        table: bat.offset,
        table_bytes: u64::from(bat.length),
        block_log: metadata.block_log,
        chunk_ratio_log,
        limit,
    };
    Ok(Opened::Disk(disk))
}

fn file_system(view: &mut View, start: u64, size: u64) -> Result<Option<&'static str>> {
    if size < SECTOR as u64 {
        return Ok(None);
    }
    let mut sector = [0u8; SECTOR];
    if !view.read_if_there(start, &mut sector)? {
        return Ok(None);
    }
    let jump = match sector[0] {
        0xE9 => true,
        0xEB => sector[2] == 0x90,
        _ => false,
    };
    let signed = sector[BOOT_SIGNATURE_AT..] == [0x55, 0xAA];
    if signed && jump && &sector[3..12] == NTFS_SIGNATURE {
        return Ok(Some("NTFS"));
    }
    if signed && sector[..3] == [0xEB, 0x76, 0x90] && &sector[3..11] == EXFAT_SIGNATURE {
        return Ok(Some("exFAT"));
    }
    if native_fat::is_boot_sector(&sector) {
        return Ok(Some("FAT"));
    }
    if size >= UDF_AT + SECTOR as u64 && sector.iter().all(|byte| *byte == 0) {
        let mut rest = vec![0u8; UDF_AT as usize];
        if !view.read_if_there(start + SECTOR as u64, &mut rest)? {
            return Ok(None);
        }
        let at = UDF_AT as usize - SECTOR;
        if &rest[at..at + UDF_SIGNATURE.len()] == UDF_SIGNATURE {
            return Ok(Some("UDF"));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
struct MbrItem {
    entry: MbrEntry,
    real: bool,
    size: u64,
    file_system: Option<&'static str>,
    parsed: bool,
}

struct Mbr {
    items: Vec<MbrItem>,
    sector_log: u32,
    total: u64,
}

impl Mbr {
    fn position(&self, entry: &MbrEntry) -> u64 {
        u64::from(entry.lba) << self.sector_log
    }

    // EXTERNAL(7-Zip): `CHandler::ReadTables` in MbrHandler.cpp, followed rule for
    // rule, wrapping 32-bit sums included, since its items name the files it writes.
    fn read_tables(&mut self, view: &mut View, base: u32, lba: u32, level: u32) -> Result<bool> {
        if level >= MBR_DEPTH_LIMIT || self.items.len() >= MBR_ITEM_LIMIT {
            return Ok(false);
        }
        if level == 0 {
            self.sector_log = SECTOR_LOG;
        }
        let read = if level == 0 && self.total >= MBR_FIRST_READ as u64 {
            MBR_FIRST_READ
        } else {
            1 << self.sector_log
        };
        let at = u64::from(lba) << self.sector_log;
        let mut buffer = vec![0u8; read];
        if at + read as u64 > self.total || !view.read_if_there(at, &mut buffer)? {
            return Ok(false);
        }
        if buffer[BOOT_SIGNATURE_AT..BOOT_SIGNATURE_AT + 2] != [0x55, 0xAA] {
            return Ok(false);
        }
        let mut parts = [MbrEntry {
            kind: 0,
            lba: 0,
            blocks: 0,
        }; MBR_ENTRIES];
        for (index, part) in parts.iter_mut().enumerate() {
            let at = MBR_TABLE_AT + MBR_ENTRY * index;
            match mbr_entry(buffer[at..at + MBR_ENTRY].try_into().unwrap()) {
                Ok(entry) => *part = entry,
                Err(_) => return Ok(false),
            }
        }
        if level == 0 && read >= MBR_FIRST_READ {
            let (mut last_limit, mut first_lba, mut first_blocks) = (0u32, 0u32, 0u32);
            for part in parts.iter().filter(|part| !part.empty()) {
                if first_lba == 0 && part.blocks != 0 {
                    first_lba = part.lba;
                    first_blocks = part.blocks;
                }
                if part.limit() < last_limit {
                    return Ok(false);
                }
                last_limit = part.limit();
            }
            if last_limit != 0
                && u64::from(last_limit) << FOUR_KIB_SECTOR_LOG <= self.total
                && file_system(
                    view,
                    u64::from(first_lba) << FOUR_KIB_SECTOR_LOG,
                    u64::from(first_blocks) << FOUR_KIB_SECTOR_LOG,
                )?
                .is_some()
            {
                self.sector_log = FOUR_KIB_SECTOR_LOG;
            }
        }
        let Some(mut limit_lba) = lba.checked_add(1) else {
            return Ok(false);
        };
        for mut part in parts {
            if part.empty() {
                continue;
            }
            let count = self.items.len();
            let mut new_lba = lba.wrapping_add(part.lba);
            if part.extended() {
                new_lba = base.wrapping_add(part.lba);
                if new_lba < limit_lba {
                    return Ok(false);
                }
                self.read_tables(
                    view,
                    if level < 1 { new_lba } else { base },
                    new_lba,
                    level + 1,
                )?;
            }
            if new_lba < limit_lba {
                return Ok(false);
            }
            part.lba = new_lba;
            if !part.fits() {
                return Ok(false);
            }
            let mut item = MbrItem {
                entry: part,
                real: false,
                size: 0,
                file_system: None,
                parsed: false,
            };
            let mut add = false;
            if count == self.items.len() {
                item.real = true;
                add = true;
            } else {
                let back = self.items.last().unwrap().entry.limit();
                if back < part.limit() {
                    item.entry.lba = back;
                    item.entry.blocks = part.limit() - back;
                    add = true;
                }
            }
            if add {
                if item.entry.limit() < limit_lba {
                    return Ok(false);
                }
                limit_lba = item.entry.limit();
                item.size = u64::from(item.entry.blocks) << self.sector_log;
                self.items.push(item);
            }
        }
        Ok(true)
    }
}

fn mbr(view: &mut View) -> Result<Option<Mbr>> {
    let mut mbr = Mbr {
        items: Vec::new(),
        sector_log: SECTOR_LOG,
        total: view.length,
    };
    if !mbr.read_tables(view, 0, 0, 0)? || mbr.items.is_empty() {
        return Ok(None);
    }
    let limit = mbr.items.last().unwrap().entry.limit();
    let end = u64::from(limit) << mbr.sector_log;
    if end < mbr.total {
        mbr.items.push(MbrItem {
            entry: MbrEntry {
                kind: 0,
                lba: limit,
                blocks: 0,
            },
            real: false,
            size: mbr.total - end,
            file_system: None,
            parsed: false,
        });
    }
    for index in 0..mbr.items.len() {
        if mbr.items[index].entry.kind == MBR_NTFS {
            let item = mbr.items[index];
            mbr.items[index].file_system = file_system(view, mbr.position(&item.entry), item.size)?;
            mbr.items[index].parsed = true;
        }
    }
    Ok(Some(mbr))
}

fn mbr_name(index: usize, item: &MbrItem) -> String {
    if !item.real {
        return index.to_string();
    }
    let extension = match item.file_system {
        Some(name) => name.to_ascii_lowercase(),
        None if item.parsed => "img".to_owned(),
        None => MBR_TYPES
            .iter()
            .find(|(kind, _)| *kind == item.entry.kind)
            .and_then(|(_, extension)| *extension)
            .unwrap_or("img")
            .to_owned(),
    };
    format!("{index}.{extension}")
}

struct GptItem {
    name: String,
    position: u64,
    size: u64,
}

fn gpt(view: &mut View) -> Result<Option<Vec<GptItem>>> {
    let mut head = vec![0u8; GPT_HEAD_READ];
    if !view.read_if_there(0, &mut head)?
        || head[BOOT_SIGNATURE_AT..BOOT_SIGNATURE_AT + 2] != [0x55, 0xAA]
    {
        return Ok(None);
    }
    let Some(sector_log) = [SECTOR_LOG, FOUR_KIB_SECTOR_LOG]
        .into_iter()
        .find(|log| head[1 << log..].starts_with(GPT_SIGNATURE))
    else {
        return Ok(None);
    };
    let sector = &head[1 << sector_log..(2 << sector_log)];
    let Ok(header) = gpt_header(sector[..0x5C].try_into().unwrap(), sector_log) else {
        return Ok(None);
    };
    if !crc_without_field(sector, 0x10, header.header_size as usize, crc32fast::hash) {
        return Ok(None);
    }
    let table_size = u64::from(header.entry_size) * u64::from(header.entries);
    let table_read = table_size.next_multiple_of(1 << sector_log);
    let table_at = header.table_lba << sector_log;
    let mut hasher = crc32fast::Hasher::new();
    let mut record = vec![0u8; header.entry_size as usize];
    let mut entries = Vec::new();
    let mut position = table_at;
    for _ in 0..header.entries {
        if !view.read_if_there(position, &mut record)? {
            return Ok(None);
        }
        position += u64::from(header.entry_size);
        hasher.update(&record);
        match gpt_entry(record[..GPT_ENTRY_READ].try_into().unwrap(), sector_log) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(_) => return Ok(None),
        }
    }
    let mut padding = vec![0u8; (table_read - table_size) as usize];
    if !view.read_if_there(position, &mut padding)? || hasher.finalize() != header.entries_crc {
        return Ok(None);
    }
    if header.backup_lba >> (63 - sector_log) != 0 {
        return Ok(None);
    }
    let mut items = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let position = entry.first << sector_log;
        let size = (entry.last - entry.first + 1) << sector_log;
        let known = GPT_TYPES.iter().find(|(kind, _, _)| *kind == entry.kind);
        let extension = match known {
            Some((_, Some(extension), _)) => Some(*extension),
            Some((_, None, true)) => file_system(view, position, size)?,
            _ => None,
        };
        let units: Vec<u16> = entry
            .name
            .iter()
            .copied()
            .take_while(|unit| *unit != 0)
            .collect();
        let Ok(label) = String::from_utf16(&units) else {
            bail!("GPT partition {index} has a name that is not valid UTF-16");
        };
        let mut name = index.to_string();
        if !label.is_empty() {
            name.push('.');
            name.push_str(&label);
        }
        name.push('.');
        name.push_str(&extension.map_or("img".to_owned(), str::to_ascii_lowercase));
        items.push(GptItem {
            name,
            position,
            size,
        });
    }
    Ok(Some(items))
}

fn read_sector(view: &mut View) -> Result<Option<[u8; SECTOR]>> {
    let mut sector = [0u8; SECTOR];
    Ok((view.length >= SECTOR as u64 && view.read_if_there(0, &mut sector)?).then_some(sector))
}

fn plan_span(disk: &mut Disk, span: Span, depth: u32) -> Result<Plan> {
    if depth >= NESTING_LIMIT {
        return Ok(Plan::Elsewhere(
            "the disk nests partition tables too deeply".to_owned(),
        ));
    }
    let gpt_items = gpt(&mut span.view(disk))?;
    if let Some(items) = gpt_items {
        if let [item] = &items[..] {
            return plan_span(disk, span.inner(item.position, item.size), depth + 1);
        }
        let mut files = Vec::with_capacity(items.len());
        for item in items {
            if item.name.contains(['/', '\\', ':']) || item.name.chars().any(char::is_control) {
                return Ok(Plan::Elsewhere(format!(
                    "GPT partition name {:?} is not one plain file name",
                    item.name
                )));
            }
            files.push(RawItem {
                name: item.name,
                span: span.inner(item.position, item.size),
            });
        }
        return Ok(Plan::Files {
            format: "GPT",
            items: files,
        });
    }
    let sector = read_sector(&mut span.view(disk))?;
    if sector.is_some_and(|sector| native_fat::is_boot_sector(&sector)) {
        return Ok(Plan::FileSystem(span));
    }
    let found = mbr(&mut span.view(disk))?;
    if let Some(mbr) = found {
        let mut real = mbr.items.iter().filter(|item| item.real);
        if let (Some(lone), None) = (real.next(), real.next()) {
            return plan_span(
                disk,
                span.inner(mbr.position(&lone.entry), lone.size),
                depth + 1,
            );
        }
        let items = mbr
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| RawItem {
                name: mbr_name(index, item),
                span: span.inner(mbr.position(&item.entry), item.size),
            })
            .collect();
        return Ok(Plan::Files {
            format: "MBR",
            items,
        });
    }
    Ok(Plan::Elsewhere(
        match file_system(&mut span.view(disk), 0, span.length)? {
            Some(name) => format!("the disk holds {name}"),
            None => "the disk holds no partition table or file system that Zipper knows".to_owned(),
        },
    ))
}

fn plan(path: &Path) -> Result<(Option<Disk>, Plan)> {
    match open(path)? {
        Opened::Elsewhere(reason) => Ok((None, Plan::Elsewhere(reason))),
        Opened::Disk(mut disk) => {
            let whole = Span {
                start: 0,
                length: disk.size,
                end: disk.size,
            };
            let plan = plan_span(&mut disk, whole, 0)?;
            Ok((Some(disk), plan))
        }
    }
}

fn raw_disk_is_claimed(head: &[u8], tail: &[u8]) -> bool {
    if head.len() < SECTOR
        || head[BOOT_SIGNATURE_AT..BOOT_SIGNATURE_AT + 2] != [0x55, 0xAA]
        || tail.starts_with(DMG_SIGNATURE)
    {
        return false;
    }
    let sector: &[u8; SECTOR] = head[..SECTOR].try_into().unwrap();
    if native_fat::is_boot_sector(sector) {
        return true;
    }
    if [SECTOR, 1 << FOUR_KIB_SECTOR_LOG]
        .iter()
        .any(|at| head.get(*at..at + GPT_SIGNATURE.len()) == Some(GPT_SIGNATURE))
    {
        return true;
    }
    let foreign = head.get(HFS_AT..HFS_AT + 4).is_some_and(|bytes| {
        bytes.starts_with(b"BD") || bytes == b"H+\0\x04" || bytes == b"HX\0\x05"
    }) || head.get(EXT_SIGNATURE_AT..EXT_SIGNATURE_AT + 2) == Some(&[0x53, 0xEF]);
    let mut nonempty = false;
    let table_holds = (0..MBR_ENTRIES).all(|index| {
        let at = MBR_TABLE_AT + MBR_ENTRY * index;
        mbr_entry(head[at..at + MBR_ENTRY].try_into().unwrap()).is_ok_and(|entry| {
            nonempty |= !entry.empty();
            true
        })
    });
    !foreign && table_holds && nonempty
}

/// Returns whether the file is a VHD or VHDX image, or a raw disk that
/// starts with an MBR or GPT partition table or a FAT boot sector. A file
/// that only ends with a fixed VHD footer counts when it is named `.vhd`.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    if length < SECTOR as u64 {
        return Ok(false);
    }
    let mut head = vec![0u8; (length.min(SNIFF_READ as u64)) as usize];
    read_file_exact(&file, &mut head, 0)?;
    if head.starts_with(VHDX_SIGNATURE) || head.starts_with(VHD_SIGNATURE) {
        return Ok(true);
    }
    let mut tail = [0u8; VHD_FOOTER];
    read_file_exact(&file, &mut tail, length - VHD_FOOTER as u64)?;
    if has_vhd_extension(path) && vhd_footer(&tail).is_ok_and(|footer| footer.kind == VHD_FIXED) {
        return Ok(true);
    }
    Ok(raw_disk_is_claimed(&head, &tail))
}

/// Returns whether the default engine should send the image to 7-Zip: a
/// differencing VHD or VHDX, or a disk whose file system or partition names
/// Zipper does not read in process, such as NTFS.
pub fn needs_backend(path: &Path) -> Result<bool> {
    Ok(matches!(plan(path), Ok((_, Plan::Elsewhere(_)))))
}

/// Verify and optionally extract a disk image into a new directory, in the
/// tree `7z x` extracts. A disk with one partition, or a VHD or VHDX holding
/// one, extracts the FAT file system inside. A disk with several partitions
/// extracts each partition, and each gap between them, as one file.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let (disk, plan) = plan(path)?;
    match plan {
        Plan::Elsewhere(reason) => {
            bail!("{reason}; Zipper does not read it in process, so {BACKEND_HINT}")
        }
        Plan::FileSystem(span) => {
            let mut disk = disk.expect("a file system plan comes from an opened disk");
            let mut volume = native_fat::Volume::open(span.view(&mut disk))?;
            tree::extract("FAT", path, destination, options, |tree| {
                volume.extract(tree)
            })
        }
        Plan::Files { format, items } => {
            let mut disk = disk.expect("a partition plan comes from an opened disk");
            tree::extract(format, path, destination, options, |tree| {
                for item in &items {
                    tree.check()?;
                    let mut view = item.span.view(&mut disk);
                    tree.file(&item.name, item.span.length, RAW_FILE_MODE, None, &mut view)?;
                }
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn crc32c_matches_its_published_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn every_block_maps_to_a_payload_entry_before_its_chunks_bitmap() {
        for chunk_ratio_log in 0..6 {
            let ratio = 1u64 << chunk_ratio_log;
            let mut seen = std::collections::HashSet::new();
            for block in 0..200 {
                let index = vhdx_bat_index(block, chunk_ratio_log);
                assert_ne!(
                    (index + 1) % (ratio + 1),
                    0,
                    "block {block} landed on a bitmap entry"
                );
                assert!(index < block + 1 + block / ratio);
                assert!(seen.insert(index));
            }
        }
    }

    #[test]
    fn mbr_entries_check_their_status_chs_and_limits() {
        let mut entry = [0u8; MBR_ENTRY];
        assert_eq!(mbr_entry(&entry).unwrap().kind, 0);
        entry[4] = 6;
        entry[1..4].copy_from_slice(&[0, 2, 0]);
        entry[8..12].copy_from_slice(&2048u32.to_le_bytes());
        entry[12..16].copy_from_slice(&100u32.to_le_bytes());
        assert_eq!(mbr_entry(&entry).unwrap().limit(), 2148);
        entry[0] = 1;
        assert!(mbr_entry(&entry).is_err());
        entry[0] = 0x80;
        entry[2] = 0x40;
        assert!(mbr_entry(&entry).is_err());
        entry[2] = 0x41;
        entry[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(mbr_entry(&entry).is_err());
    }

    #[test]
    fn vhd_bitmaps_cover_every_sector_of_their_block() {
        for log in VHD_SMALLEST_BLOCK_LOG..=VHD_LARGEST_BLOCK_LOG {
            let bytes = u64::from(vhd_bitmap_bytes(log));
            assert!(bytes * 8 >= (1u64 << log) / 512);
            assert_eq!(bytes % 512, 0);
        }
        assert!(vhd_blocks_fit(0, 0, 21));
        assert!(!vhd_blocks_fit(0, 1, 21));
        assert!(vhd_blocks_fit(1, 1, 21));
        assert!(vhd_blocks_fit(2 << 21, 2, 21));
        assert!(!vhd_blocks_fit((2 << 21) + 1, 2, 21));
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(12)]
    fn vhd_footers_never_panic_and_keep_their_limits() {
        let bytes: [u8; VHD_FOOTER_FIELDS] = kani::any();
        if let Ok(footer) = vhd_footer_fields(&bytes) {
            assert!(footer.size <= VHD_LARGEST_DISK);
            assert!((VHD_FIXED..=VHD_DIFFERENCING).contains(&footer.kind));
            assert!(footer.kind != VHD_FIXED || footer.data_offset == u64::MAX);
            assert!(footer.kind == VHD_FIXED || footer.data_offset < VHD_DATA_OFFSET_LIMIT);
        }
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn vhd_checksums_never_panic_or_read_past_their_block() {
        let bytes: [u8; 12] = kani::any();
        let at: usize = kani::any();
        kani::assume(at <= bytes.len() - 4);
        let _ = vhd_checksum_holds(&bytes, at);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn dynamic_vhd_headers_never_panic_and_name_blocks_their_bitmaps_cover() {
        let bytes: [u8; VHD_SPARSE_FIELDS] = kani::any();
        if let Ok(sparse) = vhd_sparse_fields(&bytes) {
            assert!((VHD_SMALLEST_BLOCK_LOG..=VHD_LARGEST_BLOCK_LOG).contains(&sparse.block_log));
            let bitmap = u64::from(vhd_bitmap_bytes(sparse.block_log));
            assert!(bitmap * 8 * 512 >= 1u64 << sparse.block_log);
            let size: u64 = kani::any();
            kani::assume(size <= VHD_LARGEST_DISK);
            if vhd_blocks_fit(size, sparse.blocks, sparse.block_log) {
                assert!(u64::from(sparse.blocks) << sparse.block_log >= size);
                let offset: u64 = kani::any();
                kani::assume(offset < size);
                assert!(offset >> sparse.block_log < u64::from(sparse.blocks));
            }
        }
    }

    #[kani::proof]
    fn vhdx_header_fields_never_panic_and_keep_the_log_aligned() {
        let bytes: [u8; VHDX_HEADER_FIELDS] = kani::any();
        if let Ok(header) = vhdx_header_fields(&bytes) {
            assert!(u64::from(header.log_length) & VHDX_ALIGNMENT_MASK == 0);
            assert!(header.log_offset & VHDX_ALIGNMENT_MASK == 0);
        }
    }

    #[kani::proof]
    fn vhdx_regions_and_metadata_items_stay_inside_their_bounds() {
        let region: [u8; VHDX_REGION_ENTRY] = kani::any();
        if let Ok(found) = vhdx_region(&region) {
            assert!(found.offset.checked_add(u64::from(found.length)).is_some());
            assert!(found.offset & VHDX_ALIGNMENT_MASK == 0);
        }
        let item: [u8; VHDX_METADATA_ENTRY] = kani::any();
        let size: u32 = kani::any();
        if let Ok(found) = vhdx_item(&item, size) {
            assert!(u64::from(found.offset) + u64::from(found.length) <= u64::from(size));
            assert!(found.length <= VHDX_METADATA_ITEM_LIMIT);
        }
    }

    #[kani::proof]
    fn vhdx_block_table_entries_keep_their_state_and_every_block_finds_a_payload_entry() {
        let value: u64 = kani::any();
        let sector_bitmap: bool = kani::any();
        if let Ok((state, offset)) = vhdx_bat_entry(value, sector_bitmap) {
            assert!(offset & VHDX_ALIGNMENT_MASK == 0);
            assert!(!sector_bitmap || state == BLOCK_NOT_PRESENT);
            assert!(state != BLOCK_FULLY_PRESENT || offset != 0);
            assert!(state != BLOCK_PARTIALLY_PRESENT);
        }
        let chunk_ratio_log: u32 = kani::any();
        kani::assume((4..=15).contains(&chunk_ratio_log));
        let blocks: u64 = kani::any();
        kani::assume(blocks >= 1 && blocks <= VHDX_LARGEST_DISK >> VHDX_SMALLEST_BLOCK_LOG);
        let block: u64 = kani::any();
        kani::assume(block < blocks);
        let index = vhdx_bat_index(block, chunk_ratio_log);
        assert!(index < blocks + ((blocks - 1) >> chunk_ratio_log));
        assert!((index + 1) % ((1 << chunk_ratio_log) + 1) != 0);
    }

    #[kani::proof]
    fn mbr_entries_never_panic_and_end_inside_32_bit_sectors() {
        let bytes: [u8; MBR_ENTRY] = kani::any();
        if let Ok(entry) = mbr_entry(&bytes) {
            if !entry.empty() {
                assert!(entry.blocks > 0);
                assert!(u64::from(entry.lba) + u64::from(entry.blocks) <= u64::from(u32::MAX));
                assert!(entry.limit() == entry.lba + entry.blocks);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(37)]
    fn gpt_headers_and_entries_never_panic_and_stay_inside_the_disk() {
        let sector_log = if kani::any() {
            SECTOR_LOG
        } else {
            FOUR_KIB_SECTOR_LOG
        };
        let header: [u8; 0x5C] = kani::any();
        if let Ok(found) = gpt_header(&header, sector_log) {
            assert!(found.header_size <= 1 << sector_log);
            assert!(u64::from(found.entries) * u64::from(found.entry_size) <= 1 << 28);
            assert!(found.table_lba << sector_log >> sector_log == found.table_lba);
        }
        let entry: [u8; GPT_ENTRY_READ] = kani::any();
        if let Ok(Some(found)) = gpt_entry(&entry, sector_log) {
            assert!(found.last >= found.first);
            let end = (found.last + 1) << sector_log;
            assert!(end >> sector_log == found.last + 1);
            assert!(((found.last - found.first + 1) << sector_log) <= end);
        }
    }
}
