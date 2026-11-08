use crate::hfs::Volume;
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
    time::{Duration, SystemTime},
};

pub(crate) const SUPERBLOCK_AT: usize = 1024;
pub(crate) const SUPERBLOCK: usize = 1024;
pub(crate) const SUPERBLOCK_FIELDS: usize = 0x280;
const CHECKSUM_AT: usize = 0x3FC;
const MAGIC: u16 = 0xEF53;
const SMALLEST_BLOCK_LOG: u32 = 10;
const LARGEST_BLOCK_LOG: u32 = 16;
const GOOD_OLD_INODE: usize = 128;
const GOOD_OLD_FIRST_INODE: u32 = 11;
const SMALL_DESCRIPTOR: u32 = 32;
const LARGE_DESCRIPTOR: u32 = 64;
const LARGEST_DESCRIPTOR: u32 = 1024;
const CHECKSUM_CRC32C: u8 = 1;

const COMPAT_SPARSE_SUPER2: u32 = 0x200;

const INCOMPAT_COMPRESSION: u32 = 0x1;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_JOURNAL_DEV: u32 = 0x8;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_MMP: u32 = 0x100;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_EA_INODE: u32 = 0x400;
const INCOMPAT_DIRDATA: u32 = 0x1000;
const INCOMPAT_CSUM_SEED: u32 = 0x2000;
const INCOMPAT_LARGEDIR: u32 = 0x4000;
const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_ENCRYPT: u32 = 0x10000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;
const INCOMPAT_READ: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_META_BG
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_MMP
    | INCOMPAT_FLEX_BG
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR
    | INCOMPAT_INLINE_DATA
    | INCOMPAT_CASEFOLD;

const RO_COMPAT_SPARSE_SUPER: u32 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;
const RO_COMPAT_BTREE_DIR: u32 = 0x4;
const RO_COMPAT_HUGE_FILE: u32 = 0x8;
const RO_COMPAT_GDT_CSUM: u32 = 0x10;
const RO_COMPAT_DIR_NLINK: u32 = 0x20;
const RO_COMPAT_EXTRA_ISIZE: u32 = 0x40;
const RO_COMPAT_QUOTA: u32 = 0x100;
const RO_COMPAT_BIGALLOC: u32 = 0x200;
const RO_COMPAT_METADATA_CSUM: u32 = 0x400;
const RO_COMPAT_READONLY: u32 = 0x1000;
const RO_COMPAT_PROJECT: u32 = 0x2000;
const RO_COMPAT_SHARED_BLOCKS: u32 = 0x4000;
const RO_COMPAT_VERITY: u32 = 0x8000;
const RO_COMPAT_ORPHAN_PRESENT: u32 = 0x10000;
const RO_COMPAT_READ: u32 = RO_COMPAT_SPARSE_SUPER
    | RO_COMPAT_LARGE_FILE
    | RO_COMPAT_BTREE_DIR
    | RO_COMPAT_HUGE_FILE
    | RO_COMPAT_GDT_CSUM
    | RO_COMPAT_DIR_NLINK
    | RO_COMPAT_EXTRA_ISIZE
    | RO_COMPAT_QUOTA
    | RO_COMPAT_METADATA_CSUM
    | RO_COMPAT_READONLY
    | RO_COMPAT_PROJECT
    | RO_COMPAT_VERITY;

const GROUP_INODE_UNINIT: u16 = 0x1;

const INODE_FLAG_ENCRYPT: u32 = 0x800;
const INODE_FLAG_INDEX: u32 = 0x1000;
const INODE_FLAG_HUGE_FILE: u32 = 0x4_0000;
const INODE_FLAG_EXTENTS: u32 = 0x8_0000;
const INODE_FLAG_EA_INODE: u32 = 0x20_0000;
const INODE_FLAG_INLINE_DATA: u32 = 0x1000_0000;

const TYPE_BITS: u16 = 0o170_000;
const FIFO: u16 = 0o010_000;
const CHARACTER: u16 = 0o020_000;
const DIRECTORY: u16 = 0o040_000;
const BLOCK_DEVICE: u16 = 0o060_000;
const REGULAR: u16 = 0o100_000;
const SYMLINK: u16 = 0o120_000;
const SOCKET: u16 = 0o140_000;
const DEFAULT_FILE_MODE: u32 = 0o666;

pub(crate) const BLOCK_POINTERS: usize = 15;
const DIRECT_POINTERS: usize = 12;
const I_BLOCK: usize = 0x28;
const I_BLOCK_BYTES: usize = 4 * BLOCK_POINTERS;
const EXTRA_ISIZE_AT: usize = 0x80;
const CHECKSUM_HI_END: usize = 0x84;
const MTIME_EXTRA_END: usize = 0x8C;

pub(crate) const EXTENT_HEADER: usize = 12;
pub(crate) const EXTENT_ENTRY: usize = 12;
const EXTENT_MAGIC: u16 = 0xF30A;
const EXTENT_TAIL: usize = 4;
const MAX_EXTENT_DEPTH: u16 = 5;
const LARGEST_INITIALIZED_EXTENT: u16 = 32768;

pub(crate) const DIR_ENTRY_HEADER: usize = 8;
const DIR_TAIL: usize = 12;
const MIN_DIR_RECORD: usize = 12;
const DIR_TAIL_TYPE: u8 = 0xDE;
const INLINE_PARENT: usize = 4;

const XATTR_MAGIC: u32 = 0xEA02_0000;
const XATTR_HEADER: usize = 4;
pub(crate) const XATTR_ENTRY: usize = 16;
const XATTR_SYSTEM: u8 = 7;
const INLINE_DATA_NAME: &[u8] = b"data";

const ROOT: u32 = 2;
const RESIZE: u32 = 7;
const SYSTEM_NAMES: [(u32, &str); 8] = [
    (1, "Bad"),
    (3, "UserQuota"),
    (4, "GroupQuota"),
    (5, "BootLoader"),
    (6, "Undelete"),
    (8, "Journal"),
    (9, "Exclude"),
    (10, "Replica"),
];
const SYSTEM_FOLDER: &str = "[SYS]";
const UNKNOWN_FOLDER: &str = "[UNKNOWN]";

const MAX_INODES: usize = tree::MAX_ENTRIES + 16;
const MAX_RUNS: usize = 1 << 24;
const MAX_NAMES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 1024;
const UNINITIALIZED: u64 = 1 << 63;
const READ_PIECE: usize = 1 << 20;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
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

