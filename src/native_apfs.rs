//! In-process extraction of APFS containers: raw, inside a DMG image and
//! inside a GPT disk. See `docs/reference/apfs-reader.md` for what it
//! accepts and restores.
use crate::hfs::{self, Volume};
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

const HEADER_BLOCK: usize = 4096;
const OBJECT_HEADER: usize = 0x20;
const SMALLEST_BLOCK_LOG: u32 = 12;
const LARGEST_BLOCK_LOG: u32 = 16;
const SIZE_LIMIT: u64 = 1 << 62;
const NXSB: &[u8; 4] = b"NXSB";
const APSB: &[u8; 4] = b"APSB";
const SUPERBLOCK_OID: u64 = 1;
pub(crate) const MAX_VOLUMES: usize = 100;
pub(crate) const SUPERBLOCK_FIELDS: usize = 0xB8 + 8 * MAX_VOLUMES;
pub(crate) const VOLUME_FIELDS: usize = 0x414;
pub(crate) const OBJECT_MAP_FIELDS: usize = 0x38;
pub(crate) const CHECKPOINT_HEADER: usize = 0x28;
pub(crate) const CHECKPOINT_MAPPING: usize = 40;
pub(crate) const NODE_HEADER: usize = 0x38;
pub(crate) const TREE_INFO: usize = 0x28;
pub(crate) const INODE: usize = 0x5C;
const XFIELD_HEADER: usize = 4;
const XFIELD: usize = 4;
const DSTREAM: usize = 40;
const DIRECTORY_VALUE: usize = 0x12;
const FILE_EXTENT_KEY: usize = 16;
const FILE_EXTENT_VALUE: usize = 24;
const OMAP_KEY: usize = 16;
const OMAP_VALUE: usize = 16;
const XATTR_VALUE: usize = 4;
const XATTR_STREAM: usize = 8 + DSTREAM;

const TYPE_MASK: u32 = 0xFFFF;
const STORAGE_MASK: u32 = 0xC000_0000;
const PHYSICAL: u32 = 0x4000_0000;
const VIRTUAL: u32 = 0;
const NX_SUPERBLOCK: u32 = 1;
const BTREE: u32 = 2;
const BTREE_NODE: u32 = 3;
const OMAP: u32 = 0xB;
const CHECKPOINT_MAP: u32 = 0xC;
const FS: u32 = 0xD;
const FSTREE: u32 = 0xE;

const NOT_CONTIGUOUS: u32 = 0x8000_0000;
const CHECKPOINT_MAP_LAST: u32 = 1;
const NX_INCOMPAT_FUSION: u64 = 0x100;

const OMAP_DELETED: u32 = 1;
const OMAP_ENCRYPTED: u32 = 4;
const OMAP_NOHEADER: u32 = 8;

const NODE_ROOT: u16 = 1;
const NODE_LEAF: u16 = 2;
const NODE_FIXED: u16 = 4;
const NODE_HASHED: u16 = 8;
const NODE_NOHEADER: u16 = 0x10;
const TREE_EPHEMERAL: u32 = 8;
const TREE_PHYSICAL: u32 = 0x10;
const TREE_HASHED: u32 = 0x80;
const TREE_NOHEADER: u32 = 0x100;
const GHOST: u16 = 0xFFFF;

const VOLUME_CASE_INSENSITIVE: u64 = 1;
const VOLUME_ENCRYPTION_ROLLED: u64 = 4;
const VOLUME_NORMALIZATION_INSENSITIVE: u64 = 8;
const VOLUME_SEALED: u64 = 0x20;
const VOLUME_UNENCRYPTED: u64 = 1;

const RECORD_ID_MASK: u64 = 0x0FFF_FFFF_FFFF_FFFF;
const RECORD_TYPE_SHIFT: u32 = 60;
const INODE_RECORD: u8 = 3;
const XATTR_RECORD: u8 = 4;
const SIBLING_LINK_RECORD: u8 = 5;
const DSTREAM_ID_RECORD: u8 = 6;
const FILE_EXTENT_RECORD: u8 = 8;
const DIRECTORY_RECORD: u8 = 9;
const SIBLING_MAP_RECORD: u8 = 12;
const FILE_INFO_RECORD: u8 = 13;
const NAME_LENGTH_MASK: u32 = 0x3FF;
const EXTENT_LENGTH_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

const XFIELD_NAME: u8 = 4;
const XFIELD_DSTREAM: u8 = 8;
const XATTR_STREAM_FLAG: u16 = 1;
const XATTR_EMBEDDED_FLAG: u16 = 2;
const HAS_UNCOMPRESSED_SIZE: u64 = 0x0004_0000;

const ROOT_PARENT: u64 = 1;
const ROOT_FOLDER: u64 = 2;
const PRIVATE_FOLDER: u64 = 3;
const PRIVATE_FOLDER_NAME: &str = "private-dir";
const UNKNOWN_KIND: u16 = 0;
const KIND_SHIFT: u32 = 12;
const TYPE_BITS: u16 = 0o170_000;
const DIRECTORY: u16 = 0o040_000;
const REGULAR: u16 = 0o100_000;
const SYMLINK: u16 = 0o120_000;
const DEFAULT_FILE_MODE: u32 = 0o666;
const DEFAULT_FOLDER_MODE: u32 = 0o777;

const DECMPFS: &[u8] = b"com.apple.decmpfs";
const RESOURCE_FORK: &[u8] = b"com.apple.ResourceFork";
const SYMLINK_TARGET: &[u8] = b"com.apple.fs.symlink";
const LARGEST_DECMPFS_STREAM: u64 = 1 << 16;
const COMPRESSED_CHUNK: usize = 1 << 16;
const LARGEST_COMPRESSED_CHUNK: usize = COMPRESSED_CHUNK + 1;
const ZLIB_FORK_HEADER: usize = 0x100;
const RESOURCE_MAP: u32 = 50;
const MAX_CHUNKS: u64 = 1 << 29;
const LZVN_STORED_MARKER: u8 = 0x06;
const ZLIB_STORED_MARKER: u8 = 0x0F;
const DEFLATE_EXPANSION: u64 = 1032;
const LZVN_EXPANSION: u64 = 272;

const MAX_CHECKPOINT_BLOCKS: u32 = 1 << 16;
const MAX_TREE_LEVELS: u16 = 64;
const MAX_NODES: u64 = 1 << 24;
const MAX_KEYS: u64 = 1 << 26;
const MAX_ENTRIES: usize = tree::MAX_ENTRIES + 16;
const MAX_EXTENTS: usize = 1 << 24;
const MAX_NAMES: usize = 64 * 1024 * 1024;
const MAX_ATTRIBUTES: usize = tree::MAX_ENTRIES;
const MAX_DEPTH: usize = 1024;
const COPY_PIECE: usize = 1 << 20;
const FLETCHER_MODULUS: u64 = 0xFFFF_FFFF;
const FLETCHER_RUN: usize = 1024;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