// SPEC(ext4 Checksums): metadata checksums are CRC32C without the final inversion,
// chained from the seed the caller gives.
pub(crate) fn crc32c(seed: u32, bytes: &[u8]) -> u32 {
    let mut crc = seed;
    for byte in bytes {
        crc = CRC32C[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

const fn crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u16;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xA001 & (0u16.wrapping_sub(value & 1)));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

static CRC16: [u16; 256] = crc16_table();

// SPEC(ext4 Block Group Descriptors): without metadata_csum, uninit_bg descriptors carry
// the reflected CRC-16 of polynomial 0x8005, chained from the seed the caller gives.
pub(crate) fn crc16(seed: u16, bytes: &[u8]) -> u16 {
    let mut crc = seed;
    for byte in bytes {
        crc = CRC16[((crc ^ u16::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Superblock {
    pub(crate) block_log: u32,
    pub(crate) blocks: u64,
    pub(crate) first_data_block: u32,
    pub(crate) blocks_per_group: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) groups: u32,
    pub(crate) inode_size: u32,
    pub(crate) first_inode: u32,
    pub(crate) descriptor_size: u32,
    pub(crate) reserved_descriptor_blocks: u32,
    pub(crate) first_meta_group: u32,
    pub(crate) backup_groups: [u32; 2],
    pub(crate) compat: u32,
    pub(crate) incompat: u32,
    pub(crate) ro_compat: u32,
    pub(crate) uuid: [u8; 16],
    pub(crate) checksum_type: u8,
    pub(crate) checksum_seed: u32,
    pub(crate) last_orphan: u32,
}

impl Superblock {
    fn block_size(&self) -> usize {
        1 << self.block_log
    }

    fn metadata_checksums(&self) -> bool {
        self.ro_compat & RO_COMPAT_METADATA_CSUM != 0
    }

    fn descriptor_checksums(&self) -> bool {
        self.ro_compat & (RO_COMPAT_METADATA_CSUM | RO_COMPAT_GDT_CSUM) != 0
    }

    fn file_types(&self) -> bool {
        self.incompat & INCOMPAT_FILETYPE != 0
    }

    fn descriptors_per_block(&self) -> u32 {
        (1u32 << self.block_log) / self.descriptor_size
    }

    fn group_start(&self, group: u32) -> u64 {
        u64::from(self.first_data_block) + u64::from(group) * u64::from(self.blocks_per_group)
    }

    fn holds_superblock(&self, group: u32) -> bool {
        if group == 0 {
            return true;
        }
        if self.compat & COMPAT_SPARSE_SUPER2 != 0 {
            return self.backup_groups.contains(&group);
        }
        if self.ro_compat & RO_COMPAT_SPARSE_SUPER == 0 || group == 1 {
            return true;
        }
        [3u32, 5, 7].iter().any(|&base| {
            let mut power = base;
            while power < group {
                power = power.saturating_mul(base);
            }
            power == group
        })
    }

    fn descriptor_blocks(&self) -> u32 {
        self.groups.div_ceil(self.descriptors_per_block())
    }

    // SPEC(ext4 Meta Block Groups): with meta_bg, each run of one block's worth of groups
    // keeps its descriptors in the first block after its first group's superblock copy.
    fn descriptor_place(&self, group: u32) -> (u64, usize) {
        let per_block = self.descriptors_per_block();
        let meta_group = group / per_block;
        let offset = (group % per_block * self.descriptor_size) as usize;
        if self.incompat & INCOMPAT_META_BG != 0 && meta_group >= self.first_meta_group {
            let first = meta_group * per_block;
            let block = self.group_start(first) + u64::from(self.holds_superblock(first));
            (block, offset)
        } else {
            (
                u64::from(self.first_data_block) + 1 + u64::from(meta_group),
                offset,
            )
        }
    }

    fn largest_block_map_file(&self) -> u64 {
        let pointers = 1u64 << (self.block_log - 2);
        let blocks = DIRECT_POINTERS as u64 + pointers + pointers * pointers;
        blocks
            .saturating_add(pointers.saturating_mul(pointers).saturating_mul(pointers))
            .min(1 << 32)
            << self.block_log
    }
}

pub(crate) fn superblock_layout(bytes: &[u8; SUPERBLOCK_FIELDS]) -> Field<Superblock> {
    field_check(
        le16(bytes, 0x38) == MAGIC,
        "the ext superblock has no signature",
    )?;
    let block_log = le32(bytes, 0x18);
    field_check(
        block_log <= LARGEST_BLOCK_LOG - SMALLEST_BLOCK_LOG,
        "the ext block size is larger than 64 KiB",
    )?;
    let block_log = block_log + SMALLEST_BLOCK_LOG;
    let ro_compat = le32(bytes, 0x64);
    field_check(
        ro_compat & RO_COMPAT_BIGALLOC != 0 || le32(bytes, 0x1C) == block_log - SMALLEST_BLOCK_LOG,
        "the ext cluster size differs from the block size",
    )?;
    let first_data_block = le32(bytes, 0x14);
    field_check(
        first_data_block == u32::from(block_log == SMALLEST_BLOCK_LOG),
        "the ext first data block does not follow the boot block",
    )?;
    let bits_per_block = 8u32 << block_log;
    let blocks_per_group = le32(bytes, 0x20);
    field_check(
        blocks_per_group >= 8 && blocks_per_group <= bits_per_block,
        "the ext blocks per group do not fit a bitmap block",
    )?;
    field_check(
        ro_compat & RO_COMPAT_BIGALLOC != 0 || le32(bytes, 0x24) == blocks_per_group,
        "the ext clusters per group differ from the blocks per group",
    )?;
    let inodes_per_group = le32(bytes, 0x28);
    field_check(
        inodes_per_group >= 1 && inodes_per_group <= bits_per_block,
        "the ext inodes per group do not fit a bitmap block",
    )?;
    let revision = le32(bytes, 0x4C);
    field_check(revision <= 1, "the ext revision is not 0 or 1")?;
    let (inode_size, first_inode) = if revision == 0 {
        (GOOD_OLD_INODE as u32, GOOD_OLD_FIRST_INODE)
    } else {
        (u32::from(le16(bytes, 0x58)), le32(bytes, 0x54))
    };
    field_check(
        inode_size.is_power_of_two()
            && inode_size >= GOOD_OLD_INODE as u32
            && inode_size <= 1 << block_log,
        "the ext inode size is not a power of two from 128 bytes to the block size",
    )?;
    field_check(
        first_inode == GOOD_OLD_FIRST_INODE,
        "the ext first inode is not 11",
    )?;
    let incompat = le32(bytes, 0x60);
    let wide = incompat & INCOMPAT_64BIT != 0;
    let blocks = u64::from(le32(bytes, 0x04))
        | if wide {
            u64::from(le32(bytes, 0x150)) << 32
        } else {
            0
        };
    field_check(
        blocks > u64::from(first_data_block),
        "the ext volume has no blocks past its first",
    )?;
    field_check(
        blocks <= u64::MAX >> block_log,
        "the ext volume is larger than 2^64 bytes",
    )?;
    let groups = (blocks - u64::from(first_data_block)).div_ceil(u64::from(blocks_per_group));
    let inodes = le32(bytes, 0x00);
    field_check(
        groups.checked_mul(u64::from(inodes_per_group)) == Some(u64::from(inodes)),
        "the ext inode count is not the group count times the inodes per group",
    )?;
    let groups = groups as u32;
    let descriptor_size = if wide {
        u32::from(le16(bytes, 0xFE))
    } else {
        SMALL_DESCRIPTOR
    };
    field_check(
        descriptor_size.is_power_of_two()
            && descriptor_size
                >= if wide {
                    LARGE_DESCRIPTOR
                } else {
                    SMALL_DESCRIPTOR
                }
            && descriptor_size <= LARGEST_DESCRIPTOR.min(1 << block_log),
        "the ext group descriptor size is not a power of two from 64 bytes to the block size",
    )?;
    let first_meta_group = le32(bytes, 0x104);
    let reserved_descriptor_blocks = u32::from(le16(bytes, 0xCE));
    let checksum_type = bytes[0x175];
    let uuid: [u8; 16] = bytes[0x68..0x78].try_into().unwrap();
    let checksum_seed = if incompat & INCOMPAT_CSUM_SEED != 0 {
        le32(bytes, 0x270)
    } else {
        0
    };
    Ok(Superblock {
        block_log,
        blocks,
        first_data_block,
        blocks_per_group,
        inodes_per_group,
        groups,
        inode_size,
        first_inode,
        descriptor_size,
        reserved_descriptor_blocks,
        first_meta_group,
        backup_groups: [le32(bytes, 0x24C), le32(bytes, 0x250)],
        compat: le32(bytes, 0x5C),
        incompat,
        ro_compat,
        uuid,
        checksum_type,
        checksum_seed,
        last_orphan: le32(bytes, 0xE8),
    })
}

fn refused_feature(fields: &Superblock) -> Option<String> {
    let named = [
        (
            fields.incompat & INCOMPAT_RECOVER,
            "a journal that needs recovery",
        ),
        (fields.incompat & INCOMPAT_ENCRYPT, "encryption"),
        (fields.incompat & INCOMPAT_COMPRESSION, "compression"),
        (
            fields.incompat & INCOMPAT_JOURNAL_DEV,
            "the journal device layout",
        ),
        (
            fields.incompat & INCOMPAT_EA_INODE,
            "extended attributes in inodes",
        ),
        (
            fields.incompat & INCOMPAT_DIRDATA,
            "data in directory entries",
        ),
        (
            fields.ro_compat & RO_COMPAT_BIGALLOC,
            "clusters larger than a block",
        ),
        (fields.ro_compat & RO_COMPAT_SHARED_BLOCKS, "shared blocks"),
        (
            fields.ro_compat & RO_COMPAT_ORPHAN_PRESENT | fields.last_orphan,
            "orphan inodes that need recovery",
        ),
    ];
    if let Some((_, name)) = named.iter().find(|(bit, _)| *bit != 0) {
        return Some((*name).to_owned());
    }
    let unknown = fields.incompat & !INCOMPAT_READ;
    if unknown != 0 {
        return Some(format!("the incompatible feature 0x{unknown:X}"));
    }
    let unknown = fields.ro_compat & !RO_COMPAT_READ;
    if unknown != 0 {
        return Some(format!("the read-only feature 0x{unknown:X}"));
    }
    if fields.metadata_checksums() && fields.checksum_type != CHECKSUM_CRC32C {
        return Some(format!("checksum type {}", fields.checksum_type));
    }
    None
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Group {
    pub(crate) inode_bitmap: u64,
    pub(crate) inode_table: u64,
    pub(crate) free_inodes: u32,
    pub(crate) flags: u16,
    pub(crate) unused_inodes: u32,
    pub(crate) inode_bitmap_checksum: u32,
}

// SPEC(ext4 Block Group Descriptors): the low half of each field, and with 64bit the
// high half in the descriptor's second 32 bytes.
pub(crate) fn group_fields(bytes: &[u8], feature_64bit: bool) -> Field<Group> {
    field_check(
        bytes.len() >= SMALL_DESCRIPTOR as usize
            && (!feature_64bit || bytes.len() >= LARGE_DESCRIPTOR as usize),
        "the ext group descriptor is too short",
    )?;
    let high = |at: usize| if feature_64bit { le32(bytes, at) } else { 0 };
    let high16 = |at: usize| if feature_64bit { le16(bytes, at) } else { 0 };
    Ok(Group {
        inode_bitmap: u64::from(le32(bytes, 0x04)) | u64::from(high(0x24)) << 32,
        inode_table: u64::from(le32(bytes, 0x08)) | u64::from(high(0x28)) << 32,
        free_inodes: u32::from(le16(bytes, 0x0E)) | u32::from(high16(0x2E)) << 16,
        flags: le16(bytes, 0x12),
        unused_inodes: u32::from(le16(bytes, 0x1C)) | u32::from(high16(0x32)) << 16,
        inode_bitmap_checksum: u32::from(le16(bytes, 0x1A)) | u32::from(high16(0x3A)) << 16,
    })
}

fn group_checksum_holds(fields: &Superblock, group: u32, bytes: &[u8]) -> bool {
    let stored = le16(bytes, 0x1E);
    let number = group.to_le_bytes();
    if fields.metadata_checksums() {
        let crc = crc32c(fields.checksum_seed, &number);
        let crc = crc32c(crc, &bytes[..0x1E]);
        let crc = crc32c(crc, &[0, 0]);
        let crc = crc32c(crc, &bytes[0x20..]);
        crc as u16 == stored
    } else {
        let crc = crc16(u16::MAX, &fields.uuid);
        let crc = crc16(crc, &number);
        let crc = crc16(crc, &bytes[..0x1E]);
        let crc = if bytes.len() > 0x20 {
            crc16(crc, &bytes[0x20..])
        } else {
            crc
        };
        crc == stored
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InodeFields {
    pub(crate) mode: u16,
    pub(crate) links: u16,
    pub(crate) flags: u32,
    pub(crate) size: u64,
    pub(crate) blocks: u64,
    pub(crate) file_acl: u64,
    pub(crate) generation: u32,
    pub(crate) mtime: u32,
    pub(crate) mtime_extra: Option<u32>,
    pub(crate) extra_end: usize,
}

// SPEC(ext4 Index Nodes): the size's high half counts for files and, with largedir, for
// folders; the extra fields must end inside the inode on a 4-byte boundary.
pub(crate) fn inode_fields(bytes: &[u8], largedir: bool) -> Field<InodeFields> {
    field_check(bytes.len() >= GOOD_OLD_INODE, "the ext inode is too short")?;
    let extra_end = if bytes.len() >= EXTRA_ISIZE_AT + 2 {
        let extra = usize::from(le16(bytes, EXTRA_ISIZE_AT));
        field_check(
            extra % 4 == 0 && GOOD_OLD_INODE + extra <= bytes.len(),
            "the ext inode's extra fields do not fit the inode",
        )?;
        GOOD_OLD_INODE + extra
    } else {
        GOOD_OLD_INODE
    };
    let mode = le16(bytes, 0x00);
    let flags = le32(bytes, 0x20);
    let wide_size = mode & TYPE_BITS == REGULAR || largedir && mode & TYPE_BITS == DIRECTORY;
    let size = u64::from(le32(bytes, 0x04))
        | if wide_size {
            u64::from(le32(bytes, 0x6C)) << 32
        } else {
            0
        };
    let blocks = u64::from(le32(bytes, 0x1C)) | u64::from(le16(bytes, 0x74)) << 32;
    Ok(InodeFields {
        mode,
        links: le16(bytes, 0x1A),
        flags,
        size,
        blocks,
        file_acl: u64::from(le32(bytes, 0x68)) | u64::from(le16(bytes, 0x76)) << 32,
        generation: le32(bytes, 0x64),
        mtime: le32(bytes, 0x10),
        mtime_extra: (extra_end >= MTIME_EXTRA_END).then(|| le32(bytes, 0x88)),
        extra_end,
    })
}

fn inode_seed(fields: &Superblock, number: u32, generation: u32) -> u32 {
    crc32c(
        crc32c(fields.checksum_seed, &number.to_le_bytes()),
        &generation.to_le_bytes(),
    )
}

fn inode_checksum_holds(
    fields: &Superblock,
    number: u32,
    bytes: &[u8],
    parsed: &InodeFields,
) -> bool {
    let seed = inode_seed(fields, number, parsed.generation);
    let crc = crc32c(seed, &bytes[..0x7C]);
    let crc = crc32c(crc, &[0, 0]);
    let wide = parsed.extra_end >= CHECKSUM_HI_END;
    if bytes.len() < EXTRA_ISIZE_AT + 2 {
        let crc = crc32c(crc, &bytes[0x7E..]);
        return crc as u16 == le16(bytes, 0x7C);
    }
    let crc = crc32c(crc, &bytes[0x7E..0x82]);
    let crc = if wide {
        crc32c(crc32c(crc, &[0, 0]), &bytes[CHECKSUM_HI_END..])
    } else {
        crc32c(crc, &bytes[0x82..])
    };
    if wide {
        crc == u32::from(le16(bytes, 0x82)) << 16 | u32::from(le16(bytes, 0x7C))
    } else {
        crc as u16 == le16(bytes, 0x7C)
    }
}

// EXTERNAL(7-Zip 26.05 `7zz x` on ext): signed seconds plus the extra field's epoch bits;
// nanoseconds of a second or more read as 0, and a time of zero is not written.
pub(crate) fn ext_time(seconds: u32, extra: Option<u32>) -> Option<SystemTime> {
    let extra = extra.unwrap_or(0);
    let seconds = i64::from(seconds as i32) + (i64::from(extra & 3) << 32);
    let nanoseconds = Some(extra >> 2)
        .filter(|&nanos| nanos < 1_000_000_000)
        .unwrap_or(0);
    if seconds == 0 && nanoseconds == 0 {
        return None;
    }
    let whole = if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    };
    whole?.checked_add(Duration::from_nanos(u64::from(nanoseconds)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExtentHeader {
    pub(crate) entries: u16,
    pub(crate) max: u16,
    pub(crate) depth: u16,
}

// SPEC(ext4 Extent Tree): a node holds its header and at most as many 12-byte entries
// as fit; a block-sized node keeps four bytes after them for its checksum.
pub(crate) fn extent_header(node: &[u8], in_inode: bool) -> Field<ExtentHeader> {
    field_check(
        node.len() >= EXTENT_HEADER,
        "the ext extent node is too short",
    )?;
    field_check(
        le16(node, 0) == EXTENT_MAGIC,
        "the ext extent node has no signature",
    )?;
    let entries = le16(node, 2);
    let max = le16(node, 4);
    let depth = le16(node, 6);
    let room = if in_inode {
        node.len() - EXTENT_HEADER
    } else {
        node.len().saturating_sub(EXTENT_HEADER + EXTENT_TAIL)
    } / EXTENT_ENTRY;
    field_check(
        max >= 1 && usize::from(max) <= room && entries <= max,
        "the ext extent node holds more entries than fit",
    )?;
    field_check(
        depth <= MAX_EXTENT_DEPTH,
        "the ext extent tree is deeper than 5 levels",
    )?;
    Ok(ExtentHeader {
        entries,
        max,
        depth,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    pub(crate) logical: u32,
    pub(crate) length: u32,
    pub(crate) physical: u64,
    pub(crate) initialized: bool,
}

pub(crate) fn extent_entry(node: &[u8], index: usize, leaf: bool) -> Field<Extent> {
    let at = EXTENT_HEADER + index * EXTENT_ENTRY;
    field_check(
        at + EXTENT_ENTRY <= node.len(),
        "the ext extent entry lies past its node",
    )?;
    let entry = &node[at..at + EXTENT_ENTRY];
    let logical = le32(entry, 0);
    if !leaf {
        return Ok(Extent {
            logical,
            length: 0,
            physical: u64::from(le32(entry, 4)) | u64::from(le16(entry, 8)) << 32,
            initialized: true,
        });
    }
    let raw = le16(entry, 4);
    field_check(raw != 0, "the ext extent is empty")?;
    let initialized = raw <= LARGEST_INITIALIZED_EXTENT;
    let length = u32::from(if initialized {
        raw
    } else {
        raw - LARGEST_INITIALIZED_EXTENT
    });
    field_check(
        u64::from(logical) + u64::from(length) <= 1 << 32,
        "the ext extent ends past the last logical block",
    )?;
    Ok(Extent {
        logical,
        length,
        physical: u64::from(le32(entry, 8)) | u64::from(le16(entry, 6)) << 32,
        initialized,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirEntry {
    pub(crate) inode: u32,
    pub(crate) length: usize,
    pub(crate) name: Range<usize>,
    pub(crate) kind: u8,
}

// SPEC(ext4 Directory Entries): a record of at least 12 bytes on a 4-byte boundary that
// holds its name and ends inside its block; 0 or 65,535 means 64 KiB in a 64 KiB block.
pub(crate) fn dir_entry(block: &[u8], at: usize, file_types: bool) -> Field<DirEntry> {
    field_check(
        at.is_multiple_of(4)
            && block
                .len()
                .checked_sub(DIR_ENTRY_HEADER)
                .is_some_and(|last| at <= last),
        "an ext folder record lies past its block",
    )?;
    let raw = le16(block, at + 4);
    let length = if block.len() >= 1 << 16 && (raw == 0 || raw == u16::MAX) {
        1 << 16
    } else {
        usize::from(raw)
    };
    let name_length = if file_types {
        usize::from(block[at + 6])
    } else {
        usize::from(le16(block, at + 6))
    };
    field_check(
        length >= MIN_DIR_RECORD && length % 4 == 0 && length <= block.len() - at,
        "an ext folder record has a length that leaves its block",
    )?;
    field_check(
        DIR_ENTRY_HEADER + name_length <= length,
        "an ext folder record's name is longer than its record",
    )?;
    Ok(DirEntry {
        inode: le32(block, at),
        length,
        name: at + DIR_ENTRY_HEADER..at + DIR_ENTRY_HEADER + name_length,
        kind: if file_types { block[at + 7] } else { 0 },
    })
}

fn dir_tail(block: &[u8]) -> Option<u32> {
    let at = block.len().checked_sub(DIR_TAIL)?;
    let tail = &block[at..];
    (le32(tail, 0) == 0
        && usize::from(le16(tail, 4)) == DIR_TAIL
        && tail[6] == 0
        && tail[7] == DIR_TAIL_TYPE)
        .then(|| le32(tail, 8))
}

// SPEC(ext4 Extended Attributes): the attributes inside an inode follow its extra fields,
// with values placed from the first entry; inline data is the value of `system.data`.
pub(crate) fn inline_value(inode: &[u8], extra_end: usize) -> Field<Range<usize>> {
    if extra_end + XATTR_HEADER > inode.len() || le32(inode, extra_end) != XATTR_MAGIC {
        return Ok(0..0);
    }
    let first = extra_end + XATTR_HEADER;
    let mut at = first;
    while at + 4 <= inode.len() && le32(inode, at) != 0 {
        field_check(
            at + XATTR_ENTRY <= inode.len(),
            "an ext inode attribute lies past the inode",
        )?;
        let name_length = usize::from(inode[at]);
        let index = inode[at + 1];
        let value_at = usize::from(le16(inode, at + 2));
        let value_inode = le32(inode, at + 4);
        let value_size = le32(inode, at + 8) as usize;
        let name_end = at + XATTR_ENTRY + name_length;
        field_check(
            name_end <= inode.len(),
            "an ext inode attribute's name lies past the inode",
        )?;
        if index == XATTR_SYSTEM && &inode[at + XATTR_ENTRY..name_end] == INLINE_DATA_NAME {
            field_check(
                value_inode == 0,
                "the ext inline data lies in another inode",
            )?;
            let start = first + value_at;
            field_check(
                start <= inode.len() && value_size <= inode.len() - start,
                "the ext inline data lies past the inode",
            )?;
            return Ok(start..start + value_size);
        }
        at = (name_end + 3) & !3;
    }
    Ok(0..0)
}

#[derive(Clone, Copy)]
struct Run {
    logical: u32,
    length: u32,
    physical: u64,
}

impl Run {
    fn initialized(&self) -> bool {
        self.physical & UNINITIALIZED == 0
    }

    fn start(&self) -> u64 {
        self.physical & !UNINITIALIZED
    }

    fn logical_end(&self) -> u64 {
        u64::from(self.logical) + u64::from(self.length)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Runs,
    Inline,
    FastLink,
    Empty,
}

struct Inode {
    number: u32,
    mode: u16,
    links: u16,
    flags: u32,
    size: u64,
    generation: u32,
    modified: Option<SystemTime>,
    content: Content,
    runs: Range<u32>,
    names: u32,
    subfolders: u32,
    children: Range<u32>,
}

struct Record {
    child: u32,
    name: Range<u32>,
}

struct Catalog {
    inodes: Vec<Inode>,
    runs: Vec<Run>,
    records: Vec<Record>,
    names: Vec<u8>,
    system: Vec<(u32, &'static str)>,
    unknown: Vec<u32>,
}

impl Catalog {
    fn index(&self, number: u32) -> Option<usize> {
        self.inodes
            .binary_search_by_key(&number, |inode| inode.number)
            .ok()
    }

    fn runs_of(&self, inode: &Inode) -> &[Run] {
        &self.runs[inode.runs.start as usize..inode.runs.end as usize]
    }

    fn name(&self, record: &Record) -> &[u8] {
        &self.names[record.name.start as usize..record.name.end as usize]
    }
}

struct FileSystem<'v> {
    volume: &'v mut dyn Volume,
    fields: Superblock,
    cached: Option<(u64, Vec<u8>)>,
}

fn kind_name(mode: u16) -> &'static str {
    match mode & TYPE_BITS {
        REGULAR => "file",
        DIRECTORY => "folder",
        SYMLINK => "link",
        FIFO => "pipe",
        SOCKET => "socket",
        CHARACTER => "character device",
        BLOCK_DEVICE => "block device",
        _ => "inode of unknown type",
    }
}

fn kind_code(mode: u16) -> u8 {
    match mode & TYPE_BITS {
        REGULAR => 1,
        DIRECTORY => 2,
        CHARACTER => 3,
        BLOCK_DEVICE => 4,
        FIFO => 5,
        SOCKET => 6,
        SYMLINK => 7,
        _ => 0,
    }
}

struct Claims {
    spans: Vec<(u64, u64)>,
}

impl Claims {
    fn claim(&mut self, start: u64, length: u64) -> Result<()> {
        if let Some(last) = self.spans.last_mut()
            && last.1 == start
        {
            last.1 += length;
            return Ok(());
        }
        ensure!(
            self.spans.len() < MAX_RUNS,
            "the ext volume's files use more than {MAX_RUNS} runs of blocks"
        );
        self.spans.push((start, start + length));
        Ok(())
    }

    fn check(mut self) -> Result<()> {
        self.spans.sort_unstable();
        for pair in self.spans.windows(2) {
            ensure!(
                pair[1].0 >= pair[0].1,
                "ext block {} is used twice",
                pair[1].0
            );
        }
        Ok(())
    }
}

struct Loader<'c> {
    runs: &'c mut Vec<Run>,
    claims: &'c mut Claims,
}

impl Loader<'_> {
    fn push(&mut self, logical: u64, physical: u64, length: u64, initialized: bool) -> Result<()> {
        let flag = if initialized { 0 } else { UNINITIALIZED };
        if let Some(last) = self.runs.last_mut()
            && last.logical_end() == logical
            && last.start() + u64::from(last.length) == physical
            && last.initialized() == initialized
            && u64::from(last.length) + length <= u64::from(u32::MAX)
        {
            last.length += length as u32;
        } else {
            ensure!(
                self.runs.len() < MAX_RUNS,
                "the ext volume's files use more than {MAX_RUNS} runs of blocks"
            );
            self.runs.push(Run {
                logical: logical as u32,
                length: length as u32,
                physical: physical | flag,
            });
        }
        self.claims.claim(physical, length)
    }
}

impl<'v> FileSystem<'v> {
    fn open(volume: &'v mut dyn Volume) -> Result<Self> {
        let mut bytes = [0u8; SUPERBLOCK];
        ensure!(
            volume.size() >= (SUPERBLOCK_AT + SUPERBLOCK) as u64,
            "the ext volume is shorter than its superblock"
        );
        volume
            .read_exact_at(SUPERBLOCK_AT as u64, &mut bytes)
            .context("cannot read the ext superblock")?;
        let mut fields = superblock_layout(bytes[..SUPERBLOCK_FIELDS].try_into().unwrap())?;
        if fields.incompat & INCOMPAT_CSUM_SEED == 0 {
            fields.checksum_seed = crc32c(u32::MAX, &fields.uuid);
        }
        if let Some(feature) = refused_feature(&fields) {
            bail!("the ext volume uses {feature}, which Zipper does not read; {BACKEND_HINT}");
        }
        if fields.metadata_checksums() {
            ensure!(
                crc32c(u32::MAX, &bytes[..CHECKSUM_AT]) == le32(&bytes, CHECKSUM_AT),
                "the ext superblock checksum does not match"
            );
        }
        let length = fields.blocks << fields.block_log;
        ensure!(
            length <= volume.size(),
            "the ext volume of {length} bytes lies past the end of its image of {} bytes",
            volume.size()
        );
        Ok(FileSystem {
            volume,
            fields,
            cached: None,
        })
    }

    fn block_size(&self) -> usize {
        self.fields.block_size()
    }

    fn read_block(&mut self, block: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            block < self.fields.blocks,
            "ext block {block} lies past the end of the volume"
        );
        self.volume
            .read_exact_at(block << self.fields.block_log, buffer)
            .with_context(|| format!("cannot read ext block {block}"))
    }

    fn cached_block(&mut self, block: u64) -> Result<&[u8]> {
        if self.cached.as_ref().is_none_or(|(at, _)| *at != block) {
            let mut bytes = self
                .cached
                .take()
                .map_or_else(|| vec![0u8; self.block_size()], |(_, bytes)| bytes);
            if let Err(error) = self.read_block(block, &mut bytes) {
                self.cached = None;
                return Err(error);
            }
            self.cached = Some((block, bytes));
        }
        Ok(&self.cached.as_ref().unwrap().1)
    }

    fn group(&mut self, group: u32) -> Result<Group> {
        let fields = self.fields;
        let (block, offset) = fields.descriptor_place(group);
        let size = fields.descriptor_size as usize;
        let bytes = self.cached_block(block)?[offset..offset + size].to_vec();
        if fields.descriptor_checksums() {
            ensure!(
                group_checksum_holds(&fields, group, &bytes),
                "the ext group {group} descriptor checksum does not match"
            );
        }
        let parsed = group_fields(&bytes, fields.incompat & INCOMPAT_64BIT != 0)?;
        let table_blocks = (u64::from(fields.inodes_per_group) * u64::from(fields.inode_size))
            .div_ceil(fields.block_size() as u64);
        ensure!(
            parsed.inode_bitmap < fields.blocks
                && parsed
                    .inode_table
                    .checked_add(table_blocks)
                    .is_some_and(|end| end <= fields.blocks),
            "the ext group {group} places its inodes past the end of the volume"
        );
        Ok(parsed)
    }

    fn inode_bytes(&mut self, number: u32, group: &Group) -> Result<Vec<u8>> {
        let fields = self.fields;
        let index = u64::from((number - 1) % fields.inodes_per_group);
        let at = index * u64::from(fields.inode_size);
        let block = group.inode_table + (at >> fields.block_log);
        let offset = (at & (fields.block_size() as u64 - 1)) as usize;
        let size = fields.inode_size as usize;
        Ok(self.cached_block(block)?[offset..offset + size].to_vec())
    }

    fn inode(&mut self, number: u32) -> Result<(Vec<u8>, InodeFields)> {
        let group = self.group((number - 1) / self.fields.inodes_per_group)?;
        let bytes = self.inode_bytes(number, &group)?;
        let parsed = self.checked_inode(number, &bytes)?;
        Ok((bytes, parsed))
    }

    fn checked_inode(&self, number: u32, bytes: &[u8]) -> Result<InodeFields> {
        let parsed = inode_fields(bytes, self.fields.incompat & INCOMPAT_LARGEDIR != 0)
            .with_context(|| format!("ext inode {number}"))?;
        if self.fields.metadata_checksums() {
            ensure!(
                inode_checksum_holds(&self.fields, number, bytes, &parsed),
                "the ext inode {number} checksum does not match"
            );
        }
        Ok(parsed)
    }

    fn check_extent_span(&self, number: u32, physical: u64, length: u64) -> Result<()> {
        ensure!(
            physical >= u64::from(self.fields.first_data_block)
                && physical
                    .checked_add(length)
                    .is_some_and(|end| end <= self.fields.blocks),
            "ext inode {number} names blocks past the end of the volume"
        );
        Ok(())
    }

    fn extents(
        &mut self,
        number: u32,
        generation: u32,
        node: &[u8],
        depth: Option<u16>,
        range: (u64, u64),
        loader: &mut Loader,
    ) -> Result<()> {
        let in_inode = depth.is_none();
        let header =
            extent_header(node, in_inode).with_context(|| format!("ext inode {number}"))?;
        if let Some(expected) = depth {
            ensure!(
                header.depth == expected,
                "ext inode {number} has an extent node at the wrong depth"
            );
        }
        if !in_inode && self.fields.metadata_checksums() {
            let tail = EXTENT_HEADER + usize::from(header.max) * EXTENT_ENTRY;
            let seed = inode_seed(&self.fields, number, generation);
            ensure!(
                crc32c(seed, &node[..tail]) == le32(node, tail),
                "the ext inode {number} extent block checksum does not match"
            );
        }
        let leaf = header.depth == 0;
        let mut next = range.0;
        let entries: Vec<Extent> = (0..usize::from(header.entries))
            .map(|index| extent_entry(node, index, leaf))
            .collect::<Field<_>>()
            .with_context(|| format!("ext inode {number}"))?;
        for (index, entry) in entries.iter().enumerate() {
            let logical = u64::from(entry.logical);
            ensure!(
                logical >= next && logical < range.1,
                "ext inode {number} has extents out of order"
            );
            if leaf {
                let end = logical + u64::from(entry.length);
                ensure!(
                    end <= range.1,
                    "ext inode {number} has extents out of order"
                );
                self.check_extent_span(number, entry.physical, u64::from(entry.length))?;
                loader.push(
                    logical,
                    entry.physical,
                    u64::from(entry.length),
                    entry.initialized,
                )?;
                next = end;
            } else {
                let end = entries
                    .get(index + 1)
                    .map_or(range.1, |after| u64::from(after.logical));
                ensure!(end > logical, "ext inode {number} has extents out of order");
                self.check_extent_span(number, entry.physical, 1)?;
                loader.claims.claim(entry.physical, 1)?;
                let mut child = vec![0u8; self.block_size()];
                self.read_block(entry.physical, &mut child)?;
                self.extents(
                    number,
                    generation,
                    &child,
                    Some(header.depth - 1),
                    (logical, end),
                    loader,
                )?;
                next = end;
            }
        }
        Ok(())
    }

    fn block_map(
        &mut self,
        number: u32,
        pointer: u32,
        level: u32,
        logical: u64,
        needed: u64,
        loader: &mut Loader,
    ) -> Result<()> {
        if pointer == 0 || logical >= needed {
            return Ok(());
        }
        self.check_extent_span(number, u64::from(pointer), 1)?;
        if level == 0 {
            return loader.push(logical, u64::from(pointer), 1, true);
        }
        loader.claims.claim(u64::from(pointer), 1)?;
        let mut block = vec![0u8; self.block_size()];
        self.read_block(u64::from(pointer), &mut block)?;
        let per_block = (self.block_size() / 4) as u64;
        let span = per_block.pow(level - 1);
        for (index, entry) in block.chunks_exact(4).enumerate() {
            let start = logical + index as u64 * span;
            if start >= needed {
                break;
            }
            let child = u32::from_le_bytes(entry.try_into().unwrap());
            self.block_map(number, child, level - 1, start, needed, loader)?;
        }
        Ok(())
    }

    fn map(
        &mut self,
        number: u32,
        bytes: &[u8],
        parsed: &InodeFields,
        loader: &mut Loader,
    ) -> Result<()> {
        let block_size = self.block_size() as u64;
        let needed = parsed.size.div_ceil(block_size);
        let pointers = &bytes[I_BLOCK..I_BLOCK + I_BLOCK_BYTES];
        if parsed.flags & INODE_FLAG_EXTENTS != 0 {
            ensure!(
                parsed.size <= (1 << 32) * block_size,
                "ext inode {number} is larger than its extents can map"
            );
            return self.extents(
                number,
                parsed.generation,
                pointers,
                None,
                (0, 1 << 32),
                loader,
            );
        }
        ensure!(
            parsed.size <= self.fields.largest_block_map_file(),
            "ext inode {number} is larger than its block map can map"
        );
        let per_block = block_size / 4;
        let mut logical = 0u64;
        for (index, entry) in pointers.chunks_exact(4).enumerate() {
            let pointer = u32::from_le_bytes(entry.try_into().unwrap());
            let level = index.saturating_sub(DIRECT_POINTERS - 1) as u32;
            self.block_map(number, pointer, level, logical, needed, loader)?;
            logical += per_block.pow(level);
        }
        Ok(())
    }

    fn content(&self, number: u32, parsed: &InodeFields) -> Result<Content> {
        let kind = parsed.mode & TYPE_BITS;
        ensure!(
            parsed.flags & INODE_FLAG_ENCRYPT == 0,
            "ext inode {number} is encrypted on a volume without encryption"
        );
        ensure!(
            parsed.flags & INODE_FLAG_EA_INODE == 0,
            "ext inode {number} holds an attribute value on a volume without attribute inodes"
        );
        if parsed.flags & INODE_FLAG_INLINE_DATA != 0 {
            ensure!(
                self.fields.incompat & INCOMPAT_INLINE_DATA != 0
                    && parsed.flags & INODE_FLAG_EXTENTS == 0,
                "ext inode {number} has inline data the volume does not allow"
            );
            ensure!(
                matches!(kind, REGULAR | DIRECTORY | SYMLINK),
                "ext inode {number} is a {} with inline data",
                kind_name(parsed.mode)
            );
            return Ok(Content::Inline);
        }
        match kind {
            FIFO | SOCKET | CHARACTER | BLOCK_DEVICE => {
                ensure!(
                    parsed.size == 0,
                    "ext inode {number} is a {} of {} bytes",
                    kind_name(parsed.mode),
                    parsed.size
                );
                return Ok(Content::Empty);
            }
            SYMLINK if parsed.flags & INODE_FLAG_EXTENTS == 0 => {
                let unit = if parsed.flags & INODE_FLAG_HUGE_FILE != 0 {
                    self.block_size() as u64
                } else {
                    512
                };
                let attribute_blocks = if parsed.file_acl != 0 {
                    self.block_size() as u64 / unit
                } else {
                    0
                };
                if parsed.blocks == attribute_blocks {
                    ensure!(
                        parsed.size < I_BLOCK_BYTES as u64,
                        "ext link inode {number} is longer than a link held in its inode"
                    );
                    return Ok(Content::FastLink);
                }
            }
            _ => {}
        }
        if parsed.flags & INODE_FLAG_EXTENTS != 0 {
            ensure!(
                self.fields.incompat & INCOMPAT_EXTENTS != 0,
                "ext inode {number} has extents the volume does not allow"
            );
        }
        Ok(Content::Runs)
    }

    fn inline_data(&mut self, number: u32) -> Result<Vec<u8>> {
        let (bytes, parsed) = self.inode(number)?;
        let value = inline_value(&bytes, parsed.extra_end)
            .with_context(|| format!("ext inode {number}"))?;
        let size = parsed.size as usize;
        ensure!(
            parsed.size <= (I_BLOCK_BYTES + value.len()) as u64,
            "ext inode {number} holds less inline data than its size"
        );
        let mut data = bytes[I_BLOCK..I_BLOCK + I_BLOCK_BYTES].to_vec();
        data.extend_from_slice(&bytes[value]);
        data.truncate(size);
        Ok(data)
    }
}

fn kept_inode(
    fs: &mut FileSystem,
    number: u32,
    bytes: &[u8],
    parsed: &InodeFields,
    loader: &mut Loader,
) -> Result<Inode> {
    let content = fs.content(number, parsed)?;
    let start = loader.runs.len() as u32;
    if content == Content::Runs {
        fs.map(number, bytes, parsed, loader)?;
    }
    Ok(Inode {
        number,
        mode: parsed.mode,
        links: parsed.links,
        flags: parsed.flags,
        size: parsed.size,
        generation: parsed.generation,
        modified: ext_time(parsed.mtime, parsed.mtime_extra),
        content,
        runs: start..loader.runs.len() as u32,
        names: 0,
        subfolders: 0,
        children: 0..0,
    })
}

fn load_inodes(fs: &mut FileSystem, catalog: &mut Catalog, claims: &mut Claims) -> Result<()> {
    let fields = fs.fields;
    let mut loader = Loader {
        runs: &mut catalog.runs,
        claims,
    };
    let reserved_end = (fields.first_data_block + 1 + fields.reserved_descriptor_blocks) as u64
        + if fields.incompat & INCOMPAT_META_BG != 0 {
            u64::from(fields.first_meta_group)
        } else {
            u64::from(fields.descriptor_blocks())
        };
    loader.claims.claim(0, reserved_end.min(fields.blocks))?;
    for number in 1..fields.first_inode {
        if number == RESIZE {
            continue;
        }
        let group = fs.group(0)?;
        let bytes = fs.inode_bytes(number, &group)?;
        let raw = inode_fields(&bytes, fields.incompat & INCOMPAT_LARGEDIR != 0)
            .with_context(|| format!("ext inode {number}"))?;
        if number != ROOT && raw.size == 0 {
            continue;
        }
        let parsed = fs.checked_inode(number, &bytes)?;
        let inode = kept_inode(fs, number, &bytes, &parsed, &mut loader)?;
        if number == ROOT {
            ensure!(
                inode.mode & TYPE_BITS == DIRECTORY,
                "the ext root inode is not a folder"
            );
        } else {
            let name = SYSTEM_NAMES
                .iter()
                .find(|(reserved, _)| *reserved == number)
                .unwrap()
                .1;
            ensure!(
                matches!(inode.mode & TYPE_BITS, 0 | REGULAR) && inode.content == Content::Runs,
                "the ext reserved inode {number} is a {}",
                kind_name(inode.mode)
            );
            catalog.system.push((number, name));
        }
        catalog.inodes.push(inode);
    }
    let per_group = fields.inodes_per_group;
    let bitmap_bytes = per_group.div_ceil(8) as usize;
    let mut bitmap = vec![0u8; fs.block_size()];
    for group_number in 0..fields.groups {
        let group = fs.group(group_number)?;
        let checksums = fields.descriptor_checksums();
        if checksums && group.flags & GROUP_INODE_UNINIT != 0 {
            ensure!(
                group.free_inodes == per_group,
                "the ext group {group_number} has inodes in use but marks its inodes unused"
            );
            continue;
        }
        fs.read_block(group.inode_bitmap, &mut bitmap)?;
        if fields.metadata_checksums() {
            let crc = crc32c(fields.checksum_seed, &bitmap[..bitmap_bytes]);
            let crc = if fields.descriptor_size >= LARGE_DESCRIPTOR {
                crc
            } else {
                crc & 0xFFFF
            };
            ensure!(
                crc == group.inode_bitmap_checksum,
                "the ext group {group_number} inode bitmap checksum does not match"
            );
        }
        let used = if checksums {
            ensure!(
                group.unused_inodes <= per_group,
                "the ext group {group_number} has more unused inodes than inodes"
            );
            per_group - group.unused_inodes
        } else {
            per_group
        };
        let mut free = 0u32;
        for index in 0..per_group {
            let number = group_number * per_group + index + 1;
            if bitmap[(index / 8) as usize] & (1 << (index % 8)) == 0 {
                free += 1;
                continue;
            }
            if number < fields.first_inode {
                continue;
            }
            ensure!(
                index < used,
                "the ext group {group_number} marks inode {number} in use past its used inodes"
            );
            ensure!(
                catalog.inodes.len() < MAX_INODES,
                "the ext volume has more than {MAX_INODES} inodes in use"
            );
            let bytes = fs.inode_bytes(number, &group)?;
            let parsed = fs.checked_inode(number, &bytes)?;
            ensure!(
                parsed.links != 0,
                "ext inode {number} is in use with no links"
            );
            ensure!(
                kind_code(parsed.mode) != 0,
                "ext inode {number} is in use and has no type"
            );
            let inode = kept_inode(fs, number, &bytes, &parsed, &mut loader)?;
            catalog.inodes.push(inode);
        }
        ensure!(
            free == group.free_inodes,
            "the ext group {group_number} counts {} free inodes and its bitmap {free}",
            group.free_inodes
        );
    }
    Ok(())
}

struct Folders<'c> {
    catalog: &'c mut Catalog,
    pending: Vec<(u32, u32)>,
    seen_names: HashSet<u64>,
    keys: std::hash::RandomState,
}

impl Folders<'_> {
    fn add(
        &mut self,
        parent: usize,
        child_number: u32,
        name: &[u8],
        kind: u8,
        file_types: bool,
    ) -> Result<()> {
        let parent_number = self.catalog.inodes[parent].number;
        ensure!(
            name != b"." && name != b"..",
            "ext folder {parent_number} names {:?} past its first records",
            String::from_utf8_lossy(name)
        );
        ensure!(
            !name.is_empty() && !name.contains(&b'/') && !name.contains(&0),
            "ext folder {parent_number} holds a name with a slash or NUL"
        );
        let child = self
            .catalog
            .index(child_number)
            .filter(|_| child_number >= GOOD_OLD_FIRST_INODE)
            .with_context(|| {
                format!(
                    "ext folder {parent_number} names inode {child_number}, which is not in use"
                )
            })?;
        let mode = self.catalog.inodes[child].mode;
        ensure!(
            !file_types || kind == kind_code(mode),
            "ext folder {parent_number} names inode {child_number} as type {kind}, but it is a {}",
            kind_name(mode)
        );
        ensure!(
            self.seen_names.insert(std::hash::BuildHasher::hash_one(
                &self.keys,
                (parent_number, name)
            )),
            "ext folder {parent_number} holds two entries named {:?}",
            String::from_utf8_lossy(name)
        );
        let start = self.catalog.names.len();
        ensure!(start + name.len() <= MAX_NAMES, "ext names exceed 64 MiB");
        ensure!(
            self.catalog.records.len() < MAX_INODES,
            "the ext volume has more than {MAX_INODES} names"
        );
        self.catalog.names.extend_from_slice(name);
        self.catalog.records.push(Record {
            child: child as u32,
            name: start as u32..self.catalog.names.len() as u32,
        });
        let inode = &mut self.catalog.inodes[child];
        inode.names += 1;
        if mode & TYPE_BITS == DIRECTORY {
            ensure!(
                inode.names == 1,
                "ext folder {child_number} has more than one name"
            );
            self.catalog.inodes[parent].subfolders += 1;
            self.pending.push((child as u32, parent_number));
        }
        Ok(())
    }
}

fn check_dot(entry: &DirEntry, block: &[u8], name: &[u8], inode: u32, folder: u32) -> Result<()> {
    ensure!(
        &block[entry.name.clone()] == name && entry.inode == inode,
        "ext folder {folder} does not start with its {:?} record",
        String::from_utf8_lossy(name)
    );
    Ok(())
}

fn parse_records(
    folders: &mut Folders,
    folder: usize,
    region: &[u8],
    skip: usize,
    parent: u32,
    file_types: bool,
) -> Result<()> {
    let number = folders.catalog.inodes[folder].number;
    let mut at = 0;
    let mut index = 0usize;
    while at < region.len() {
        let entry =
            dir_entry(region, at, file_types).with_context(|| format!("ext folder {number}"))?;
        if index < skip {
            let (name, inode): (&[u8], u32) = if index == 0 {
                (b".", number)
            } else {
                (b"..", parent)
            };
            check_dot(&entry, region, name, inode, number)?;
        } else if entry.inode != 0 {
            let name = region[entry.name.clone()].to_vec();
            folders.add(folder, entry.inode, &name, entry.kind, file_types)?;
        } else {
            ensure!(
                !region[entry.name.clone()].contains(&0),
                "ext folder {number} holds an empty record whose name has a NUL"
            );
        }
        index += 1;
        at += entry.length;
    }
    ensure!(
        index >= skip,
        "ext folder {number} does not start with its \".\" and \"..\" records"
    );
    Ok(())
}

fn read_folder(
    fs: &mut FileSystem,
    folders: &mut Folders,
    folder: usize,
    parent: u32,
) -> Result<()> {
    let inode = &folders.catalog.inodes[folder];
    let (number, size, flags, generation) =
        (inode.number, inode.size, inode.flags, inode.generation);
    let file_types = fs.fields.file_types();
    let start = folders.catalog.records.len() as u32;
    if inode.content == Content::Inline {
        let data = fs.inline_data(number)?;
        ensure!(
            data.len() >= I_BLOCK_BYTES,
            "ext inline folder {number} is shorter than its inode's block area"
        );
        ensure!(
            le32(&data, 0) == parent,
            "ext inline folder {number} names the wrong parent"
        );
        parse_records(
            folders,
            folder,
            &data[INLINE_PARENT..I_BLOCK_BYTES],
            0,
            parent,
            file_types,
        )?;
        parse_records(
            folders,
            folder,
            &data[I_BLOCK_BYTES..],
            0,
            parent,
            file_types,
        )?;
    } else {
        let block_size = fs.block_size();
        ensure!(
            size % block_size as u64 == 0 && size != 0,
            "ext folder {number} is not a whole number of blocks"
        );
        let runs: Vec<Run> = folders.catalog.runs_of(inode).to_vec();
        let indexed = flags & INODE_FLAG_INDEX != 0;
        let blocks = size / block_size as u64;
        let mut covered = 0u64;
        let mut block = vec![0u8; block_size];
        for run in &runs {
            ensure!(
                run.initialized() && u64::from(run.logical) == covered,
                "ext folder {number} has a hole"
            );
            for step in 0..u64::from(run.length) {
                if covered >= blocks {
                    break;
                }
                fs.read_block(run.start() + step, &mut block)?;
                let records = if fs.fields.metadata_checksums() {
                    match dir_tail(&block) {
                        Some(stored) => {
                            let seed = inode_seed(&fs.fields, number, generation);
                            ensure!(
                                crc32c(seed, &block[..block_size - DIR_TAIL]) == stored,
                                "the ext folder {number} block checksum does not match"
                            );
                            block_size - DIR_TAIL
                        }
                        None => {
                            let index_node = covered == 0
                                || le32(&block, 0) == 0
                                    && dir_entry(&block, 0, file_types)
                                        .is_ok_and(|entry| entry.length == block_size);
                            ensure!(
                                indexed && index_node,
                                "the ext folder {number} block has no checksum"
                            );
                            block_size
                        }
                    }
                } else {
                    block_size
                };
                let skip = if covered == 0 { 2 } else { 0 };
                parse_records(folders, folder, &block[..records], skip, parent, file_types)?;
                covered += 1;
            }
        }
        ensure!(covered == blocks, "ext folder {number} has a hole");
    }
    folders.catalog.inodes[folder].children = start..folders.catalog.records.len() as u32;
    Ok(())
}

fn load_folders(fs: &mut FileSystem, catalog: &mut Catalog) -> Result<()> {
    let root = catalog
        .index(ROOT)
        .context("the ext root folder is missing")?;
    let mut folders = Folders {
        catalog,
        pending: vec![(root as u32, ROOT)],
        seen_names: HashSet::new(),
        keys: std::hash::RandomState::new(),
    };
    while let Some((folder, parent)) = folders.pending.pop() {
        read_folder(fs, &mut folders, folder as usize, parent)?;
    }
    let dir_nlink = fs.fields.ro_compat & RO_COMPAT_DIR_NLINK != 0;
    for inode in &catalog.inodes {
        let number = inode.number;
        if number < GOOD_OLD_FIRST_INODE && number != ROOT {
            continue;
        }
        if inode.mode & TYPE_BITS == DIRECTORY {
            ensure!(
                number == ROOT || inode.names == 1,
                "ext folder {number} is in use but lies in no folder"
            );
            let expected = 2 + u64::from(inode.subfolders);
            ensure!(
                u64::from(inode.links) == expected || dir_nlink && inode.links == 1,
                "ext folder {number} has a link count of {} and {} subfolders",
                inode.links,
                inode.subfolders
            );
        } else if inode.names == 0 {
            ensure!(
                inode.mode & TYPE_BITS == REGULAR && inode.links == 1,
                "ext inode {number} is a {} in no folder with a link count of {}",
                kind_name(inode.mode),
                inode.links
            );
            catalog.unknown.push(number);
        } else {
            ensure!(
                u32::from(inode.links) == inode.names,
                "ext inode {number} has a link count of {} and {} names",
                inode.links,
                inode.names
            );
        }
    }
    Ok(())
}

fn load(fs: &mut FileSystem) -> Result<Catalog> {
    let mut catalog = Catalog {
        inodes: Vec::new(),
        runs: Vec::new(),
        records: Vec::new(),
        names: Vec::new(),
        system: Vec::new(),
        unknown: Vec::new(),
    };
    let mut claims = Claims { spans: Vec::new() };
    load_inodes(fs, &mut catalog, &mut claims)?;
    claims.check()?;
    load_folders(fs, &mut catalog)?;
    Ok(catalog)
}

struct FileData<'f, 'v> {
    fs: &'f mut FileSystem<'v>,
    runs: &'f [Run],
    size: u64,
    position: u64,
    holes: Holes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Holes {
    ReadAsZeros,
    LeftUnread,
}

impl Read for FileData<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.size || buffer.is_empty() {
            return Ok(0);
        }
        let log = self.fs.fields.block_log;
        let block = self.position >> log;
        while let Some(run) = self.runs.first()
            && run.logical_end() <= block
        {
            self.runs = &self.runs[1..];
        }
        let left = (self.size - self.position).min(buffer.len().min(READ_PIECE) as u64);
        let (want, physical) = match self.runs.first() {
            Some(run) if u64::from(run.logical) <= block => {
                let end = run.logical_end() << log;
                let want = left.min(end - self.position) as usize;
                let offset = self.position - (u64::from(run.logical) << log);
                (
                    want,
                    run.initialized().then(|| (run.start() << log) + offset),
                )
            }
            Some(run) => (
                left.min((u64::from(run.logical) << log) - self.position) as usize,
                None,
            ),
            None => (left as usize, None),
        };
        match physical {
            Some(at) => self
                .fs
                .volume
                .read_exact_at(at, &mut buffer[..want])
                .map_err(std::io::Error::other)?,
            None if self.holes == Holes::ReadAsZeros => buffer[..want].fill(0),
            None => {}
        }
        self.position += want as u64;
        Ok(want)
    }
}

// EXTERNAL(7-Zip 26.05 `7zz x` on ext): a file drops setuid, setgid and sticky, and a
// pipe, socket, device or inode of no type becomes a file of mode 0o666.
fn sevenzip_file_mode(mode: u16) -> u32 {
    if mode & TYPE_BITS == REGULAR {
        u32::from(mode) & 0o777
    } else {
        DEFAULT_FILE_MODE
    }
}

// EXTERNAL(7-Zip 26.05 `7zz x` on ext): a folder adds its owner's read, write and search rights.
fn sevenzip_folder_mode(mode: u16) -> u32 {
    (u32::from(mode) | 0o700) & 0o777
}

struct Writer<'o> {
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    leaders: HashMap<u32, String>,
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }
}

fn utf8_name(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes)
        .with_context(|| format!("ext name {:?} is not UTF-8", String::from_utf8_lossy(bytes)))
}

fn link_target(
    fs: &mut FileSystem,
    catalog: &Catalog,
    inode: &Inode,
    path: &str,
) -> Result<String> {
    ensure!(
        inode.size <= crate::MAX_LINK_TARGET && inode.size != 0,
        "ext link {path:?} has a target of {} bytes",
        inode.size
    );
    let bytes = match inode.content {
        Content::FastLink => {
            let (raw, _) = fs.inode(inode.number)?;
            raw[I_BLOCK..I_BLOCK + inode.size as usize].to_vec()
        }
        Content::Inline => fs.inline_data(inode.number)?,
        _ => {
            let mut bytes = Vec::new();
            FileData {
                fs,
                runs: catalog.runs_of(inode),
                size: inode.size,
                position: 0,
                holes: Holes::ReadAsZeros,
            }
            .read_to_end(&mut bytes)?;
            bytes
        }
    };
    ensure!(
        !bytes.contains(&0),
        "ext link {path:?} has a target with a NUL"
    );
    Ok(utf8_name(&bytes)
        .with_context(|| format!("ext link {path:?}"))?
        .to_owned())
}

fn write_file(
    fs: &mut FileSystem,
    catalog: &Catalog,
    inode: &Inode,
    path: &str,
    tree: &mut tree::Tree,
    holes: Holes,
) -> Result<()> {
    let mode = sevenzip_file_mode(inode.mode);
    match inode.content {
        Content::Inline => {
            let data = fs.inline_data(inode.number)?;
            tree.file(path, inode.size, mode, inode.modified, &mut data.as_slice())
        }
        Content::Empty | Content::FastLink => {
            tree.file(path, 0, mode, inode.modified, &mut std::io::empty())
        }
        Content::Runs => {
            let mut data = FileData {
                fs,
                runs: catalog.runs_of(inode),
                size: inode.size,
                position: 0,
                holes,
            };
            tree.file(path, inode.size, mode, inode.modified, &mut data)
        }
    }
}

fn emit(
    fs: &mut FileSystem,
    catalog: &Catalog,
    tree: &mut tree::Tree,
    options: ReadOptions,
    holes: Holes,
) -> Result<()> {
    let mut writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        leaders: HashMap::new(),
    };
    let root = catalog.index(ROOT).unwrap();
    let mut stack: Vec<(Range<u32>, Option<String>)> =
        vec![(catalog.inodes[root].children.clone(), None)];
    let synthetic = [
        (!catalog.system.is_empty(), SYSTEM_FOLDER),
        (!catalog.unknown.is_empty(), UNKNOWN_FOLDER),
    ];
    while let Some((mut children, parent)) = stack.pop() {
        let Some(index) = children.next() else {
            continue;
        };
        stack.push((children, parent.clone()));
        tree.check()?;
        let record = &catalog.records[index as usize];
        let name = utf8_name(catalog.name(record))?;
        if parent.is_none() {
            ensure!(
                !synthetic
                    .iter()
                    .any(|(used, folder)| *used && name == *folder),
                "the ext root folder holds {name:?}, the folder 7-Zip writes system files in"
            );
        }
        let path = match &parent {
            Some(parent) => format!("{parent}/{name}"),
            None => name.to_owned(),
        };
        let inode = &catalog.inodes[record.child as usize];
        match inode.mode & TYPE_BITS {
            DIRECTORY => {
                ensure!(
                    stack.len() <= MAX_DEPTH,
                    "ext folders nest deeper than {MAX_DEPTH}"
                );
                tree.directory(&path, sevenzip_folder_mode(inode.mode), inode.modified)?;
                stack.push((inode.children.clone(), Some(path)));
            }
            SYMLINK => {
                let target = link_target(fs, catalog, inode, &path)?;
                tree.symlink(&path, target)?;
            }
            _ => {
                if let Some(leader) = writer.leaders.get(&inode.number)
                    && writer.writes(&path)
                {
                    let leader = leader.clone();
                    tree.hard_link(&path, &leader)?;
                    continue;
                }
                write_file(fs, catalog, inode, &path, tree, holes)?;
                if inode.links > 1 && writer.writes(&path) {
                    writer.leaders.insert(inode.number, path);
                }
            }
        }
    }
    for (number, name) in &catalog.system {
        tree.check()?;
        let inode = &catalog.inodes[catalog.index(*number).unwrap()];
        write_file(
            fs,
            catalog,
            inode,
            &format!("{SYSTEM_FOLDER}/{name}"),
            tree,
            holes,
        )?;
    }
    for number in &catalog.unknown {
        tree.check()?;
        let inode = &catalog.inodes[catalog.index(*number).unwrap()];
        write_file(
            fs,
            catalog,
            inode,
            &format!("{UNKNOWN_FOLDER}/{number}"),
            tree,
            holes,
        )?;
    }
    Ok(())
}

fn read(
    volume: &mut dyn Volume,
    tree: &mut tree::Tree,
    options: ReadOptions,
    destination: Option<&Path>,
) -> Result<()> {
    let holes = if destination.is_some() {
        Holes::ReadAsZeros
    } else {
        Holes::LeftUnread
    };
    let mut fs = FileSystem::open(volume)?;
    let catalog = load(&mut fs)?;
    emit(&mut fs, &catalog, tree, options, holes)
}

pub(crate) fn is_volume(start: &[u8]) -> bool {
    start.len() >= SUPERBLOCK_AT + SUPERBLOCK_FIELDS
        && superblock_layout(
            start[SUPERBLOCK_AT..SUPERBLOCK_AT + SUPERBLOCK_FIELDS]
                .try_into()
                .unwrap(),
        )
        .is_ok()
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
            "read past the end of the ext image"
        );
        let mut buffer = buffer;
        let mut offset = offset;
        while !buffer.is_empty() {
            let read = self
                .file
                .read_at(buffer, offset)
                .context("cannot read the ext image")?;
            ensure!(read != 0, "truncated ext image");
            buffer = &mut buffer[read..];
            offset += read as u64;
        }
        Ok(())
    }
}

struct SeekVolume<V> {
    inner: V,
    length: u64,
}

impl<V: Read + Seek> Volume for SeekVolume<V> {
    fn size(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            offset
                .checked_add(buffer.len() as u64)
                .is_some_and(|end| end <= self.length),
            "read past the end of the ext partition"
        );
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner
            .read_exact(buffer)
            .context("cannot read the ext partition")
    }
}

fn opens_elsewhere(volume: &mut dyn Volume) -> bool {
    FileSystem::open(volume)
        .err()
        .is_some_and(|error| format!("{error:#}").contains(BACKEND_HINT))
}

/// Returns whether the file is a raw ext2, ext3 or ext4 volume whose
/// superblock at byte 1024 passes Zipper's layout checks.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let wanted = (SUPERBLOCK_AT + SUPERBLOCK_FIELDS) as u64;
    if file.len()? < wanted {
        return Ok(false);
    }
    let mut start = vec![0u8; wanted as usize];
    let mut volume = FileVolume {
        file: &file,
        length: wanted,
    };
    Ok(volume.read_exact_at(0, &mut start).is_ok() && is_volume(&start))
}

/// Returns whether the default engine sends this raw volume to 7-Zip: one
/// whose superblock names a feature Zipper does not read, such as encryption
/// or a journal that needs recovery.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    Ok(opens_elsewhere(&mut FileVolume {
        file: &file,
        length,
    }))
}