// EXTERNAL(7-Zip ApfsHandler Fletcher64): Fletcher-64 over 32-bit words modulo 2^32 - 1,
// stored as the two check values that make the whole object sum to zero.
pub(crate) fn fletcher64(words: &[u8]) -> u64 {
    let (mut low, mut high) = (0u64, 0u64);
    for run in words.chunks(FLETCHER_RUN * 4) {
        for word in run.chunks_exact(4) {
            low += u64::from(u32::from_le_bytes(word.try_into().unwrap()));
            high += low;
        }
        low %= FLETCHER_MODULUS;
        high %= FLETCHER_MODULUS;
    }
    let check_low = FLETCHER_MODULUS - (low + high) % FLETCHER_MODULUS;
    let check_high = FLETCHER_MODULUS - (low + check_low) % FLETCHER_MODULUS;
    check_high << 32 | check_low
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Object {
    pub(crate) oid: u64,
    pub(crate) xid: u64,
    pub(crate) kind: u32,
    pub(crate) storage: u32,
    pub(crate) subtype: u32,
}

pub(crate) fn object(block: &[u8]) -> Field<Object> {
    field_check(
        block.len() >= OBJECT_HEADER && block.len().is_multiple_of(4),
        "APFS object is not a whole number of words after its header",
    )?;
    field_check(
        le64(block, 0) == fletcher64(&block[8..]),
        "APFS object fails its Fletcher-64 checksum",
    )?;
    let kind = le32(block, 0x18);
    Ok(Object {
        oid: le64(block, 8),
        xid: le64(block, 0x10),
        kind: kind & TYPE_MASK,
        storage: kind & STORAGE_MASK,
        subtype: le32(block, 0x1C),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) base: u64,
    pub(crate) blocks: u32,
}

impl Area {
    fn fits(&self, block_count: u64) -> bool {
        self.base
            .checked_add(u64::from(self.blocks))
            .is_some_and(|end| end <= block_count)
    }

    fn at(&self, index: u32) -> u64 {
        self.base + u64::from(index % self.blocks)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Superblock {
    pub(crate) xid: u64,
    pub(crate) block_log: u32,
    pub(crate) block_count: u64,
    pub(crate) incompatible: u64,
    pub(crate) uuid: [u8; 16],
    pub(crate) descriptors: Area,
    pub(crate) data: Area,
    pub(crate) first_descriptor: u32,
    pub(crate) descriptor_count: u32,
    pub(crate) omap: u64,
    pub(crate) max_volumes: u32,
    pub(crate) volumes: [u64; MAX_VOLUMES],
}

// EXTERNAL(7-Zip ApfsHandler CSuperBlock::Parse): object 1 of type NX_SUPERBLOCK with
// subtype 0, a block of 4 KiB to 64 KiB, at most 2^62 bytes and at most 100 volumes.
pub(crate) fn superblock(bytes: &[u8; SUPERBLOCK_FIELDS]) -> Field<Superblock> {
    field_check(
        &bytes[0x20..0x24] == NXSB,
        "APFS container superblock has no NXSB signature",
    )?;
    field_check(
        le64(bytes, 8) == SUPERBLOCK_OID
            && le32(bytes, 0x18) & TYPE_MASK == NX_SUPERBLOCK
            && le32(bytes, 0x1C) == 0,
        "APFS container superblock is not object 1 of its type",
    )?;
    let block_size = le32(bytes, 0x24);
    field_check(
        block_size.is_power_of_two()
            && (SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_size.trailing_zeros()),
        "APFS block size is not a power of two from 4 KiB to 64 KiB",
    )?;
    let block_log = block_size.trailing_zeros();
    let block_count = le64(bytes, 0x28);
    field_check(
        block_count <= SIZE_LIMIT >> block_log,
        "APFS container is larger than 2^62 bytes",
    )?;
    let max_volumes = le32(bytes, 0xB4);
    field_check(
        max_volumes as usize <= MAX_VOLUMES,
        "APFS container allows more than 100 volumes",
    )?;
    let descriptor_blocks = le32(bytes, 0x68);
    let data_blocks = le32(bytes, 0x6C);
    field_check(
        (descriptor_blocks | data_blocks) & NOT_CONTIGUOUS == 0,
        "APFS checkpoint area is a B-tree, which Zipper does not read",
    )?;
    let descriptors = Area {
        base: le64(bytes, 0x70),
        blocks: descriptor_blocks,
    };
    let data = Area {
        base: le64(bytes, 0x78),
        blocks: data_blocks,
    };
    field_check(
        descriptors.blocks != 0
            && descriptors.blocks <= MAX_CHECKPOINT_BLOCKS
            && descriptors.fits(block_count)
            && data.fits(block_count),
        "APFS checkpoint areas lie outside the container",
    )?;
    let first_descriptor = le32(bytes, 0x88);
    let descriptor_count = le32(bytes, 0x8C);
    field_check(
        first_descriptor < descriptors.blocks
            && descriptor_count != 0
            && descriptor_count <= descriptors.blocks,
        "APFS checkpoint lies outside its descriptor area",
    )?;
    let mut volumes = [0u64; MAX_VOLUMES];
    for (index, volume) in volumes.iter_mut().enumerate() {
        *volume = le64(bytes, 0xB8 + 8 * index);
    }
    Ok(Superblock {
        xid: le64(bytes, 0x10),
        block_log,
        block_count,
        incompatible: le64(bytes, 0x40),
        uuid: bytes[0x48..0x58].try_into().unwrap(),
        descriptors,
        data,
        first_descriptor,
        descriptor_count,
        omap: le64(bytes, 0xA0),
        max_volumes,
        volumes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CheckpointMap {
    pub(crate) last: bool,
    pub(crate) count: usize,
}

pub(crate) fn checkpoint_map(block: &[u8]) -> Field<CheckpointMap> {
    field_check(
        block.len() >= CHECKPOINT_HEADER
            && le32(block, 0x18) & TYPE_MASK == CHECKPOINT_MAP
            && le32(block, 0x18) & STORAGE_MASK == PHYSICAL,
        "APFS checkpoint block is not a physical checkpoint map",
    )?;
    let count = le32(block, 0x24) as usize;
    field_check(
        count <= (block.len() - CHECKPOINT_HEADER) / CHECKPOINT_MAPPING,
        "APFS checkpoint map counts more mappings than its block holds",
    )?;
    Ok(CheckpointMap {
        last: le32(block, 0x20) & CHECKPOINT_MAP_LAST != 0,
        count,
    })
}

pub(crate) fn checkpoint_mapping(
    bytes: &[u8; CHECKPOINT_MAPPING],
    data: Area,
    block_log: u32,
) -> Field<Range<u64>> {
    let size = le32(bytes, 8);
    field_check(
        size != 0 && size.trailing_zeros() >= block_log,
        "APFS checkpoint mapping's object is not a whole number of blocks",
    )?;
    let blocks = u64::from(size >> block_log);
    let address = le64(bytes, 32);
    field_check(
        address >= data.base
            && address
                .checked_add(blocks)
                .is_some_and(|end| end <= data.base + u64::from(data.blocks)),
        "APFS checkpoint mapping's object lies outside the checkpoint data area",
    )?;
    Ok(address..address + blocks)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObjectMap {
    pub(crate) snapshots: u32,
    pub(crate) root: u64,
}

pub(crate) fn object_map(bytes: &[u8; OBJECT_MAP_FIELDS]) -> Field<ObjectMap> {
    field_check(
        le32(bytes, 0x18) & TYPE_MASK == OMAP,
        "APFS object map is not of the object map type",
    )?;
    field_check(
        le32(bytes, 0x28) == PHYSICAL | BTREE,
        "APFS object map's tree is not a physical B-tree",
    )?;
    Ok(ObjectMap {
        snapshots: le32(bytes, 0x24),
        root: le64(bytes, 0x30),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeInfo {
    pub(crate) flags: u32,
    pub(crate) node_size: u32,
    pub(crate) key_size: u32,
    pub(crate) value_size: u32,
    pub(crate) keys: u64,
    pub(crate) nodes: u64,
}

fn tree_info(bytes: &[u8]) -> TreeInfo {
    TreeInfo {
        flags: le32(bytes, 0),
        node_size: le32(bytes, 4),
        key_size: le32(bytes, 8),
        value_size: le32(bytes, 12),
        keys: le64(bytes, 0x18),
        nodes: le64(bytes, 0x20),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) flags: u16,
    pub(crate) level: u16,
    pub(crate) keys: usize,
    pub(crate) key_area: usize,
    pub(crate) value_end: usize,
    pub(crate) info: Option<TreeInfo>,
}

impl Node {
    fn is_leaf(&self) -> bool {
        self.flags & NODE_LEAF != 0
    }

    fn is_fixed(&self) -> bool {
        self.flags & NODE_FIXED != 0
    }
}

// EXTERNAL(7-Zip ApfsHandler ReadMap): a leaf is level 0 and only a leaf, the table of
// contents ends inside the node, and a root keeps its tree information at its end.
pub(crate) fn node(block: &[u8]) -> Field<Node> {
    field_check(
        block.len() >= NODE_HEADER,
        "APFS B-tree node is shorter than its header",
    )?;
    let flags = le16(block, 0x20);
    let level = le16(block, 0x22);
    let keys = le32(block, 0x24) as usize;
    let (table, table_length) = (
        usize::from(le16(block, 0x28)),
        usize::from(le16(block, 0x2A)),
    );
    field_check(
        (flags & NODE_LEAF != 0) == (level == 0),
        "APFS B-tree node's level disagrees with its leaf flag",
    )?;
    let mut value_end = block.len();
    let info = if flags & NODE_ROOT != 0 {
        field_check(
            value_end >= NODE_HEADER + TREE_INFO,
            "APFS B-tree root is too short for its tree information",
        )?;
        value_end -= TREE_INFO;
        Some(tree_info(&block[value_end..]))
    } else {
        None
    };
    field_check(
        table == 0 && NODE_HEADER + table_length <= value_end,
        "APFS B-tree node's table of contents lies outside the node",
    )?;
    let entry = if flags & NODE_FIXED != 0 { 4 } else { 8 };
    field_check(
        keys <= table_length / entry,
        "APFS B-tree node counts more keys than its table of contents holds",
    )?;
    Ok(Node {
        flags,
        level,
        keys,
        key_area: NODE_HEADER + table_length,
        value_end,
        info,
    })
}

// EXTERNAL(7-Zip ApfsHandler ReadMap): fixed entries name a key and a value offset, variable
// entries a key offset and length and a value offset from the end and length.
pub(crate) fn entry(
    node: &Node,
    block: &[u8],
    index: usize,
    sizes: (u32, u32),
) -> Field<(Range<usize>, Range<usize>)> {
    field_check(
        index < node.keys && node.value_end <= block.len() && node.key_area <= node.value_end,
        "APFS B-tree entry lies past its node's table of contents",
    )?;
    let room = node.value_end - node.key_area;
    if node.is_fixed() {
        let at = NODE_HEADER + index * 4;
        let (key, value) = (
            usize::from(le16(block, at)),
            usize::from(le16(block, at + 2)),
        );
        let (key_size, value_size) = (sizes.0 as usize, sizes.1 as usize);
        let value_size = if node.is_leaf() { value_size } else { 8 };
        field_check(
            key_size <= room && key <= room - key_size && value <= room && value >= value_size,
            "APFS B-tree entry lies outside its node",
        )?;
        let key_start = node.key_area + key;
        let value_start = node.value_end - value;
        return Ok((
            key_start..key_start + key_size,
            value_start..value_start + value_size,
        ));
    }
    let at = NODE_HEADER + index * 8;
    let (key, key_length) = (
        usize::from(le16(block, at)),
        usize::from(le16(block, at + 2)),
    );
    let (value, value_length) = (
        usize::from(le16(block, at + 4)),
        usize::from(le16(block, at + 6)),
    );
    field_check(
        key < room
            && key_length <= room - key
            && value <= room
            && value_length <= value
            && le16(block, at + 4) != GHOST,
        "APFS B-tree entry lies outside its node",
    )?;
    let key_start = node.key_area + key;
    let value_start = node.value_end - value;
    Ok((
        key_start..key_start + key_length,
        value_start..value_start + value_length,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VolumeSuperblock {
    pub(crate) incompatible: u64,
    pub(crate) omap: u64,
    pub(crate) root_tree: u64,
    pub(crate) snapshots: u64,
    pub(crate) flags: u64,
    pub(crate) integrity: u64,
    pub(crate) extent_tree: u64,
}

// EXTERNAL(7-Zip ApfsHandler CApfs::Parse): an APSB object of type FS, with its object map,
// root tree, counts, flags and the sealed-volume trees at their fixed offsets.
pub(crate) fn volume_superblock(bytes: &[u8; VOLUME_FIELDS]) -> Field<VolumeSuperblock> {
    field_check(
        &bytes[0x20..0x24] == APSB && le32(bytes, 0x18) & TYPE_MASK == FS,
        "APFS volume superblock has no APSB signature",
    )?;
    Ok(VolumeSuperblock {
        incompatible: le64(bytes, 0x38),
        omap: le64(bytes, 0x80),
        root_tree: le64(bytes, 0x88),
        snapshots: le64(bytes, 0xD8),
        flags: le64(bytes, 0x108),
        integrity: le64(bytes, 0x400),
        extent_tree: le64(bytes, 0x408),
    })
}

pub(crate) fn record_key(key: &[u8]) -> Field<(u64, u8)> {
    field_check(
        key.len() >= 8,
        "APFS file-system key is shorter than 8 bytes",
    )?;
    let header = le64(key, 0);
    Ok((header & RECORD_ID_MASK, (header >> RECORD_TYPE_SHIFT) as u8))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DataStream {
    pub(crate) size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InodeRecord {
    pub(crate) private: u64,
    pub(crate) modified: u64,
    pub(crate) flags: u64,
    pub(crate) mode: u16,
    pub(crate) uncompressed: u64,
    pub(crate) stream: Option<DataStream>,
}

// EXTERNAL(7-Zip ApfsHandler OpenVolume): the extended fields follow the fixed inode as a
// count, a used size, 4-byte descriptors and data padded to 8 bytes that ends the value.
pub(crate) fn inode_record(value: &[u8]) -> Field<InodeRecord> {
    field_check(
        value.len() >= INODE,
        "APFS inode is shorter than its fixed fields",
    )?;
    let mut record = InodeRecord {
        private: le64(value, 8),
        modified: le64(value, 0x18),
        flags: le64(value, 0x30),
        mode: le16(value, 0x50),
        uncompressed: le64(value, 0x54),
        stream: None,
    };
    let extra = &value[INODE..];
    if extra.is_empty() {
        return Ok(record);
    }
    field_check(
        extra.len() >= XFIELD_HEADER,
        "APFS inode's extended fields are shorter than their header",
    )?;
    let count = usize::from(le16(extra, 0));
    let used = usize::from(le16(extra, 2));
    let mut offset = XFIELD_HEADER + count * XFIELD;
    field_check(
        offset + used == extra.len(),
        "APFS inode's extended fields do not fill the inode",
    )?;
    for index in 0..count {
        let descriptor = XFIELD_HEADER + index * XFIELD;
        let kind = extra[descriptor];
        let size = usize::from(le16(extra, descriptor + 2));
        let padded = (size + 7) & !7;
        field_check(
            padded <= extra.len() - offset,
            "APFS inode's extended field runs past the inode",
        )?;
        match kind {
            XFIELD_NAME => field_check(size >= 2, "APFS inode's name field is too short")?,
            XFIELD_DSTREAM => {
                field_check(
                    size == DSTREAM && record.stream.is_none(),
                    "APFS inode has a bad or second data stream field",
                )?;
                record.stream = Some(DataStream {
                    size: le64(extra, offset),
                });
            }
            _ => {}
        }
        offset += padded;
    }
    field_check(
        offset == extra.len(),
        "APFS inode's extended fields do not fill the inode",
    )?;
    Ok(record)
}

fn terminated_name(bytes: &[u8]) -> Field<Range<usize>> {
    let length = bytes.len();
    field_check(
        length >= 1 && bytes[length - 1] == 0 && !bytes[..length - 1].contains(&0),
        "APFS name does not end at its only NUL",
    )?;
    Ok(0..length - 1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryRecord {
    pub(crate) name: Range<usize>,
    pub(crate) file: u64,
    pub(crate) kind: u16,
}

// EXTERNAL(7-Zip ApfsHandler OpenVolume): a hashed key holds a 10-bit length beside its
// hash, a plain key a 16-bit length, and the name with its NUL ends the key.
pub(crate) fn directory_record(key: &[u8], value: &[u8], hashed: bool) -> Field<DirectoryRecord> {
    let start = if hashed { 12 } else { 10 };
    field_check(
        key.len() > start,
        "APFS directory record's key is too short",
    )?;
    let length = if hashed {
        (le32(key, 8) & NAME_LENGTH_MASK) as usize
    } else {
        usize::from(le16(key, 8))
    };
    field_check(
        start + length == key.len(),
        "APFS directory record's name does not end its key",
    )?;
    let name = terminated_name(&key[start..])?;
    field_check(
        value.len() >= DIRECTORY_VALUE,
        "APFS directory record's value is too short",
    )?;
    Ok(DirectoryRecord {
        name: start + name.start..start + name.end,
        file: le64(value, 0),
        kind: le16(value, 0x10),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XattrData {
    Embedded(Range<usize>),
    Stream { id: u64, size: u64 },
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XattrRecord {
    pub(crate) name: Range<usize>,
    pub(crate) data: XattrData,
}

// EXTERNAL(7-Zip ApfsHandler OpenVolume): a 16-bit name length and the NUL-ended name fill
// the key, and the value's flags pick embedded data or a data stream of exactly 48 bytes.
pub(crate) fn xattr_record(key: &[u8], value: &[u8]) -> Field<XattrRecord> {
    field_check(key.len() > 10, "APFS attribute's key is too short")?;
    field_check(
        10 + usize::from(le16(key, 8)) == key.len(),
        "APFS attribute's name does not end its key",
    )?;
    let name = terminated_name(&key[10..])?;
    field_check(
        value.len() >= XATTR_VALUE && XATTR_VALUE + usize::from(le16(value, 2)) == value.len(),
        "APFS attribute's data does not fill its value",
    )?;
    let flags = le16(value, 0);
    let data = if flags & XATTR_EMBEDDED_FLAG != 0 {
        XattrData::Embedded(XATTR_VALUE..value.len())
    } else if flags & XATTR_STREAM_FLAG != 0 {
        field_check(
            value.len() == XATTR_VALUE + XATTR_STREAM,
            "APFS attribute's data stream has the wrong size",
        )?;
        XattrData::Stream {
            id: le64(value, XATTR_VALUE),
            size: le64(value, XATTR_VALUE + 8),
        }
    } else {
        XattrData::Other
    };
    Ok(XattrRecord {
        name: 10 + name.start..10 + name.end,
        data,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileExtent {
    pub(crate) logical: u64,
    pub(crate) length: u64,
    pub(crate) block: u64,
}

pub(crate) fn file_extent_record(key: &[u8], value: &[u8]) -> Field<FileExtent> {
    field_check(
        key.len() == FILE_EXTENT_KEY && value.len() == FILE_EXTENT_VALUE,
        "APFS file extent has the wrong size",
    )?;
    Ok(FileExtent {
        logical: le64(key, 8),
        length: le64(value, 0) & EXTENT_LENGTH_MASK,
        block: le64(value, 8),
    })
}

#[derive(Clone, Copy, Debug)]
struct MapEntry {
    oid: u64,
    xid: u64,
    flags: u32,
    size: u32,
    address: u64,
}

struct Omap {
    entries: Vec<MapEntry>,
}

impl Omap {
    fn find(&self, oid: u64) -> Option<MapEntry> {
        let index = self
            .entries
            .binary_search_by_key(&oid, |entry| entry.oid)
            .ok()?;
        Some(self.entries[index])
    }
}

struct Disk<'v> {
    volume: &'v mut dyn Volume,
    block_log: u32,
    block_count: u64,
    xid: u64,
}

impl Disk<'_> {
    fn block_size(&self) -> usize {
        1 << self.block_log
    }

    fn read_block(&mut self, address: u64, buffer: &mut [u8]) -> Result<()> {
        ensure!(
            address != 0 && address < self.block_count,
            "APFS object at block {address} lies outside the container"
        );
        self.volume
            .read_exact_at(address << self.block_log, buffer)
            .with_context(|| format!("cannot read APFS block {address}"))
    }

    fn read_object(&mut self, address: u64, buffer: &mut [u8], what: &str) -> Result<Object> {
        self.read_block(address, buffer)?;
        let header =
            object(buffer).with_context(|| format!("APFS {what} at block {address} is damaged"))?;
        ensure!(
            header.xid <= self.xid,
            "APFS {what} at block {address} is newer than its checkpoint"
        );
        Ok(header)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        self.volume.read_exact_at(offset, buffer)
    }
}

#[derive(Clone, Copy)]
struct TreeKind {
    what: &'static str,
    subtype: u32,
    physical: bool,
    fixed: Option<(u32, u32)>,
}

const OMAP_TREE: TreeKind = TreeKind {
    what: "object map",
    subtype: OMAP,
    physical: true,
    fixed: Some((OMAP_KEY as u32, OMAP_VALUE as u32)),
};

const FILE_TREE: TreeKind = TreeKind {
    what: "file-system tree",
    subtype: FSTREE,
    physical: false,
    fixed: None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Location {
    node: u64,
    index: u16,
}

struct Level {
    address: u64,
    block: Vec<u8>,
    node: Node,
    next: usize,
}

// EXTERNAL(7-Zip ApfsHandler ReadMap): nodes are visited depth first, an index node's values
// name its children, and the root's counts of nodes and keys must match the walk.
fn walk(
    disk: &mut Disk,
    root: u64,
    root_oid: u64,
    kind: TreeKind,
    omap: Option<&Omap>,
    mut visit: impl FnMut(&[u8], &[u8], Location) -> Result<()>,
) -> Result<()> {
    let what = kind.what;
    let size = disk.block_size();
    let mut levels: Vec<Level> = Vec::new();
    let mut spare: Vec<Vec<u8>> = Vec::new();
    let mut seen = HashSet::new();
    let mut info: Option<TreeInfo> = None;
    let mut nodes = 0u64;
    let mut keys = 0u64;
    let mut pending = Some((root, root_oid));
    loop {
        if let Some((address, oid)) = pending.take() {
            ensure!(
                seen.insert(address),
                "APFS {what} reaches node {address} twice"
            );
            let mut block = spare.pop().unwrap_or_else(|| vec![0u8; size]);
            let header = disk.read_object(address, &mut block, what)?;
            let is_root = levels.is_empty();
            let storage = if kind.physical { PHYSICAL } else { VIRTUAL };
            ensure!(
                header.kind == if is_root { BTREE } else { BTREE_NODE }
                    && header.subtype == kind.subtype
                    && header.storage == storage
                    && header.oid == oid,
                "APFS {what} node at block {address} is not the node its parent names"
            );
            let parsed =
                node(&block).with_context(|| format!("APFS {what} node at block {address}"))?;
            ensure!(
                parsed.flags & (NODE_HASHED | NODE_NOHEADER) == 0,
                "APFS {what} node at block {address} is hashed or headerless, as only sealed volumes are; {BACKEND_HINT}"
            );
            ensure!(
                (parsed.flags & NODE_ROOT != 0) == is_root
                    && parsed.is_fixed() == kind.fixed.is_some(),
                "APFS {what} node at block {address} has the wrong flags"
            );
            match levels.last() {
                None => {
                    let tree = parsed.info.unwrap();
                    ensure!(
                        tree.node_size as usize == size
                            && tree.flags & (TREE_EPHEMERAL | TREE_HASHED | TREE_NOHEADER) == 0
                            && (tree.flags & TREE_PHYSICAL != 0) == kind.physical
                            && kind
                                .fixed
                                .is_none_or(|sizes| { (tree.key_size, tree.value_size) == sizes })
                            && parsed.level < MAX_TREE_LEVELS,
                        "APFS {what} has tree information Zipper does not read"
                    );
                    ensure!(
                        tree.nodes <= MAX_NODES && tree.keys <= MAX_KEYS,
                        "APFS {what} has more than {MAX_NODES} nodes or {MAX_KEYS} keys"
                    );
                    info = Some(tree);
                }
                Some(parent) => ensure!(
                    parsed.level + 1 == parent.node.level,
                    "APFS {what} node at block {address} is not one level below its parent"
                ),
            }
            nodes += 1;
            ensure!(
                nodes <= info.unwrap().nodes,
                "APFS {what} holds more nodes than its root counts"
            );
            levels.push(Level {
                address,
                block,
                node: parsed,
                next: 0,
            });
        }
        let Some(level) = levels.last_mut() else {
            break;
        };
        if level.next == level.node.keys {
            let done = levels.pop().unwrap();
            spare.push(done.block);
            continue;
        }
        let index = level.next;
        level.next += 1;
        let sizes = kind.fixed.unwrap_or((0, 0));
        let (key, value) = entry(&level.node, &level.block, index, sizes)
            .with_context(|| format!("APFS {what} node at block {}", level.address))?;
        if level.node.is_leaf() {
            keys += 1;
            ensure!(
                keys <= info.unwrap().keys,
                "APFS {what} holds more keys than its root counts"
            );
            visit(
                &level.block[key],
                &level.block[value],
                Location {
                    node: level.address,
                    index: index as u16,
                },
            )?;
            continue;
        }
        ensure!(
            value.len() == 8,
            "APFS {what} index node at block {} has a child value of {} bytes",
            level.address,
            value.len()
        );
        let child = le64(&level.block, value.start);
        pending = Some(match omap {
            None => (child, child),
            Some(omap) => {
                let entry = omap.find(child).with_context(|| {
                    format!("APFS {what} names node {child}, which its object map lacks")
                })?;
                (resolve(disk, &entry, what)?, child)
            }
        });
    }
    let tree = info.unwrap();
    ensure!(
        nodes == tree.nodes && keys == tree.keys,
        "APFS {what} counts {} nodes and {} keys, and holds {nodes} and {keys}",
        tree.nodes,
        tree.keys
    );
    Ok(())
}

fn resolve(disk: &Disk, entry: &MapEntry, what: &str) -> Result<u64> {
    ensure!(
        entry.flags & OMAP_ENCRYPTED == 0,
        "APFS {what} is encrypted, which Zipper does not read; {BACKEND_HINT}"
    );
    ensure!(
        entry.flags & (OMAP_DELETED | OMAP_NOHEADER) == 0,
        "APFS {what} maps object {} to a deleted or headerless object",
        entry.oid
    );
    ensure!(
        entry.size as usize == disk.block_size() && entry.xid <= disk.xid,
        "APFS {what} maps object {} to an object of the wrong size or a later checkpoint",
        entry.oid
    );
    Ok(entry.address)
}

// EXTERNAL(7-Zip ApfsHandler CObjectMap::Parse): every key is 16 bytes, every value 16
// bytes, and object identifiers rise strictly, one version each.
fn load_omap(disk: &mut Disk, address: u64, what: &'static str) -> Result<(Omap, ObjectMap)> {
    let mut block = vec![0u8; disk.block_size()];
    let header = disk.read_object(address, &mut block, what)?;
    ensure!(
        header.oid == address && header.storage == PHYSICAL,
        "APFS {what} at block {address} is not the object its owner names"
    );
    let fields = object_map(block[..OBJECT_MAP_FIELDS].try_into().unwrap())
        .with_context(|| format!("APFS {what} at block {address}"))?;
    let mut entries = Vec::new();
    let kind = TreeKind { what, ..OMAP_TREE };
    walk(
        disk,
        fields.root,
        fields.root,
        kind,
        None,
        |key, value, _| {
            let oid = le64(key, 0);
            let entry = MapEntry {
                oid,
                xid: le64(key, 8),
                flags: le32(value, 0),
                size: le32(value, 4),
                address: le64(value, 8),
            };
            ensure!(
                entries.last().is_none_or(|last: &MapEntry| last.oid < oid),
                "APFS {what} has object identifiers that do not rise"
            );
            entries.push(entry);
            Ok(())
        },
    )?;
    Ok((Omap { entries }, fields))
}

fn container_candidate(block: &[u8], first: &Superblock) -> Option<Superblock> {
    let header = object(block).ok()?;
    let fields = superblock(block[..SUPERBLOCK_FIELDS].try_into().unwrap()).ok()?;
    (header.kind == NX_SUPERBLOCK
        && fields.block_log == first.block_log
        && fields.block_count == first.block_count
        && fields.uuid == first.uuid)
        .then_some(fields)
}

// SPEC(APFS Reference, Mounting an Apple File System Partition): the newest valid
// superblock in the checkpoint descriptor area names the checkpoint to mount.
fn latest_checkpoint(disk: &mut Disk, first: Superblock) -> Result<Superblock> {
    let mut block = vec![0u8; disk.block_size()];
    let mut best = first;
    for index in 0..first.descriptors.blocks {
        disk.read_block(first.descriptors.at(index), &mut block)?;
        if let Some(candidate) = container_candidate(&block, &first)
            && candidate.xid > best.xid
        {
            best = candidate;
        }
    }
    disk.xid = best.xid;
    let maps = best.descriptor_count - 1;
    for step in 0..maps {
        let address = best.descriptors.at(best.first_descriptor + step);
        let header = disk.read_object(address, &mut block, "checkpoint map")?;
        let map = checkpoint_map(&block)
            .with_context(|| format!("APFS checkpoint map at block {address}"))?;
        ensure!(
            header.oid == address && header.xid == best.xid && map.last == (step + 1 == maps),
            "APFS checkpoint map at block {address} does not belong to the newest checkpoint"
        );
        for index in 0..map.count {
            let at = CHECKPOINT_HEADER + index * CHECKPOINT_MAPPING;
            checkpoint_mapping(
                block[at..at + CHECKPOINT_MAPPING].try_into().unwrap(),
                best.data,
                best.block_log,
            )
            .with_context(|| format!("APFS checkpoint map at block {address}"))?;
        }
    }
    Ok(best)
}

struct Container<'v> {
    disk: Disk<'v>,
    superblock: Superblock,
    omap: Omap,
}

fn open_container(volume: &mut dyn Volume) -> Result<Container<'_>> {
    ensure!(
        volume.size() >= HEADER_BLOCK as u64,
        "APFS container is shorter than its superblock"
    );
    let mut start = vec![0u8; HEADER_BLOCK];
    volume.read_exact_at(0, &mut start)?;
    let first = superblock(start[..SUPERBLOCK_FIELDS].try_into().unwrap())?;
    let mut block = vec![0u8; 1 << first.block_log];
    ensure!(
        first.block_count << first.block_log <= volume.size(),
        "APFS container holds more blocks than its image, which 7-Zip reports as an unexpected end"
    );
    volume.read_exact_at(0, &mut block)?;
    object(&block).context("APFS container superblock is damaged")?;
    ensure!(
        first.incompatible & NX_INCOMPAT_FUSION == 0,
        "APFS container is a Fusion container, which Zipper does not read; {BACKEND_HINT}"
    );
    let mut disk = Disk {
        volume,
        block_log: first.block_log,
        block_count: first.block_count,
        xid: first.xid,
    };
    let superblock = latest_checkpoint(&mut disk, first)?;
    ensure!(
        superblock.incompatible & NX_INCOMPAT_FUSION == 0,
        "APFS container is a Fusion container, which Zipper does not read; {BACKEND_HINT}"
    );
    let (omap, _) = load_omap(&mut disk, superblock.omap, "container object map")?;
    Ok(Container {
        disk,
        superblock,
        omap,
    })
}

struct OpenedVolume {
    fields: VolumeSuperblock,
    omap: Omap,
    root: u64,
}

fn open_volume(container: &mut Container, oid: u64, number: usize) -> Result<OpenedVolume> {
    let entry = container.omap.find(oid).with_context(|| {
        format!("APFS volume {number} is missing from the container's object map")
    })?;
    let address = resolve(&container.disk, &entry, "container object map")?;
    let mut block = vec![0u8; container.disk.block_size()];
    let header = container
        .disk
        .read_object(address, &mut block, "volume superblock")?;
    ensure!(
        header.oid == oid && header.storage == VIRTUAL,
        "APFS volume {number} superblock is not the object its container names"
    );
    let fields = volume_superblock(block[..VOLUME_FIELDS].try_into().unwrap())
        .with_context(|| format!("APFS volume {number}"))?;
    ensure!(
        fields.flags & VOLUME_UNENCRYPTED != 0
            && fields.incompatible & VOLUME_ENCRYPTION_ROLLED == 0,
        "APFS volume {number} is encrypted, which Zipper does not read; {BACKEND_HINT}"
    );
    ensure!(
        fields.snapshots == 0,
        "APFS volume {number} holds {} snapshots, and Zipper reads only volumes without them; {BACKEND_HINT}",
        fields.snapshots
    );
    ensure!(
        fields.incompatible & VOLUME_SEALED == 0
            && fields.integrity == 0
            && fields.extent_tree == 0,
        "APFS volume {number} is a sealed volume, which Zipper does not read; {BACKEND_HINT}"
    );
    let (omap, map) = load_omap(&mut container.disk, fields.omap, "volume object map")?;
    ensure!(
        map.snapshots == 0,
        "APFS volume {number} holds snapshots in its object map, and Zipper reads only volumes without them; {BACKEND_HINT}"
    );
    let root = omap.find(fields.root_tree).with_context(|| {
        format!("APFS volume {number} names a file-system tree its object map lacks")
    })?;
    let root = resolve(&container.disk, &root, "volume object map")?;
    Ok(OpenedVolume { fields, omap, root })
}

fn volume_oids(superblock: &Superblock) -> Vec<u64> {
    superblock.volumes[..superblock.max_volumes as usize]
        .iter()
        .copied()
        .filter(|&oid| oid != 0)
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum AttrAt {
    Embedded(Location, u16),
    Stream { id: u64, size: u64 },
}

impl AttrAt {
    fn is_empty(&self) -> bool {
        matches!(
            self,
            AttrAt::Embedded(_, 0) | AttrAt::Stream { size: 0, .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Special {
    decmpfs: Option<AttrAt>,
    resource: Option<AttrAt>,
    symlink: Option<AttrAt>,
}

#[derive(Clone, Copy, Debug)]
struct Inode {
    id: u64,
    private: u64,
    modified: u64,
    flags: u64,
    mode: u16,
    uncompressed: u64,
    stream: Option<DataStream>,
    special: Option<u32>,
    folder_record: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Record {
    parent: u64,
    file: u64,
    name: u32,
    length: u16,
    kind: u16,
}

struct Catalog {
    inodes: Vec<Inode>,
    specials: Vec<Special>,
    attributes: Vec<(u64, Location)>,
    records: Vec<Record>,
    names: Vec<u8>,
    extents: Vec<FileExtent>,
    runs: Vec<(u64, u32)>,
    private_entries: usize,
}

impl Catalog {
    fn inode(&self, id: u64) -> Option<usize> {
        self.inodes.binary_search_by_key(&id, |inode| inode.id).ok()
    }

    fn name(&self, record: &Record) -> &str {
        let start = record.name as usize;
        std::str::from_utf8(&self.names[start..start + usize::from(record.length)]).unwrap()
    }

    fn special(&self, inode: &Inode) -> Special {
        inode
            .special
            .map(|index| self.specials[index as usize])
            .unwrap_or_default()
    }

    fn attributes_of(&self, owner: u64) -> &[(u64, Location)] {
        let start = self.attributes.partition_point(|&(id, _)| id < owner);
        let end = self.attributes.partition_point(|&(id, _)| id <= owner);
        &self.attributes[start..end]
    }

    fn extents_of(&self, owner: u64) -> &[FileExtent] {
        let Ok(index) = self.runs.binary_search_by_key(&owner, |run| run.0) else {
            return &[];
        };
        let start = self.runs[index].1 as usize;
        let end = self
            .runs
            .get(index + 1)
            .map_or(self.extents.len(), |run| run.1 as usize);
        &self.extents[start..end]
    }
}

struct CatalogBuilder {
    hashed: bool,
    catalog: Catalog,
    last_id: u64,
    current_run: Option<(u64, u32)>,
    attribute_owner: u64,
    attribute_names: crate::xattr::EntryNames,
}

impl CatalogBuilder {
    fn last_inode(&mut self, id: u64, what: &str) -> Result<&mut Inode> {
        match self.catalog.inodes.last_mut() {
            Some(inode) if inode.id == id => Ok(inode),
            _ => bail!("APFS {what} record for object {id} does not follow its inode"),
        }
    }

    fn special_of(&mut self, id: u64) -> Result<&mut Special> {
        let index = match self.last_inode(id, "attribute")?.special {
            Some(index) => index,
            None => {
                let index = self.catalog.specials.len() as u32;
                self.catalog.specials.push(Special::default());
                self.last_inode(id, "attribute")?.special = Some(index);
                index
            }
        };
        Ok(&mut self.catalog.specials[index as usize])
    }

    fn close_run(&mut self) {
        if let Some(run) = self.current_run.take() {
            self.catalog.runs.push(run);
        }
    }

    // EXTERNAL(7-Zip ApfsHandler OpenVolume): record identifiers never fall, inodes rise
    // strictly, and attribute and data stream records follow the inode they belong to.
    fn add(&mut self, key: &[u8], value: &[u8], at: Location) -> Result<()> {
        let (id, kind) = record_key(key)?;
        ensure!(
            id >= self.last_id.max(1),
            "APFS file-system records are out of order at object {id}"
        );
        self.last_id = id;
        match kind {
            INODE_RECORD => {
                ensure!(
                    key.len() == 8,
                    "APFS inode {id} has a key of the wrong size"
                );
                let record = inode_record(value).with_context(|| format!("APFS inode {id}"))?;
                ensure!(
                    self.catalog.inodes.last().is_none_or(|last| last.id < id),
                    "APFS inode {id} repeats"
                );
                ensure!(
                    self.catalog.inodes.len() < MAX_ENTRIES,
                    "APFS volume has more than {MAX_ENTRIES} inodes"
                );
                self.catalog.inodes.push(Inode {
                    id,
                    private: record.private,
                    modified: record.modified,
                    flags: record.flags,
                    mode: record.mode,
                    uncompressed: record.uncompressed,
                    stream: record.stream,
                    special: None,
                    folder_record: None,
                });
            }
            XATTR_RECORD => {
                let record =
                    xattr_record(key, value).with_context(|| format!("APFS inode {id}"))?;
                self.last_inode(id, "attribute")?;
                let name = &key[record.name.clone()];
                if self.attribute_owner != id {
                    self.attribute_owner = id;
                    self.attribute_names.clear();
                }
                let shown = self
                    .attribute_names
                    .add(name)
                    .with_context(|| format!("APFS inode {id}"))?;
                if ![DECMPFS, RESOURCE_FORK, SYMLINK_TARGET].contains(&name) {
                    if !crate::xattr::is_restored(shown) {
                        return Ok(());
                    }
                    match record.data {
                        XattrData::Embedded(_) => {}
                        XattrData::Stream { size, .. } => crate::xattr::check_size(shown, size)
                            .with_context(|| format!("APFS inode {id}"))?,
                        XattrData::Other => bail!(
                            "APFS inode {id} has a {shown} attribute with neither embedded nor stream data"
                        ),
                    }
                    ensure!(
                        self.catalog.attributes.len() < MAX_ATTRIBUTES,
                        "APFS volume has more than {MAX_ATTRIBUTES} extended attributes"
                    );
                    self.catalog.attributes.push((id, at));
                    return Ok(());
                }
                let name = name.to_vec();
                let at = match record.data {
                    XattrData::Embedded(range) => AttrAt::Embedded(at, range.len() as u16),
                    XattrData::Stream { id, size } => AttrAt::Stream { id, size },
                    XattrData::Other => bail!(
                        "APFS inode {id} has a {} attribute with neither embedded nor stream data",
                        String::from_utf8_lossy(&name)
                    ),
                };
                let special = self.special_of(id)?;
                let slot = if name == DECMPFS {
                    &mut special.decmpfs
                } else if name == RESOURCE_FORK {
                    &mut special.resource
                } else {
                    &mut special.symlink
                };
                *slot = Some(at);
            }
            DSTREAM_ID_RECORD => {
                ensure!(
                    key.len() == 8 && value.len() == 4,
                    "APFS data stream record {id} has the wrong size"
                );
                self.last_inode(id, "data stream")?;
            }
            FILE_EXTENT_RECORD => {
                let extent =
                    file_extent_record(key, value).with_context(|| format!("APFS object {id}"))?;
                ensure!(
                    self.catalog.extents.len() < MAX_EXTENTS,
                    "APFS volume has more than {MAX_EXTENTS} file extents"
                );
                let index = self.catalog.extents.len() as u32;
                if self.current_run.is_none_or(|(owner, _)| owner != id) {
                    self.close_run();
                    self.current_run = Some((id, index));
                }
                self.catalog.extents.push(extent);
            }
            DIRECTORY_RECORD => {
                let record = directory_record(key, value, self.hashed)
                    .with_context(|| format!("APFS folder {id}"))?;
                ensure!(
                    self.catalog.records.len() < MAX_ENTRIES,
                    "APFS volume has more than {MAX_ENTRIES} directory records"
                );
                let name = &key[record.name.clone()];
                let start = self.catalog.names.len();
                ensure!(start + name.len() <= MAX_NAMES, "APFS names exceed 64 MiB");
                std::str::from_utf8(name).with_context(|| {
                    format!("APFS folder {id} holds a name that is not UTF-8; {BACKEND_HINT}")
                })?;
                self.catalog.names.extend_from_slice(name);
                if id == PRIVATE_FOLDER {
                    self.catalog.private_entries += 1;
                }
                self.catalog.records.push(Record {
                    parent: id,
                    file: record.file,
                    name: start as u32,
                    length: name.len() as u16,
                    kind: record.kind,
                });
            }
            SIBLING_LINK_RECORD => ensure!(
                key.len() == 16 && value.len() > 10,
                "APFS sibling link of inode {id} has the wrong size"
            ),
            SIBLING_MAP_RECORD => ensure!(
                key.len() == 8 && value.len() == 8,
                "APFS sibling map {id} has the wrong size"
            ),
            FILE_INFO_RECORD => {
                bail!("APFS volume holds file hashes, as only sealed volumes do; {BACKEND_HINT}")
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self) -> Catalog {
        self.close_run();
        self.catalog
    }
}

fn load_catalog(container: &mut Container, opened: &OpenedVolume) -> Result<Catalog> {
    let hashed = opened.fields.incompatible
        & (VOLUME_CASE_INSENSITIVE | VOLUME_NORMALIZATION_INSENSITIVE)
        != 0;
    let mut builder = CatalogBuilder {
        hashed,
        catalog: Catalog {
            inodes: Vec::new(),
            specials: Vec::new(),
            attributes: Vec::new(),
            records: Vec::new(),
            names: Vec::new(),
            extents: Vec::new(),
            runs: Vec::new(),
            private_entries: 0,
        },
        last_id: 0,
        current_run: None,
        attribute_owner: 0,
        attribute_names: crate::xattr::EntryNames::default(),
    };
    walk(
        &mut container.disk,
        opened.root,
        opened.fields.root_tree,
        FILE_TREE,
        Some(&opened.omap),
        |key, value, at| builder.add(key, value, at),
    )?;
    let mut catalog = builder.finish();
    link_folders(&mut catalog)?;
    Ok(catalog)
}

// EXTERNAL(7-Zip ApfsHandler CVol::FillRefs): a headers error when a record names a missing
// inode or one of another type, a folder has two records, or an inode has none.
fn link_folders(catalog: &mut Catalog) -> Result<()> {
    let mut linked = vec![false; catalog.inodes.len()];
    for (position, record) in catalog.records.iter().enumerate() {
        if record.file == ROOT_FOLDER
            || record.file == PRIVATE_FOLDER && catalog.private_entries == 0
        {
            continue;
        }
        let name = {
            let start = record.name as usize;
            String::from_utf8_lossy(&catalog.names[start..start + usize::from(record.length)])
                .into_owned()
        };
        let index = catalog
            .inodes
            .binary_search_by_key(&record.file, |inode| inode.id)
            .map_err(|_| {
                anyhow::anyhow!(
                    "APFS entry {name:?} names inode {}, which the volume lacks",
                    record.file
                )
            })?;
        let inode = &mut catalog.inodes[index];
        ensure!(
            record.kind == UNKNOWN_KIND || inode.mode >> KIND_SHIFT == record.kind,
            "APFS entry {name:?} has a type that disagrees with its inode"
        );
        if inode.mode & TYPE_BITS == DIRECTORY {
            ensure!(
                inode.folder_record.is_none(),
                "APFS folder {} has two names, a hard link to a folder",
                inode.id
            );
            inode.folder_record = Some(position as u32);
        }
        linked[index] = true;
    }
    for (inode, linked) in catalog.inodes.iter().zip(linked) {
        ensure!(
            linked || inode.id == ROOT_FOLDER || inode.id == PRIVATE_FOLDER,
            "APFS inode {} has no directory record",
            inode.id
        );
    }
    Ok(())
}

fn validate_extents(extents: &[FileExtent], size: u64, disk: &Disk, name: &str) -> Result<()> {
    let block_size = disk.block_size() as u64;
    let mut covered = 0u64;
    for extent in extents {
        ensure!(
            covered < size,
            "APFS file {name:?} has an extent past its size"
        );
        ensure!(
            extent.logical == covered,
            "APFS file {name:?} has extents that do not follow one another"
        );
        ensure!(
            extent.length != 0 && extent.length.is_multiple_of(block_size),
            "APFS file {name:?} has an extent that is not a whole number of blocks"
        );
        let blocks = extent.length >> disk.block_log;
        ensure!(
            extent.block == 0
                || extent
                    .block
                    .checked_add(blocks)
                    .is_some_and(|end| end <= disk.block_count),
            "APFS file {name:?} has an extent past the end of the container"
        );
        covered = covered
            .checked_add(extent.length)
            .with_context(|| format!("APFS file {name:?} has extents that overflow"))?;
    }
    ensure!(
        covered >= size,
        "APFS file {name:?} holds {covered} bytes in its extents for {size} bytes"
    );
    Ok(())
}

fn read_extents(
    disk: &mut Disk,
    extents: &[FileExtent],
    offset: u64,
    buffer: &mut [u8],
) -> Result<()> {
    let mut offset = offset;
    let mut buffer = buffer;
    while !buffer.is_empty() {
        let index = extents.partition_point(|extent| extent.logical <= offset) - 1;
        let extent = extents[index];
        let inside = offset - extent.logical;
        let take = (extent.length - inside).min(buffer.len() as u64) as usize;
        let (now, rest) = buffer.split_at_mut(take);
        if extent.block == 0 {
            now.fill(0);
        } else {
            disk.read_at((extent.block << disk.block_log) + inside, now)?;
        }
        buffer = rest;
        offset += take as u64;
    }
    Ok(())
}

struct ExtentStream<'d, 'v, 'e> {
    disk: &'d mut Disk<'v>,
    extents: &'e [FileExtent],
    size: u64,
    position: u64,
}

impl Read for ExtentStream<'_, '_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let take = ((self.size - self.position).min(buffer.len() as u64) as usize).min(COPY_PIECE);
        if take == 0 {
            return Ok(0);
        }
        read_extents(self.disk, self.extents, self.position, &mut buffer[..take])
            .map_err(std::io::Error::other)?;
        self.position += take as u64;
        Ok(take)
    }
}

enum Fork {
    Bytes(Vec<u8>),
    Extents(Vec<FileExtent>, u64),
}

impl Fork {
    fn size(&self) -> u64 {
        match self {
            Fork::Bytes(bytes) => bytes.len() as u64,
            Fork::Extents(_, size) => *size,
        }
    }

    fn read_at(&self, disk: &mut Disk, at: u64, length: usize) -> Result<Vec<u8>> {
        ensure!(
            at.checked_add(length as u64)
                .is_some_and(|end| end <= self.size()),
            "APFS compressed file's table reaches past its resource fork"
        );
        match self {
            Fork::Bytes(bytes) => Ok(bytes[at as usize..at as usize + length].to_vec()),
            Fork::Extents(extents, _) => {
                let mut bytes = vec![0; length];
                read_extents(disk, extents, at, &mut bytes)?;
                Ok(bytes)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkTable {
    Zlib,
    Lzvn,
}

struct Chunks<'d, 'v> {
    disk: &'d mut Disk<'v>,
    fork: Fork,
    table: ChunkTable,
    size: u64,
    chunks: u64,
    chunk: u64,
    next_offset: u64,
    decoded: Vec<u8>,
    held: Range<usize>,
}

impl<'d, 'v> Chunks<'d, 'v> {
    fn open(disk: &'d mut Disk<'v>, fork: Fork, table: ChunkTable, size: u64) -> Result<Self> {
        let chunks = size.div_ceil(COMPRESSED_CHUNK as u64);
        ensure!(
            chunks < MAX_CHUNKS,
            "APFS compressed file is larger than 7-Zip reads"
        );
        let mut stream = Chunks {
            disk,
            fork,
            table,
            size,
            chunks,
            chunk: 0,
            next_offset: 0,
            decoded: vec![0; COMPRESSED_CHUNK],
            held: 0..0,
        };
        stream.next_offset = match table {
            ChunkTable::Lzvn => stream.lzvn_table_start()?,
            ChunkTable::Zlib => stream.zlib_table_start()?,
        };
        if chunks == 0 {
            stream.finish()?;
        }
        Ok(stream)
    }

    fn fork_bytes(&mut self, at: u64, length: usize) -> Result<Vec<u8>> {
        self.fork.read_at(self.disk, at, length)
    }

    // EXTERNAL(7-Zip HfsHandler ExtractResourceFork_LZFSE): a table of chunk count + 1 offsets
    // that starts at its own size, rises strictly and ends at the resource fork's size.
    fn lzvn_table_start(&mut self) -> Result<u64> {
        let table = (self.chunks + 1) * 4;
        ensure!(
            table <= self.fork.size(),
            "APFS compressed file's chunk table is larger than its resource fork"
        );
        let first = le32(&self.fork_bytes(0, 4)?, 0);
        ensure!(
            u64::from(first) == table,
            "APFS compressed file's chunk table does not start at its end"
        );
        Ok(table)
    }

    // EXTERNAL(7-Zip HfsHandler ExtractResourceFork_ZLIB): a resource fork with one 'cmpf'
    // resource, whose data starts with the chunk count and a table of offsets and sizes.
    fn zlib_table_start(&mut self) -> Result<u64> {
        let size = self.fork.size();
        let head = self.fork_bytes(0, ZLIB_FORK_HEADER + 8)?;
        let be32 =
            |bytes: &[u8], at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        let be16 = |bytes: &[u8], at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let (data_at, map_at) = (be32(&head, 0), be32(&head, 4));
        let (data_size, map_size) = (be32(&head, 8), be32(&head, 12));
        ensure!(
            map_size == RESOURCE_MAP
                && data_at == ZLIB_FORK_HEADER as u32
                && data_at <= map_at
                && data_size == map_at - data_at
                && u64::from(map_at) + u64::from(map_size) == size,
            "APFS compressed file's resource fork has a bad header"
        );
        let resource_size = be32(&head, ZLIB_FORK_HEADER);
        let chunks = le32(&head, ZLIB_FORK_HEADER + 4);
        ensure!(
            4 + u64::from(resource_size) == u64::from(data_size)
                && resource_size >= 8
                && u64::from(chunks) <= u64::from((resource_size - 4) >> 3)
                && u64::from(chunks) == self.chunks,
            "APFS compressed file's resource holds the wrong number of chunks"
        );
        let map = self.fork_bytes(u64::from(map_at), RESOURCE_MAP as usize)?;
        let map_ok = be16(&map, 24) == 28
            && be16(&map, 26) == RESOURCE_MAP as u16
            && be16(&map, 28) == 0
            && &map[30..34] == b"cmpf"
            && be16(&map, 34) == 0
            && be16(&map, 36) == 10
            && be16(&map, 38) == 1
            && be16(&map, 40) == 0xFFFF
            && be32(&map, 42) & 0xFF_FFFF == 0;
        ensure!(
            map_ok,
            "APFS compressed file's resource map is not the one 7-Zip reads"
        );
        Ok(4 + self.chunks * 8)
    }

    fn chunk_span(&mut self) -> Result<(u64, usize)> {
        let index = self.chunk;
        let (start, length) = match self.table {
            ChunkTable::Lzvn => {
                let pair = self.fork_bytes(index * 4, 8)?;
                let (start, end) = (le32(&pair, 0), le32(&pair, 4));
                ensure!(
                    u64::from(start) == self.next_offset && end > start,
                    "APFS compressed file's chunk table does not rise"
                );
                (u64::from(start), u64::from(end - start))
            }
            ChunkTable::Zlib => {
                let at = (ZLIB_FORK_HEADER + 8) as u64 + index * 8;
                let pair = self.fork_bytes(at, 8)?;
                let (start, length) = (le32(&pair, 0), le32(&pair, 4));
                ensure!(
                    u64::from(start) == self.next_offset && length != 0,
                    "APFS compressed file's chunks do not follow one another"
                );
                (
                    (ZLIB_FORK_HEADER + 4) as u64 + u64::from(start),
                    u64::from(length),
                )
            }
        };
        ensure!(
            length <= LARGEST_COMPRESSED_CHUNK as u64,
            "APFS compressed chunk is larger than 64 KiB and one byte"
        );
        self.next_offset = match self.table {
            ChunkTable::Lzvn => start + length,
            ChunkTable::Zlib => start - (ZLIB_FORK_HEADER + 4) as u64 + length,
        };
        Ok((start, length as usize))
    }

    fn finish(&mut self) -> Result<()> {
        let end = match self.table {
            ChunkTable::Lzvn => self.fork.size(),
            ChunkTable::Zlib => {
                let bytes = self.fork_bytes(ZLIB_FORK_HEADER as u64, 4)?;
                u64::from(u32::from_be_bytes(bytes[..4].try_into().unwrap()))
            }
        };
        ensure!(
            self.next_offset == end,
            "APFS compressed file's chunks do not end where its table says"
        );
        Ok(())
    }

    fn decode_next(&mut self) -> Result<()> {
        let (start, length) = self.chunk_span()?;
        let packed = self.fork_bytes(start, length)?;
        let out = (self.size - self.chunk * COMPRESSED_CHUNK as u64).min(COMPRESSED_CHUNK as u64)
            as usize;
        let stored = match self.table {
            ChunkTable::Lzvn => packed[0] == LZVN_STORED_MARKER,
            ChunkTable::Zlib => packed[0] & 0xF == ZLIB_STORED_MARKER,
        };
        if stored {
            ensure!(
                length - 1 == out,
                "APFS stored chunk is not the size of its block"
            );
            self.decoded[..out].copy_from_slice(&packed[1..]);
        } else {
            match self.table {
                ChunkTable::Lzvn => {
                    crate::lzvn::decode(
                        &packed,
                        &mut self.decoded[..out],
                        0,
                        crate::lzvn::Tail::Ignored,
                    )?;
                }
                ChunkTable::Zlib => {
                    hfs::inflate_zlib(&packed, &mut self.decoded[..out], hfs::ZlibEnd::Padded)?
                }
            }
        }
        self.chunk += 1;
        if self.chunk == self.chunks {
            self.finish()?;
        }
        self.held = 0..out;
        Ok(())
    }
}

impl Read for Chunks<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.held.is_empty() {
            if self.chunk == self.chunks {
                return Ok(0);
            }
            self.decode_next().map_err(std::io::Error::other)?;
        }
        let take = self.held.len().min(buffer.len());
        buffer[..take].copy_from_slice(&self.decoded[self.held.start..self.held.start + take]);
        self.held.start += take;
        Ok(take)
    }
}

fn inline_data(attribute: &[u8], header: &hfs::Decmpfs) -> Result<Vec<u8>> {
    let payload = &attribute[hfs::DECMPFS_HEADER..];
    let bound = |ratio: u64| {
        ensure!(
            header.size <= payload.len() as u64 * ratio,
            "APFS compressed file declares more bytes than its data can hold"
        );
        Ok(vec![0u8; header.size as usize])
    };
    match header.stored {
        hfs::Stored::Inline => Ok(payload[1..].to_vec()),
        hfs::Stored::ZlibInline => {
            let mut output = bound(DEFLATE_EXPANSION)?;
            hfs::inflate_zlib(payload, &mut output, hfs::ZlibEnd::Exact)?;
            Ok(output)
        }
        hfs::Stored::LzvnInline => {
            let mut output = bound(LZVN_EXPANSION)?;
            crate::lzvn::decode(payload, &mut output, 0, crate::lzvn::Tail::Ignored)?;
            Ok(output)
        }
        _ => unreachable!(),
    }
}

struct Reading<'c, 'd, 'v> {
    disk: &'d mut Disk<'v>,
    catalog: &'c Catalog,
}

impl Reading<'_, '_, '_> {
    fn restore_attributes(
        &mut self,
        owner: u64,
        path: &str,
        tree: &mut tree::Tree,
        mut target: Option<tree::Owner>,
    ) -> Result<()> {
        let writes = target.is_some() && tree.writes_attributes();
        for &(_, at) in self.catalog.attributes_of(owner) {
            tree.check()?;
            let mut block = vec![0u8; self.disk.block_size()];
            self.disk
                .read_object(at.node, &mut block, "file-system tree")?;
            let parsed = node(&block)?;
            let (key, value) = entry(&parsed, &block, usize::from(at.index), (0, 0))?;
            let (key, value) = (&block[key], &block[value]);
            let record = xattr_record(key, value)?;
            ensure!(
                record_key(key)? == (owner, XATTR_RECORD),
                "APFS file-system tree changed while it was read"
            );
            let name = crate::xattr::checked_name(&key[record.name])?;
            let bytes = match record.data {
                XattrData::Embedded(range) => value[range].to_vec(),
                XattrData::Stream { id, size } => {
                    let extents = self.catalog.extents_of(id);
                    validate_extents(extents, size, self.disk, path)?;
                    if !writes {
                        continue;
                    }
                    let stream = ExtentStream {
                        disk: &mut *self.disk,
                        extents,
                        size,
                        position: 0,
                    };
                    crate::xattr::read_value(stream, size, name)
                        .with_context(|| format!("APFS file {path:?}"))?
                }
                XattrData::Other => bail!("APFS file-system tree changed while it was read"),
            };
            if let Some(target) = &mut target {
                tree.set_attribute(target, name, bytes)?;
            }
        }
        Ok(())
    }

    fn embedded(&mut self, at: Location, owner: u64, name: &[u8]) -> Result<Vec<u8>> {
        let mut block = vec![0u8; self.disk.block_size()];
        self.disk
            .read_object(at.node, &mut block, "file-system tree")?;
        let parsed = node(&block)?;
        let (key, value) = entry(&parsed, &block, usize::from(at.index), (0, 0))?;
        let (key, value) = (&block[key], &block[value]);
        let record = xattr_record(key, value)?;
        ensure!(
            record_key(key)? == (owner, XATTR_RECORD) && &key[record.name.clone()] == name,
            "APFS file-system tree changed while it was read"
        );
        let XattrData::Embedded(range) = record.data else {
            bail!("APFS file-system tree changed while it was read");
        };
        Ok(value[range].to_vec())
    }

    fn attribute(
        &mut self,
        at: AttrAt,
        owner: u64,
        name: &[u8],
        limit: u64,
        path: &str,
    ) -> Result<Vec<u8>> {
        match at {
            AttrAt::Embedded(location, _) => self.embedded(location, owner, name),
            AttrAt::Stream { id, size } => {
                ensure!(
                    size <= limit,
                    "APFS file {path:?} has a {} attribute of {size} bytes",
                    String::from_utf8_lossy(name)
                );
                let extents = self.catalog.extents_of(id);
                validate_extents(extents, size, self.disk, path)?;
                let mut bytes = vec![0u8; size as usize];
                read_extents(self.disk, extents, 0, &mut bytes)?;
                Ok(bytes)
            }
        }
    }

    fn fork(&mut self, at: AttrAt, owner: u64, path: &str) -> Result<Fork> {
        match at {
            AttrAt::Embedded(location, _) => Ok(Fork::Bytes(self.embedded(
                location,
                owner,
                RESOURCE_FORK,
            )?)),
            AttrAt::Stream { id, size } => {
                let extents = self.catalog.extents_of(id);
                validate_extents(extents, size, self.disk, path)?;
                Ok(Fork::Extents(extents.to_vec(), size))
            }
        }
    }

    fn link_target(&mut self, inode: &Inode, path: &str) -> Result<String> {
        let at = self
            .catalog
            .special(inode)
            .symlink
            .with_context(|| format!("APFS link {path:?} has no target"))?;
        let bytes = self.attribute(at, inode.id, SYMLINK_TARGET, crate::MAX_LINK_TARGET, path)?;
        let target = terminated_name(&bytes).with_context(|| {
            format!("APFS link {path:?} has a target that does not end at its only NUL")
        })?;
        let target = std::str::from_utf8(&bytes[target])
            .with_context(|| format!("APFS link {path:?} has a target that is not UTF-8"))?;
        ensure!(
            target.len() as u64 <= crate::MAX_LINK_TARGET,
            "APFS link {path:?} has a target longer than {} bytes",
            crate::MAX_LINK_TARGET
        );
        Ok(target.to_owned())
    }

    // EXTERNAL(7-Zip ApfsHandler Extract): only a file with no data stream or extents, the
    // uncompressed-size flag and a decmpfs size equal to its inode's is decompressed.
    fn content<'r>(&'r mut self, inode: &Inode, path: &str) -> Result<(u64, Box<dyn Read + 'r>)> {
        let catalog = self.catalog;
        let special = catalog.special(inode);
        let extents = catalog.extents_of(inode.private);
        if let Some(at) = special.decmpfs {
            ensure!(
                inode.stream.is_none()
                    && extents.is_empty()
                    && inode.flags & HAS_UNCOMPRESSED_SIZE != 0,
                "APFS file {path:?} has a decmpfs attribute beside its data, which 7-Zip writes as {path:?}:com.apple.decmpfs; {BACKEND_HINT}"
            );
            let attribute = self.attribute(at, inode.id, DECMPFS, LARGEST_DECMPFS_STREAM, path)?;
            let header = hfs::decmpfs(&attribute)
                .with_context(|| format!("APFS compressed file {path:?}"))?;
            ensure!(
                header.size == inode.uncompressed,
                "APFS compressed file {path:?} declares {} bytes and its inode {}; {BACKEND_HINT}",
                header.size,
                inode.uncompressed
            );
            let table = match header.stored {
                hfs::Stored::ZlibFork => ChunkTable::Zlib,
                hfs::Stored::LzvnFork => ChunkTable::Lzvn,
                hfs::Stored::Unsupported(method) => bail!(
                    "APFS file {path:?} is compressed with method {method}, which 7-Zip does not decode; {BACKEND_HINT}"
                ),
                _ => {
                    ensure!(
                        special.resource.is_none_or(|fork| fork.is_empty()),
                        "APFS file {path:?} has a resource fork, which 7-Zip writes as {path:?}:com.apple.ResourceFork and Zipper does not write; {BACKEND_HINT}"
                    );
                    let data = inline_data(&attribute, &header)
                        .with_context(|| format!("APFS compressed file {path:?}"))?;
                    return Ok((header.size, Box::new(std::io::Cursor::new(data))));
                }
            };
            let resource = special
                .resource
                .with_context(|| format!("APFS compressed file {path:?} has no resource fork"))?;
            let fork = self.fork(resource, inode.id, path)?;
            let stream = Chunks::open(&mut *self.disk, fork, table, header.size)
                .with_context(|| format!("APFS compressed file {path:?}"))?;
            return Ok((header.size, Box::new(stream)));
        }
        ensure!(
            special.resource.is_none_or(|fork| fork.is_empty()),
            "APFS file {path:?} has a resource fork, which 7-Zip writes as {path:?}:com.apple.ResourceFork and Zipper does not write; {BACKEND_HINT}"
        );
        let size = inode.stream.map_or(0, |stream| stream.size);
        validate_extents(extents, size, self.disk, path)?;
        Ok((
            size,
            Box::new(ExtentStream {
                disk: &mut *self.disk,
                extents,
                size,
                position: 0,
            }),
        ))
    }
}

// EXTERNAL(7-Zip Windows/FileDir SetFileAttrib_PosixHighDetect): only a regular file's or
// folder's own mode is applied, and a folder keeps its owner's read, write and search rights.
fn file_mode(mode: u16) -> u32 {
    if mode & TYPE_BITS == REGULAR {
        u32::from(mode) & 0o777
    } else {
        DEFAULT_FILE_MODE
    }
}

fn folder_mode(mode: u16) -> u32 {
    if mode & TYPE_BITS == DIRECTORY {
        (u32::from(mode) | 0o700) & 0o777
    } else {
        DEFAULT_FOLDER_MODE
    }
}

fn apfs_time(nanoseconds: u64) -> Option<SystemTime> {
    if nanoseconds == 0 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanoseconds))
}

fn shown_name(name: &str) -> String {
    name.replace('/', "_")
}

struct Writer<'o> {
    prefix: String,
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    folders: HashMap<u64, (String, usize)>,
    folder_bytes: usize,
    leaders: HashMap<u64, String>,
    names: HashSet<u64>,
    name_keys: std::hash::RandomState,
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }

    fn join(&self, parent: Option<&str>, name: &str) -> String {
        match parent {
            Some(parent) => format!("{parent}/{name}"),
            None if self.prefix.is_empty() => name.to_owned(),
            None => format!("{}/{name}", self.prefix),
        }
    }

    // EXTERNAL(7-Zip ApfsHandler CDatabase::GetItemPath): a path joins the names of the folder
    // records above an entry up to the root folder, whose own record is not shown.
    fn folder_path(&mut self, catalog: &Catalog, id: u64) -> Result<Option<String>> {
        if id == ROOT_FOLDER {
            return Ok(None);
        }
        let mut chain = Vec::new();
        let mut at = id;
        while at != ROOT_FOLDER && !self.folders.contains_key(&at) {
            ensure!(
                chain.len() <= MAX_DEPTH,
                "APFS folders nest deeper than {MAX_DEPTH} or form a loop"
            );
            let index = catalog.inode(at).with_context(|| {
                format!("APFS entry lies in folder {at}, which the volume lacks")
            })?;
            let inode = &catalog.inodes[index];
            ensure!(
                inode.mode & TYPE_BITS == DIRECTORY,
                "APFS entry lies inside inode {at}, which is not a folder"
            );
            let record = inode
                .folder_record
                .with_context(|| format!("APFS folder {at} has no name"))?;
            let record = catalog.records[record as usize];
            chain.push(record);
            if record.parent == ROOT_PARENT {
                ensure!(
                    record.file == PRIVATE_FOLDER,
                    "APFS folder {at} lies outside the root folder"
                );
                break;
            }
            at = record.parent;
        }
        let (mut path, mut depth) = match self.folders.get(&at) {
            Some((path, depth)) => (Some(path.clone()), *depth),
            None => (None, 0),
        };
        for record in chain.iter().rev() {
            let name = if record.parent == ROOT_PARENT {
                PRIVATE_FOLDER_NAME.to_owned()
            } else {
                shown_name(catalog.name(record))
            };
            let joined = self.join(path.as_deref(), &name);
            depth += 1;
            ensure!(
                depth <= MAX_DEPTH,
                "APFS folders nest deeper than {MAX_DEPTH} or form a loop"
            );
            self.folder_bytes += joined.len();
            ensure!(
                self.folder_bytes <= MAX_NAMES,
                "APFS folder paths exceed 64 MiB"
            );
            self.folders.insert(record.file, (joined.clone(), depth));
            path = Some(joined);
        }
        Ok(path)
    }

    fn path(&mut self, catalog: &Catalog, record: &Record) -> Result<String> {
        let name = catalog.name(record);
        ensure!(
            self.names.insert(std::hash::BuildHasher::hash_one(
                &self.name_keys,
                (record.parent, name)
            )),
            "APFS folder {} holds two entries named {name:?}",
            record.parent
        );
        if record.parent == ROOT_PARENT {
            ensure!(
                record.file == PRIVATE_FOLDER,
                "APFS entry {name:?} lies outside the root folder"
            );
            return Ok(self.join(None, PRIVATE_FOLDER_NAME));
        }
        let parent = self.folder_path(catalog, record.parent)?;
        Ok(self.join(parent.as_deref(), &shown_name(name)))
    }
}

fn emit(
    disk: &mut Disk,
    catalog: &Catalog,
    prefix: String,
    tree: &mut tree::Tree,
    options: ReadOptions,
) -> Result<()> {
    let mut writer = Writer {
        prefix,
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        folders: HashMap::new(),
        folder_bytes: 0,
        leaders: HashMap::new(),
        names: HashSet::new(),
        name_keys: std::hash::RandomState::new(),
    };
    if !writer.prefix.is_empty() {
        tree.directory(&writer.prefix, DEFAULT_FOLDER_MODE, None)?;
    }
    let mut reading = Reading { disk, catalog };
    for record in &catalog.records {
        tree.check()?;
        if record.file == ROOT_FOLDER
            || record.file == PRIVATE_FOLDER && catalog.private_entries == 0
        {
            continue;
        }
        let path = writer.path(catalog, record)?;
        let inode = &catalog.inodes[catalog.inode(record.file).unwrap()];
        match inode.mode & TYPE_BITS {
            DIRECTORY => {
                let placed = tree.placed_directory(
                    &path,
                    folder_mode(inode.mode),
                    apfs_time(inode.modified),
                )?;
                let target = placed.as_deref().map(tree::Owner::Folder);
                reading.restore_attributes(inode.id, &path, tree, target)?;
                continue;
            }
            SYMLINK => {
                let target = reading.link_target(inode, &path)?;
                tree.check()?;
                let link = tree.placed_symlink(&path, target)?;
                reading.restore_attributes(inode.id, &path, tree, link.map(tree::Owner::Link))?;
                continue;
            }
            _ => {}
        }
        if let Some(leader) = writer.leaders.get(&inode.id)
            && writer.writes(&path)
        {
            let leader = leader.clone();
            tree.hard_link(&path, &leader)?;
            continue;
        }
        let (size, mut data) = reading.content(inode, &path)?;
        let mut written = tree.write_file(
            &path,
            size,
            file_mode(inode.mode),
            apfs_time(inode.modified),
            &mut data,
        )?;
        let mut rest = [0u8; 1];
        ensure!(
            data.read(&mut rest)? == 0,
            "APFS file {path:?} decodes to more than its declared size"
        );
        drop(data);
        let target = written.as_mut().map(tree::Owner::File);
        reading.restore_attributes(inode.id, &path, tree, target)?;
        tree.finish_file(written)?;
        if writer.writes(&path) {
            writer.leaders.insert(inode.id, path);
        }
    }
    Ok(())
}

pub(crate) fn plan(volume: &mut dyn Volume) -> Result<()> {
    let mut container = open_container(volume)?;
    for (index, oid) in volume_oids(&container.superblock).into_iter().enumerate() {
        open_volume(&mut container, oid, index + 1)?;
    }
    Ok(())
}

pub(crate) fn read(
    volume: &mut dyn Volume,
    tree: &mut tree::Tree,
    options: ReadOptions,
) -> Result<()> {
    let mut container = open_container(volume)?;
    let oids = volume_oids(&container.superblock);
    let mut opened = Vec::with_capacity(oids.len());
    for (index, &oid) in oids.iter().enumerate() {
        opened.push(open_volume(&mut container, oid, index + 1)?);
    }
    let several = opened.len() > 1;
    for (index, volume) in opened.iter().enumerate() {
        tree.check()?;
        let catalog = load_catalog(&mut container, volume)?;
        let prefix = if several {
            format!("Volume{}", index + 1)
        } else {
            String::new()
        };
        emit(&mut container.disk, &catalog, prefix, tree, options)?;
    }
    Ok(())
}

pub(crate) fn is_container(start: &[u8]) -> bool {
    start.len() >= HEADER_BLOCK
        && superblock(start[..SUPERBLOCK_FIELDS].try_into().unwrap()).is_ok()
        && object(&start[..HEADER_BLOCK]).is_ok()
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
            "read past the end of the APFS image"
        );
        let mut buffer = buffer;
        let mut offset = offset;
        while !buffer.is_empty() {
            let read = self
                .file
                .read_at(buffer, offset)
                .context("cannot read the APFS image")?;
            ensure!(read != 0, "truncated APFS image");
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
            "read past the end of the APFS partition"
        );
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner
            .read_exact(buffer)
            .context("cannot read the APFS partition")
    }
}

/// Returns whether the file is a raw APFS container whose first 4 KiB hold
/// a superblock that passes 7-Zip's checks and has a contiguous checkpoint
/// area inside the container.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut start = vec![0u8; HEADER_BLOCK];
    if file.len()? < HEADER_BLOCK as u64 {
        return Ok(false);
    }
    let mut volume = FileVolume {
        file: &file,
        length: HEADER_BLOCK as u64,
    };
    Ok(volume.read_exact_at(0, &mut start).is_ok() && is_container(&start))
}

/// Returns whether the default engine sends this raw container to 7-Zip:
/// one with an encrypted, sealed or snapshot volume, or a Fusion container.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    let result = plan(&mut FileVolume {
        file: &file,
        length,
    });
    Ok(result.is_err_and(|error| format!("{error:#}").contains(BACKEND_HINT)))
}

pub(crate) fn needs_backend_in<V: Read + Seek>(mut volume: V) -> Result<bool> {
    let length = volume.seek(SeekFrom::End(0))?;
    let result = plan(&mut SeekVolume {
        inner: volume,
        length,
    });
    Ok(result.is_err_and(|error| format!("{error:#}").contains(BACKEND_HINT)))
}

/// Verify and optionally extract an APFS container held in a bounded view,
/// such as one GPT partition, into a new directory, in the tree `7zz x`
/// writes: one volume's files at the top, or each of several volumes in a
/// folder named `Volume1`, `Volume2` and on. Encrypted, sealed and snapshot
/// volumes and Fusion containers are refused with a hint to use 7-Zip, and
/// every check of `docs/reference/apfs-reader.md` holds before anything is
/// published.
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
    tree::extract("APFS", archive, destination, options, |tree| {
        read(&mut volume, tree, options)
    })
}

/// Verify and optionally extract a raw APFS container image into a new
/// directory, as [`read_volume`] describes.
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
    tree::extract("APFS", path, destination, options, |tree| {
        read(&mut volume, tree, options)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn reference_fletcher(words: &[u8]) -> u64 {
        let (mut low, mut high) = (0u64, 0u64);
        for word in words.chunks_exact(4) {
            low =
                (low + u64::from(u32::from_le_bytes(word.try_into().unwrap()))) % FLETCHER_MODULUS;
            high = (high + low) % FLETCHER_MODULUS;
        }
        let check_low = FLETCHER_MODULUS - (low + high) % FLETCHER_MODULUS;
        let check_high = FLETCHER_MODULUS - (low + check_low) % FLETCHER_MODULUS;
        check_high << 32 | check_low
    }

    #[test]
    fn fletcher64_matches_a_word_at_a_time_reference_past_every_run_edge() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut bytes = vec![0u8; 4 * (3 * FLETCHER_RUN + 7)];
        for (index, byte) in bytes.iter_mut().enumerate() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = if index % 5 == 0 { 0xFF } else { state as u8 };
        }
        for words in (0..=bytes.len() / 4).filter(|words| {
            words % FLETCHER_RUN <= 2 || words % FLETCHER_RUN >= FLETCHER_RUN - 2 || words % 97 == 0
        }) {
            let slice = &bytes[..words * 4];
            assert_eq!(
                fletcher64(slice),
                reference_fletcher(slice),
                "{words} words"
            );
        }
        let ones = vec![0xFFu8; 4 * 16384];
        assert_eq!(fletcher64(&ones), reference_fletcher(&ones));
    }

    #[test]
    fn an_object_passes_only_with_the_checksum_of_its_other_bytes() {
        let mut block = vec![0u8; 4096];
        block[8] = 1;
        block[0x18] = 1;
        let sum = fletcher64(&block[8..]);
        block[..8].copy_from_slice(&sum.to_le_bytes());
        assert!(object(&block).is_ok());
        for at in [0, 7, 8, 100, 4095] {
            block[at] ^= 0x40;
            assert!(object(&block).is_err(), "flip at {at}");
            block[at] ^= 0x40;
        }
    }

    #[test]
    fn names_end_at_their_only_nul() {
        assert_eq!(terminated_name(b"a\0").unwrap(), 0..1);
        assert_eq!(terminated_name(b"\0").unwrap(), 0..0);
        assert!(terminated_name(b"").is_err());
        assert!(terminated_name(b"a").is_err());
        assert!(terminated_name(b"a\0b\0").is_err());
    }

    #[test]
    fn modes_apply_only_to_regular_files_and_folders() {
        assert_eq!(file_mode(REGULAR | 0o4755), 0o755);
        assert_eq!(file_mode(0o010_644), DEFAULT_FILE_MODE);
        assert_eq!(folder_mode(DIRECTORY | 0o055), 0o755);
        assert_eq!(folder_mode(REGULAR | 0o644), DEFAULT_FOLDER_MODE);
    }

    #[test]
    fn apfs_times_count_nanoseconds_from_1970() {
        assert_eq!(apfs_time(0), None);
        assert_eq!(
            apfs_time(1_500_000_000_123_456_789),
            Some(SystemTime::UNIX_EPOCH + Duration::new(1_500_000_000, 123_456_789))
        );
        assert!(apfs_time(u64::MAX).is_some());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn reference_fletcher(words: &[u8]) -> u64 {
        let (mut low, mut high) = (0u64, 0u64);
        for word in words.chunks_exact(4) {
            low =
                (low + u64::from(u32::from_le_bytes(word.try_into().unwrap()))) % FLETCHER_MODULUS;
            high = (high + low) % FLETCHER_MODULUS;
        }
        let check_low = FLETCHER_MODULUS - (low + high) % FLETCHER_MODULUS;
        let check_high = FLETCHER_MODULUS - (low + check_low) % FLETCHER_MODULUS;
        check_high << 32 | check_low
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn fletcher64_matches_a_word_at_a_time_reference() {
        let bytes: [u8; 12] = kani::any();
        let words: usize = kani::any();
        kani::assume(words <= 3);
        let slice = &bytes[..words * 4];
        assert!(fletcher64(slice) == reference_fletcher(slice));
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn objects_never_panic_and_pass_only_with_their_checksum() {
        let block: [u8; 36] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        if let Ok(header) = object(&block[..length]) {
            assert!(length >= OBJECT_HEADER && length % 4 == 0);
            assert!(le64(&block, 0) == fletcher64(&block[8..length]));
            assert!(header.kind <= TYPE_MASK && header.storage & !STORAGE_MASK == 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(101)]
    fn superblocks_keep_their_block_size_and_checkpoint_inside_the_container() {
        let bytes: [u8; SUPERBLOCK_FIELDS] = kani::any();
        if let Ok(fields) = superblock(&bytes) {
            assert!((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&fields.block_log));
            assert!(fields.block_count <= SIZE_LIMIT >> fields.block_log);
            assert!(fields.max_volumes as usize <= MAX_VOLUMES);
            assert!(
                fields.descriptors.blocks != 0
                    && fields.descriptors.blocks <= MAX_CHECKPOINT_BLOCKS
            );
            assert!(
                fields.descriptors.fits(fields.block_count) && fields.data.fits(fields.block_count)
            );
            assert!(fields.first_descriptor < fields.descriptors.blocks);
            assert!((1..=fields.descriptors.blocks).contains(&fields.descriptor_count));
            let step: u32 = kani::any();
            kani::assume(step < fields.descriptor_count);
            let block = fields.descriptors.at(fields.first_descriptor + step);
            assert!(block >= fields.descriptors.base);
            assert!(block < fields.descriptors.base + u64::from(fields.descriptors.blocks));
        }
    }

    #[kani::proof]
    fn checkpoint_maps_and_mappings_stay_inside_their_block_and_data_area() {
        let block: [u8; CHECKPOINT_HEADER + 2 * CHECKPOINT_MAPPING] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        if let Ok(map) = checkpoint_map(&block[..length]) {
            assert!(CHECKPOINT_HEADER + map.count * CHECKPOINT_MAPPING <= length);
        }
        let mapping: [u8; CHECKPOINT_MAPPING] = kani::any();
        let data = Area {
            base: kani::any(),
            blocks: kani::any(),
        };
        let block_log: u32 = kani::any();
        kani::assume((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_log));
        kani::assume(data.fits(SIZE_LIMIT >> block_log));
        if let Ok(blocks) = checkpoint_mapping(&mapping, data, block_log) {
            assert!(!blocks.is_empty() && blocks.start >= data.base);
            assert!(blocks.end <= data.base + u64::from(data.blocks));
        }
    }

    #[kani::proof]
    fn object_maps_and_volume_superblocks_never_panic() {
        let map: [u8; OBJECT_MAP_FIELDS] = kani::any();
        if let Ok(fields) = object_map(&map) {
            assert!(fields.root == le64(&map, 0x30));
        }
        let volume: [u8; VOLUME_FIELDS] = kani::any();
        if let Ok(fields) = volume_superblock(&volume) {
            assert!(&volume[0x20..0x24] == APSB);
            assert!(fields.root_tree == le64(&volume, 0x88));
        }
    }

    #[kani::proof]
    fn node_entries_stay_between_the_table_of_contents_and_the_values_end() {
        let block: [u8; NODE_HEADER + TREE_INFO + 32] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        let block = &block[..length];
        if let Ok(parsed) = node(block) {
            assert!(parsed.key_area <= parsed.value_end && parsed.value_end <= length);
            assert!(parsed.is_leaf() == (parsed.level == 0));
            let index: usize = kani::any();
            let sizes: (u32, u32) = (kani::any(), kani::any());
            if let Ok((key, value)) = entry(&parsed, block, index, sizes) {
                assert!(index < parsed.keys);
                assert!(parsed.key_area <= key.start && key.end <= parsed.value_end);
                assert!(parsed.key_area <= value.start && value.end <= parsed.value_end);
                assert!(key.start <= key.end && value.start <= value.end);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn directory_records_keep_their_names_inside_the_key() {
        let key: [u8; 16] = kani::any();
        let value: [u8; DIRECTORY_VALUE] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= key.len());
        let hashed: bool = kani::any();
        if let Ok((id, _)) = record_key(&key[..length]) {
            assert!(id <= RECORD_ID_MASK);
        }
        if let Ok(record) = directory_record(&key[..length], &value, hashed) {
            assert!(record.name.end < length);
            assert!(key[record.name.end] == 0);
            for at in record.name.clone() {
                assert!(key[at] != 0);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn attribute_records_keep_their_names_and_data_inside_the_record() {
        let key: [u8; 16] = kani::any();
        let value: [u8; XATTR_VALUE + XATTR_STREAM] = kani::any();
        let (key_length, value_length): (usize, usize) = (kani::any(), kani::any());
        kani::assume(key_length <= key.len() && value_length <= value.len());
        if let Ok(record) = xattr_record(&key[..key_length], &value[..value_length]) {
            assert!(record.name.end < key_length && key[record.name.end] == 0);
            if let XattrData::Embedded(range) = record.data {
                assert!(range.start == XATTR_VALUE && range.end == value_length);
            }
        }
        let extent_key: [u8; FILE_EXTENT_KEY] = kani::any();
        let extent_value: [u8; FILE_EXTENT_VALUE] = kani::any();
        if let Ok(extent) = file_extent_record(&extent_key, &extent_value) {
            assert!(extent.length <= EXTENT_LENGTH_MASK);
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn inode_extended_fields_stay_inside_the_inode() {
        let value: [u8; INODE + 24] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= value.len());
        if let Ok(record) = inode_record(&value[..length]) {
            assert!(length >= INODE);
            assert!(record.mode == le16(&value, 0x50));
            if record.stream.is_some() {
                assert!(length >= INODE + XFIELD_HEADER + XFIELD + DSTREAM);
            }
        }
    }
}