pub(crate) fn needs_backend_in<V: Read + Seek>(mut volume: V) -> Result<bool> {
    let length = volume.seek(SeekFrom::End(0))?;
    Ok(opens_elsewhere(&mut SeekVolume {
        inner: volume,
        length,
    }))
}

/// Verify and optionally extract an ext2, ext3 or ext4 volume held in a
/// bounded view, such as one partition, into a new directory, in the tree
/// `7zz x` writes, with `[SYS]` and `[UNKNOWN]` folders for the inodes no
/// folder names. Every check of `docs/reference/ext-reader.md` holds before
/// anything is published.
pub fn read_volume<V: Read + Seek>(
    mut volume: V,
    archive: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let length = volume.seek(SeekFrom::End(0))?;
    let mut volume = SeekVolume {
        inner: volume,
        length,
    };
    tree::extract("ext", archive, destination, options, |tree| {
        read(&mut volume, tree, options, destination)
    })
}

/// Verify and optionally extract a raw ext2, ext3 or ext4 volume image into
/// a new directory, as [`read_volume`] describes.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    let mut volume = FileVolume {
        file: &file,
        length,
    };
    tree::extract("ext", path, destination, options, |tree| {
        read(&mut volume, tree, options, destination)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn bitwise_crc32c(seed: u32, bytes: &[u8]) -> u32 {
        let mut crc = seed;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0x82F6_3B78 & (crc & 1).wrapping_neg());
            }
        }
        crc
    }

    #[test]
    fn checksums_match_their_published_check_values() {
        assert_eq!(!crc32c(u32::MAX, b"123456789"), 0xE306_9283);
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
        let bytes: Vec<u8> = (0..300u32).map(|value| (value * 7 + 3) as u8).collect();
        for length in 0..bytes.len() {
            assert_eq!(
                crc32c(0x1234_5678, &bytes[..length]),
                bitwise_crc32c(0x1234_5678, &bytes[..length])
            );
        }
    }

    #[test]
    fn times_read_as_7zip_reads_them() {
        let at = |seconds: i64, nanos: u32| {
            Some(if seconds >= 0 {
                SystemTime::UNIX_EPOCH + Duration::new(seconds as u64, nanos)
            } else {
                SystemTime::UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
                    + Duration::from_nanos(u64::from(nanos))
            })
        };
        assert_eq!(ext_time(0, None), None);
        assert_eq!(ext_time(0, Some(0)), None);
        assert_eq!(ext_time(0, Some(0x10)), at(0, 4));
        assert_eq!(ext_time(0xED2F_7BE0, Some(0)), at(-315_655_200, 0));
        assert_eq!(ext_time(0x8000_0001, Some(1)), at(2_147_483_649, 0));
        assert_eq!(ext_time(0xFFFF_FFFF, Some(2)), at(8_589_934_591, 0));
        assert_eq!(
            ext_time(0x7FFF_FFFF, Some(1 | 1023 << 2)),
            at(6_442_450_943, 1023)
        );
        assert_eq!(ext_time(100, Some(0xFFFF_FFFC)), at(100, 0));
        assert_eq!(ext_time(0x8000_0000, None), at(-2_147_483_648, 0));
    }

    #[test]
    fn modes_follow_7zip() {
        assert_eq!(sevenzip_file_mode(REGULAR | 0o4755), 0o755);
        assert_eq!(sevenzip_file_mode(FIFO | 0o600), DEFAULT_FILE_MODE);
        assert_eq!(sevenzip_file_mode(0), DEFAULT_FILE_MODE);
        assert_eq!(sevenzip_folder_mode(DIRECTORY | 0o055), 0o755);
    }

    #[test]
    fn sparse_super_backups_lie_in_groups_0_1_and_powers_of_3_5_and_7() {
        let mut bytes = [0u8; SUPERBLOCK_FIELDS];
        bytes[0x38..0x3A].copy_from_slice(&MAGIC.to_le_bytes());
        bytes[0x14] = 1;
        bytes[0x20..0x24].copy_from_slice(&8192u32.to_le_bytes());
        bytes[0x24..0x28].copy_from_slice(&8192u32.to_le_bytes());
        bytes[0x28..0x2C].copy_from_slice(&8u32.to_le_bytes());
        bytes[0x04..0x08].copy_from_slice(&(1 + 8192 * 50u32).to_le_bytes());
        bytes[0x00..0x04].copy_from_slice(&400u32.to_le_bytes());
        bytes[0x64] = RO_COMPAT_SPARSE_SUPER as u8;
        let fields = superblock_layout(&bytes).unwrap();
        let backups: Vec<u32> = (0..50)
            .filter(|&group| fields.holds_superblock(group))
            .collect();
        assert_eq!(backups, [0, 1, 3, 5, 7, 9, 25, 27, 49]);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn bitwise_crc32c(seed: u32, bytes: &[u8]) -> u32 {
        let mut crc = seed;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
            }
        }
        crc
    }

    fn bitwise_crc16(seed: u16, bytes: &[u8]) -> u16 {
        let mut crc = seed;
        for byte in bytes {
            crc ^= u16::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xA001
                } else {
                    crc >> 1
                };
            }
        }
        crc
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn checksums_equal_a_bit_at_a_time_reference() {
        let bytes: [u8; 2] = kani::any();
        let seed: u32 = kani::any();
        assert!(crc32c(seed, &bytes) == bitwise_crc32c(seed, &bytes));
        let seed16: u16 = kani::any();
        assert!(crc16(seed16, &bytes) == bitwise_crc16(seed16, &bytes));
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn superblocks_keep_a_layout_that_fits_their_volume() {
        let bytes: [u8; SUPERBLOCK_FIELDS] = kani::any();
        if let Ok(fields) = superblock_layout(&bytes) {
            assert!((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&fields.block_log));
            assert!(fields.blocks <= u64::MAX >> fields.block_log);
            assert!(fields.inode_size as usize <= fields.block_size());
            assert!(fields.descriptor_size as usize <= fields.block_size());
            assert!(fields.descriptor_size >= SMALL_DESCRIPTOR);
            assert!(fields.blocks_per_group >= 8 && fields.inodes_per_group >= 1);
            assert!(
                u64::from(fields.groups) * u64::from(fields.inodes_per_group)
                    <= u64::from(u32::MAX)
            );
            let _ = fields.largest_block_map_file();
        }
    }

    #[kani::proof]
    #[kani::unwind(25)]
    fn every_group_descriptor_lies_inside_its_block() {
        let block_log: u32 = kani::any();
        kani::assume((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_log));
        let descriptor_log: u32 = kani::any();
        kani::assume((5..=10).contains(&descriptor_log) && descriptor_log <= block_log);
        let fields = Superblock {
            block_log,
            blocks: kani::any(),
            first_data_block: kani::any::<bool>() as u32,
            blocks_per_group: kani::any(),
            inodes_per_group: 1,
            groups: kani::any(),
            inode_size: GOOD_OLD_INODE as u32,
            first_inode: GOOD_OLD_FIRST_INODE,
            descriptor_size: 1 << descriptor_log,
            reserved_descriptor_blocks: kani::any(),
            first_meta_group: kani::any(),
            backup_groups: kani::any(),
            compat: kani::any(),
            incompat: kani::any(),
            ro_compat: kani::any(),
            uuid: [0; 16],
            checksum_type: 0,
            checksum_seed: 0,
            last_orphan: 0,
        };
        kani::assume(fields.blocks_per_group <= 8 << block_log);
        let group: u32 = kani::any();
        kani::assume(group < fields.groups);
        let (_, offset) = fields.descriptor_place(group);
        assert!(offset + fields.descriptor_size as usize <= fields.block_size());
    }

    #[kani::proof]
    fn group_descriptors_never_panic() {
        let bytes: [u8; 64] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let wide: bool = kani::any();
        if group_fields(&bytes[..length], wide).is_ok() {
            assert!(length >= if wide { 64 } else { 32 });
        }
    }

    #[kani::proof]
    fn inodes_keep_their_extra_fields_inside_the_inode() {
        let bytes: [u8; 160] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(fields) = inode_fields(&bytes[..length], kani::any()) {
            assert!(length >= GOOD_OLD_INODE);
            assert!(fields.extra_end <= length && fields.extra_end % 4 == 0);
            assert!(fields.mtime_extra.is_none() || fields.extra_end >= MTIME_EXTRA_END);
        }
    }

    #[kani::proof]
    fn times_never_panic() {
        let _ = ext_time(kani::any(), kani::any());
    }

    #[kani::proof]
    fn extent_entries_stay_inside_their_node_and_the_logical_range() {
        let node: [u8; EXTENT_HEADER + 3 * EXTENT_ENTRY] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= node.len());
        let in_inode: bool = kani::any();
        if let Ok(header) = extent_header(&node[..length], in_inode) {
            let room = if in_inode {
                length - EXTENT_HEADER
            } else {
                length - EXTENT_HEADER - EXTENT_TAIL
            };
            assert!(header.entries <= header.max);
            assert!(usize::from(header.max) * EXTENT_ENTRY <= room);
            assert!(header.depth <= MAX_EXTENT_DEPTH);
        }
        let index: usize = kani::any();
        kani::assume(index < 4);
        if let Ok(extent) = extent_entry(&node[..length], index, kani::any()) {
            assert!(EXTENT_HEADER + (index + 1) * EXTENT_ENTRY <= length);
            assert!(u64::from(extent.logical) + u64::from(extent.length) <= 1 << 32);
            assert!(extent.physical < 1 << 48);
            assert!(extent.length <= u32::from(LARGEST_INITIALIZED_EXTENT));
        }
    }

    #[kani::proof]
    fn folder_records_keep_their_names_inside_and_always_advance() {
        let block: [u8; 24] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        let at: usize = kani::any();
        if let Ok(entry) = dir_entry(&block[..length], at, kani::any()) {
            assert!(entry.length >= DIR_TAIL && at + entry.length <= length);
            assert!(entry.name.start == at + DIR_ENTRY_HEADER);
            assert!(entry.name.end <= at + entry.length);
        }
        let _ = dir_tail(&block[..length]);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn inline_data_lies_inside_the_inode() {
        let inode: [u8; 48] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= inode.len());
        let extra_end: usize = kani::any();
        kani::assume(extra_end <= length);
        if let Ok(value) = inline_value(&inode[..length], extra_end) {
            assert!(value.start <= value.end && value.end <= length);
        }
    }
}
