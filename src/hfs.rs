use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::{Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    ops::Range,
    time::SystemTime,
};

pub(crate) trait Volume {
    fn size(&self) -> u64;
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()>;
}

pub(crate) const HEADER_AREA: usize = 1536;
const RESERVED: usize = 1024;
const SECTOR: usize = 512;
const FORK: usize = 80;
const EXTENTS_PER_RECORD: usize = 8;
const NODE_DESCRIPTOR: usize = 14;
const HEADER_RECORD: usize = 0x2A + 64;
const SMALLEST_NODE_LOG: u32 = 9;
const LARGEST_NODE_LOG: u32 = 15;
const SMALLEST_BLOCK_LOG: u32 = 9;
const LARGEST_BLOCK_LOG: u32 = 30;
const LEAF: u8 = 0xFF;
const FOLDER_RECORD: usize = 0x58;
const FILE_RECORD: usize = FOLDER_RECORD + 2 * FORK;
const FOLDER: u16 = 1;
const FILE: u16 = 2;
const EXTENT_KEY: usize = 10;
const EXTENT_RECORD: usize = 2 + EXTENT_KEY + EXTENTS_PER_RECORD * 8;
const DATA_FORK: u8 = 0;
const RESOURCE_FORK: u8 = 0xFF;
const ATTRIBUTE_KEY: usize = 14;
const INLINE_ATTRIBUTE: u32 = 0x10;
const FORK_ATTRIBUTE: u32 = 0x20;
const INLINE_HEADER: usize = 16;
const ROOT_PARENT: u32 = 1;
const ROOT_FOLDER: u32 = 2;
const CATALOG_FILE: u32 = 4;
const ATTRIBUTES_FILE: u32 = 8;
const MAX_FOLDERS: u32 = 1 << 29;
const MAX_FILES: u32 = 1 << 30;
const MAX_NODES: u32 = 1 << 24;
const MAX_DEPTH: usize = 1024;
const HFS_TO_UNIX_SECONDS: u64 = 2_082_844_800;
const TYPE_MASK: u16 = 0o170_000;
const SYMLINK: u16 = 0o120_000;
const REGULAR: u16 = 0o100_000;
const DIRECTORY: u16 = 0o040_000;
const DEFAULT_FILE_MODE: u32 = 0o666;
const DEFAULT_FOLDER_MODE: u32 = 0o777;
const HARD_LINK_TYPE: u32 = u32::from_be_bytes(*b"hlnk");
const HARD_LINK_CREATOR: u32 = u32::from_be_bytes(*b"hfs+");
const FOLDER_LINK_TYPE: u32 = u32::from_be_bytes(*b"fdrp");
const FOLDER_LINK_CREATOR: u32 = u32::from_be_bytes(*b"MACS");
const PRIVATE_DATA: &str = "HFS+ Private Data";
const PRIVATE_DATA_NAME: &str = "[HFS+ Private Data]";
const BLANK_NAME: &str = "[]";
const INODE_PREFIX: &str = "iNode";
const DECMPFS: &str = "com.apple.decmpfs";
const RESOURCE_FORK_ATTRIBUTE: &str = "com.apple.ResourceFork";
const DECMPFS_MAGIC: u32 = u32::from_le_bytes(*b"fpmc");
pub(crate) const DECMPFS_HEADER: usize = 16;
const COMPRESSED_CHUNK: usize = 1 << 16;
const LARGEST_COMPRESSED_CHUNK: usize = COMPRESSED_CHUNK + 1;
const ZLIB_FORK_HEADER: usize = 0x100;
const RESOURCE_MAP: u32 = 50;
const MAX_CHUNKS: u64 = 1 << 29;
const COPY_BUFFER: usize = 1 << 20;
const MAX_NAMES: usize = 64 * 1024 * 1024;
const DEFLATE_EXPANSION: u64 = 1032;
const LZVN_EXPANSION: u64 = 272;

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn le32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Extent {
    pub(crate) start: u32,
    pub(crate) count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ForkData {
    pub(crate) size: u64,
    pub(crate) blocks: u32,
    pub(crate) extents: [Extent; EXTENTS_PER_RECORD],
}

fn extents(bytes: &[u8]) -> [Extent; EXTENTS_PER_RECORD] {
    std::array::from_fn(|index| Extent {
        start: u32_at(bytes, index * 8),
        count: u32_at(bytes, index * 8 + 4),
    })
}

pub(crate) fn fork_data(bytes: &[u8; FORK]) -> ForkData {
    ForkData {
        size: u64_at(bytes, 0),
        blocks: u32_at(bytes, 12),
        extents: extents(&bytes[16..]),
    }
}

impl ForkData {
    fn is_empty(&self) -> bool {
        self.size == 0 && self.blocks == 0 && self.extents.iter().all(|extent| extent.count == 0)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct VolumeHeader {
    pub(crate) block_log: u32,
    pub(crate) files: u32,
    pub(crate) folders: u32,
    pub(crate) blocks: u32,
    pub(crate) extents: ForkData,
    pub(crate) catalog: ForkData,
    pub(crate) attributes: ForkData,
}

pub(crate) fn volume_header(bytes: &[u8; SECTOR]) -> Field<VolumeHeader> {
    field_check(
        matches!(&bytes[..4], b"H+\0\x04" | b"HX\0\x05"),
        "HFS volume header has no HFS+ or HFSX signature",
    )?;

    let block_size = u32_at(bytes, 0x28);
    field_check(
        block_size.is_power_of_two()
            && (SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_size.trailing_zeros()),
        "HFS block size is not a power of two from 512 bytes to 1 GiB",
    )?;
    let files = u32_at(bytes, 0x20);
    let folders = u32_at(bytes, 0x24);
    field_check(
        folders <= MAX_FOLDERS && files <= MAX_FILES,
        "HFS volume counts more files or folders than 7-Zip reads",
    )?;
    let fork = |index: usize| {
        let at = 0x70 + 0x50 * index;
        fork_data(bytes[at..at + FORK].try_into().unwrap())
    };
    Ok(VolumeHeader {
        block_log: block_size.trailing_zeros(),
        files,
        folders,
        blocks: u32_at(bytes, 0x2C),
        extents: fork(1),
        catalog: fork(2),
        attributes: fork(3),
    })
}

pub(crate) fn embedded_volume(bytes: &[u8; SECTOR]) -> Field<u64> {
    field_check(
        &bytes[..2] == b"BD" && &bytes[0x7C..0x7E] == b"H+",
        "HFS wrapper holds no HFS+ volume",
    )?;
    let block_size = u32_at(bytes, 0x14);
    field_check(
        block_size != 0 && block_size.is_multiple_of(SECTOR as u32),
        "HFS wrapper block size is not a multiple of 512 bytes",
    )?;
    let first_block_sector = u64::from(u16_at(bytes, 0x1C));
    let start = u64::from(u16_at(bytes, 0x7E));
    Ok(first_block_sector * SECTOR as u64 + start * u64::from(block_size))
}

pub(crate) fn starts_with_volume(start: &[u8]) -> bool {
    if start.len() < HEADER_AREA {
        return false;
    }
    let header = &start[RESERVED..HEADER_AREA];
    let signed = matches!(&header[..4], b"H+\0\x04" | b"HX\0\x05");
    if signed {
        return start[..RESERVED].iter().all(|&byte| byte == 0);
    }
    &header[..2] == b"BD" && &header[0x7C..0x7E] == b"H+" && matches!(&start[..2], b"LK" | [0, 0])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeHeader {
    pub(crate) first_leaf: u32,
    pub(crate) node_log: u32,
    pub(crate) nodes: u32,
}

pub(crate) fn tree_header(
    bytes: &[u8; NODE_DESCRIPTOR + HEADER_RECORD],
    file_blocks: u32,
    block_log: u32,
) -> Field<TreeHeader> {
    let record = &bytes[NODE_DESCRIPTOR..];
    let node_size = u16_at(record, 0x12);
    field_check(
        node_size.is_power_of_two()
            && (SMALLEST_NODE_LOG..=LARGEST_NODE_LOG).contains(&node_size.trailing_zeros()),
        "HFS B-tree node size is not a power of two from 512 bytes to 32 KiB",
    )?;
    let node_log = node_size.trailing_zeros();
    let nodes = u32_at(record, 0x16);
    let file_bytes = u64::from(file_blocks) << block_log;
    field_check(
        u64::from(nodes) <= file_bytes >> node_log,
        "HFS B-tree counts more nodes than its file holds",
    )?;
    field_check(
        nodes <= MAX_NODES,
        "HFS B-tree has more than 16,777,216 nodes",
    )?;
    Ok(TreeHeader {
        first_leaf: u32_at(record, 0xA),
        node_log,
        nodes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) next: u32,
    pub(crate) kind: u8,
    pub(crate) records: usize,
}

pub(crate) fn node_descriptor(node: &[u8]) -> Field<Node> {
    field_check(
        node.len() >= NODE_DESCRIPTOR,
        "HFS B-tree node is too short",
    )?;
    let records = usize::from(u16_at(node, 10));
    let table = (records + 1) * 2;
    field_check(
        NODE_DESCRIPTOR + table <= node.len(),
        "HFS B-tree node holds more records than fit",
    )?;
    let limit = node.len() - table;
    for index in 0..records {
        let start = usize::from(u16_at(node, node.len() - 2 * (index + 1)));
        let end = usize::from(u16_at(node, node.len() - 2 * (index + 2)));
        field_check(
            start >= NODE_DESCRIPTOR && start < end && end <= limit,
            "HFS B-tree node has record offsets out of order",
        )?;
    }
    Ok(Node {
        next: u32_at(node, 0),
        kind: node[8],
        records,
    })
}

pub(crate) fn record(node: &[u8], index: usize) -> &[u8] {
    let start = usize::from(u16_at(node, node.len() - 2 * (index + 1)));
    let end = usize::from(u16_at(node, node.len() - 2 * (index + 2)));
    &node[start..end]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogEntry {
    pub(crate) parent: u32,
    pub(crate) name: Range<usize>,
    pub(crate) folder: bool,
    pub(crate) id: u32,
    pub(crate) modified: u32,
    pub(crate) mode: u16,
    pub(crate) special: u32,
    pub(crate) finder_type: u32,
    pub(crate) creator: u32,
    pub(crate) data: ForkData,
    pub(crate) resource: ForkData,
}

pub(crate) fn catalog_record(record: &[u8]) -> Field<Option<CatalogEntry>> {
    field_check(record.len() >= 6, "HFS catalog record is too short")?;
    let key = usize::from(u16_at(record, 0));
    field_check(
        key >= 6 && key % 2 == 0 && key + 2 <= record.len(),
        "HFS catalog key does not fit its record",
    )?;
    let parent = u32_at(record, 2);
    let name_units = usize::from(u16_at(record, 6));
    field_check(
        name_units * 2 == key - 6,
        "HFS catalog name does not fill its key",
    )?;
    let body = &record[key + 2..];
    field_check(body.len() >= 2, "HFS catalog record has no type")?;
    let kind = u16_at(body, 0);
    if kind != FOLDER && kind != FILE {
        return Ok(None);
    }
    let folder = kind == FOLDER;
    field_check(
        body.len() == if folder { FOLDER_RECORD } else { FILE_RECORD },
        "HFS catalog record has the wrong size for its type",
    )?;
    let fork = |at: usize| fork_data(body[at..at + FORK].try_into().unwrap());
    Ok(Some(CatalogEntry {
        parent,
        name: 8..8 + name_units * 2,
        folder,
        id: u32_at(body, 8),
        modified: u32_at(body, 0x10),
        mode: u16_at(body, 0x2A),
        special: u32_at(body, 0x2C),
        finder_type: u32_at(body, 0x30),
        creator: u32_at(body, 0x34),
        data: if folder {
            ForkData::default()
        } else {
            fork(FOLDER_RECORD)
        },
        resource: if folder {
            ForkData::default()
        } else {
            fork(FOLDER_RECORD + FORK)
        },
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExtentRecord {
    pub(crate) resource: bool,
    pub(crate) id: u32,
    pub(crate) start: u32,
    pub(crate) extents: [Extent; EXTENTS_PER_RECORD],
}

pub(crate) fn extent_record(record: &[u8]) -> Field<Option<ExtentRecord>> {
    field_check(
        record.len() == EXTENT_RECORD && usize::from(u16_at(record, 0)) == EXTENT_KEY,
        "HFS extents record has the wrong size",
    )?;
    let resource = match record[2] {
        DATA_FORK => false,
        RESOURCE_FORK => true,
        _ => return Ok(None),
    };
    Ok(Some(ExtentRecord {
        resource,
        id: u32_at(record, 4),
        start: u32_at(record, 8),
        extents: extents(&record[2 + EXTENT_KEY..]),
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AttributeValue {
    Inline(Range<usize>),
    Fork(ForkData),
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attribute {
    pub(crate) id: u32,
    pub(crate) name: Range<usize>,
    pub(crate) value: AttributeValue,
}

pub(crate) fn attribute_record(record: &[u8]) -> Field<Attribute> {
    field_check(
        record.len() >= ATTRIBUTE_KEY,
        "HFS attribute record is too short",
    )?;
    let key = usize::from(u16_at(record, 0));
    let id = u32_at(record, 4);
    let start = u32_at(record, 8);
    let name_units = usize::from(u16_at(record, 12));
    let name = ATTRIBUTE_KEY..ATTRIBUTE_KEY + name_units * 2;
    if start != 0 {
        return Ok(Attribute {
            id,
            name: 0..0,
            value: AttributeValue::Unsupported,
        });
    }
    field_check(
        key + 2 <= record.len() && key == ATTRIBUTE_KEY - 2 + name_units * 2,
        "HFS attribute key does not fit its record",
    )?;
    let body = &record[name.end..];
    field_check(body.len() >= 4, "HFS attribute record has no type")?;
    let value = match u32_at(body, 0) {
        FORK_ATTRIBUTE => {
            field_check(
                body.len() == 8 + FORK && u32_at(body, 4) == 0,
                "HFS fork attribute has the wrong size",
            )?;
            AttributeValue::Fork(fork_data(body[8..8 + FORK].try_into().unwrap()))
        }
        INLINE_ATTRIBUTE => {
            field_check(
                body.len() >= INLINE_HEADER && u32_at(body, 4) == 0 && u32_at(body, 8) == 0,
                "HFS inline attribute has a bad header",
            )?;
            let size = u32_at(body, 12) as usize;
            field_check(
                size <= body.len() - INLINE_HEADER,
                "HFS inline attribute is larger than its record",
            )?;
            let at = name.end + INLINE_HEADER;
            AttributeValue::Inline(at..at + size)
        }
        _ => AttributeValue::Unsupported,
    };
    Ok(Attribute { id, name, value })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stored {
    Inline,
    ZlibInline,
    LzvnInline,
    ZlibFork,
    LzvnFork,
    Unsupported(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decmpfs {
    pub(crate) size: u64,
    pub(crate) stored: Stored,
}

const ZLIB_INLINE: u32 = 3;
const ZLIB_FORK: u32 = 4;
const LZVN_INLINE: u32 = 7;
const LZVN_FORK: u32 = 8;
const COPY_INLINE: u32 = 9;
const COPY_MARKER: u8 = 0xCC;
const LZVN_STORED_MARKER: u8 = 0x06;
const ZLIB_STORED_MARKER: u8 = 0x0F;

pub(crate) fn decmpfs(attribute: &[u8]) -> Field<Decmpfs> {
    field_check(
        attribute.len() >= DECMPFS_HEADER && le32_at(attribute, 0) == DECMPFS_MAGIC,
        "HFS compressed file has no decmpfs header",
    )?;
    let method = le32_at(attribute, 4);
    let size = u64::from_le_bytes(attribute[8..16].try_into().unwrap());
    let payload = &attribute[DECMPFS_HEADER..];
    let stored = match method {
        ZLIB_FORK | LZVN_FORK if payload.is_empty() => {
            if method == ZLIB_FORK {
                Stored::ZlibFork
            } else {
                Stored::LzvnFork
            }
        }
        ZLIB_INLINE | LZVN_INLINE | COPY_INLINE if !payload.is_empty() => {
            let marked = match method {
                ZLIB_INLINE => payload[0] & 0xF == ZLIB_STORED_MARKER,
                LZVN_INLINE => payload[0] == LZVN_STORED_MARKER,
                _ => payload[0] == COPY_MARKER,
            };
            match (marked, method) {
                (true, _) if size == payload.len() as u64 - 1 => Stored::Inline,
                (false, ZLIB_INLINE) => Stored::ZlibInline,
                (false, LZVN_INLINE) => Stored::LzvnInline,
                _ => Stored::Unsupported(method),
            }
        }
        _ => Stored::Unsupported(method),
    };
    Ok(Decmpfs { size, stored })
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZlibEnd {
    Exact,
    Padded,
}

pub(crate) fn inflate_zlib(input: &[u8], output: &mut [u8], end: ZlibEnd) -> Result<()> {
    ensure!(
        input.len() >= 2
            && input[0] & 0xF == 8
            && input[0] >> 4 <= 7
            && input[1] & 0x20 == 0
            && (u32::from(input[0]) << 8 | u32::from(input[1])) % 31 == 0,
        "zlib stream has a bad header"
    );
    let mut inflater = flate2::Decompress::new(false);
    let status = inflater
        .decompress(&input[2..], output, flate2::FlushDecompress::Finish)
        .context("zlib stream is damaged")?;
    ensure!(
        status == flate2::Status::StreamEnd && inflater.total_out() == output.len() as u64,
        "zlib stream does not decode to its declared size"
    );
    let rest = &input[2 + inflater.total_in() as usize..];
    if rest.is_empty() {
        return Ok(());
    }
    ensure!(rest.len() >= 4, "zlib stream ends inside its checksum");
    ensure!(
        u32::from_be_bytes(rest[..4].try_into().unwrap()) == adler32(output),
        "zlib stream fails its Adler-32 check"
    );
    ensure!(
        rest.len() == 4 || end == ZlibEnd::Padded,
        "zlib stream has data after its end"
    );
    Ok(())
}

fn hfs_time(seconds: u32) -> Option<SystemTime> {
    if seconds == 0 {
        return None;
    }
    let seconds = u64::from(seconds);
    if seconds >= HFS_TO_UNIX_SECONDS {
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(
            seconds - HFS_TO_UNIX_SECONDS,
        ))
    } else {
        std::time::UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(
            HFS_TO_UNIX_SECONDS - seconds,
        ))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Fork {
    pub(crate) size: u64,
    pub(crate) blocks: u32,
    pub(crate) extents: Vec<Extent>,
}

#[derive(Default)]
struct Overflow {
    groups: HashMap<(bool, u32), (u32, Vec<Extent>)>,
}

fn used(extents: &[Extent]) -> impl Iterator<Item = Extent> + '_ {
    extents.iter().copied().filter(|extent| extent.count != 0)
}

fn block_sum(extents: &[Extent]) -> u64 {
    extents.iter().map(|extent| u64::from(extent.count)).sum()
}

// EXTERNAL(7-Zip HfsHandler CFork::UpgradeAndTest): a fork's overflow extents start at the block its
// eight own extents end, and all its extents together hold exactly its block count.
fn fork_with(data: &ForkData, overflow: &Overflow, resource: bool, id: u32) -> Option<Fork> {
    let mut extents: Vec<Extent> = used(&data.extents).collect();
    if let Some((start, more)) = overflow.groups.get(&(resource, id)) {
        if block_sum(&extents) != u64::from(*start) {
            return None;
        }
        extents.extend_from_slice(more);
    }
    (block_sum(&extents) == u64::from(data.blocks)).then_some(Fork {
        size: data.size,
        blocks: data.blocks,
        extents,
    })
}

pub(crate) struct Reader<'v> {
    volume: &'v mut dyn Volume,
    header: VolumeHeader,
    base: u64,
}

impl Reader<'_> {
    fn check_fork(&self, fork: &Fork, what: &str) -> Result<()> {
        ensure!(
            fork.size <= u64::from(fork.blocks) << self.header.block_log,
            "HFS {what} is larger than its blocks"
        );
        for extent in &fork.extents {
            let end = u64::from(extent.start) + u64::from(extent.count);
            ensure!(
                end <= u64::from(self.header.blocks),
                "HFS {what} has an extent past the end of the volume"
            );
            ensure!(
                self.base + (end << self.header.block_log) <= self.volume.size(),
                "HFS {what} has an extent past the end of the image"
            );
        }
        Ok(())
    }

    fn read_fork(&mut self, fork: &Fork, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let mut logical = 0u64;
        let mut offset = offset;
        let mut buffer = buffer;
        for extent in &fork.extents {
            if buffer.is_empty() {
                break;
            }
            let length = u64::from(extent.count) << self.header.block_log;
            if offset < logical + length {
                let inside = offset - logical;
                let take = (length - inside).min(buffer.len() as u64) as usize;
                let physical =
                    self.base + (u64::from(extent.start) << self.header.block_log) + inside;
                let (now, rest) = buffer.split_at_mut(take);
                self.volume.read_exact_at(physical, now)?;
                buffer = rest;
                offset += take as u64;
            }
            logical += length;
        }
        ensure!(buffer.is_empty(), "HFS read past the end of a fork");
        Ok(())
    }
}

struct Walk {
    header: TreeHeader,
    next: u32,
    current: u32,
    seen: HashSet<u32>,
    node: Vec<u8>,
}

impl Walk {
    fn open(reader: &mut Reader, fork: &Fork, what: &'static str) -> Result<Walk> {
        ensure!(
            fork.blocks < reader.header.blocks,
            "HFS {what} file is as large as the volume"
        );
        reader.check_fork(fork, what)?;
        let mut start = [0u8; NODE_DESCRIPTOR + HEADER_RECORD];
        reader.read_fork(fork, 0, &mut start)?;
        let header = tree_header(&start, fork.blocks, reader.header.block_log)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("HFS {what} file has a bad header node"))?;
        ensure!(
            header.first_leaf == 0 || header.nodes != 0,
            "HFS {what} file has a leaf but no nodes"
        );
        Ok(Walk {
            header,
            next: header.first_leaf,
            current: 0,
            seen: HashSet::new(),
            node: vec![0; 1 << header.node_log],
        })
    }

    fn read_node(&mut self, reader: &mut Reader, fork: &Fork, number: u32) -> Result<Node> {
        reader.read_fork(
            fork,
            u64::from(number) << self.header.node_log,
            &mut self.node,
        )?;
        Ok(node_descriptor(&self.node)?)
    }

    fn next_leaf(&mut self, reader: &mut Reader, fork: &Fork, what: &str) -> Result<Option<Node>> {
        if self.next == 0 {
            return Ok(None);
        }
        let number = self.next;
        ensure!(
            number < self.header.nodes,
            "HFS {what} file links to a node outside the file"
        );
        ensure!(
            self.seen.insert(number),
            "HFS {what} file links its leaves in a loop"
        );
        let node = self
            .read_node(reader, fork, number)
            .with_context(|| format!("HFS {what} node {number} is damaged"))?;
        ensure!(
            node.kind == LEAF,
            "HFS {what} leaf list reaches a node that is not a leaf"
        );
        self.current = number;
        self.next = node.next;
        Ok(Some(node))
    }
}

fn utf16_name(bytes: &[u8]) -> Option<String> {
    char::decode_utf16(
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
    )
    .collect::<Result<String, _>>()
    .ok()
}

// EXTERNAL(7-Zip HfsHandler LoadName): the folder of hard link data shows as
// "[HFS+ Private Data]", and an empty or one-space name as "[]".
fn shown_name(units: &[u8]) -> Result<String> {
    let name = utf16_name(units)
        .with_context(|| format!("HFS name is not valid UTF-16; {BACKEND_HINT}"))?;
    if name.contains('\0') {
        let private = units.len() == (4 + PRIVATE_DATA.len()) * 2
            && name.starts_with("\0\0\0\0")
            && &name[4..] == PRIVATE_DATA;
        ensure!(private, "HFS name {name:?} holds a NUL; {BACKEND_HINT}");
        return Ok(PRIVATE_DATA_NAME.to_owned());
    }
    let mut letters = name.chars();
    if matches!(
        (letters.next(), letters.next()),
        (None, _) | (Some(' '), None)
    ) {
        return Ok(BLANK_NAME.to_owned());
    }
    Ok(name.replace('/', "_"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Folder,
    File,
    Symlink,
    HardLink(u32),
}

struct Item {
    id: u32,
    parent: u32,
    name: Range<usize>,
    kind: Kind,
    mode: u16,
    modified: u32,
    data: Fork,
    resource: Fork,
}

struct AttributeAt {
    node: u32,
    record: u16,
}

#[derive(Default)]
struct Attributes {
    fork: Fork,
    node_log: u32,
    compressed: HashMap<u32, AttributeAt>,
    owners: Vec<u32>,
    restored: Vec<(u32, AttributeAt)>,
}

fn load_overflow(reader: &mut Reader) -> Result<Overflow> {
    let mut overflow = Overflow::default();
    let header = reader.header.extents;
    let fork = Fork {
        size: header.size,
        blocks: header.blocks,
        extents: used(&header.extents).collect(),
    };
    ensure!(
        block_sum(&fork.extents) == u64::from(fork.blocks),
        "HFS extents file does not fill its blocks"
    );
    if fork.blocks == 0 {
        return Ok(overflow);
    }
    let mut walk = Walk::open(reader, &fork, "extents")?;
    let mut last: Option<((bool, u32), u64)> = None;
    while let Some(node) = walk.next_leaf(reader, &fork, "extents")? {
        for index in 0..node.records {
            let Some(found) = extent_record(record(&walk.node, index))? else {
                continue;
            };
            let key = (found.resource, found.id);
            let start = match last {
                Some((previous, end)) if previous == key => {
                    ensure!(
                        end == u64::from(found.start),
                        "HFS extents records of one fork leave a gap"
                    );
                    end
                }
                _ => {
                    ensure!(
                        overflow
                            .groups
                            .insert(key, (found.start, Vec::new()))
                            .is_none(),
                        "HFS extents file holds one fork's records apart"
                    );
                    u64::from(found.start)
                }
            };
            let group = &mut overflow.groups.get_mut(&key).unwrap().1;
            group.extend(used(&found.extents));
            last = Some((key, start + block_sum(&found.extents)));
        }
    }
    Ok(overflow)
}

fn special_fork(
    reader: &Reader,
    data: &ForkData,
    overflow: &Overflow,
    id: u32,
    what: &str,
) -> Result<Fork> {
    let fork = fork_with(data, overflow, false, id)
        .with_context(|| format!("HFS {what} file's extents do not add up to its blocks"))?;
    reader.check_fork(&fork, what)?;
    Ok(fork)
}

fn load_attributes(reader: &mut Reader, overflow: &Overflow) -> Result<Attributes> {
    let mut attributes = Attributes::default();
    let header = reader.header;
    attributes.fork = special_fork(
        reader,
        &header.attributes,
        overflow,
        ATTRIBUTES_FILE,
        "attributes",
    )?;
    if attributes.fork.size == 0 || attributes.fork.blocks == 0 {
        return Ok(attributes);
    }
    let fork = attributes.fork.clone();
    let mut walk = Walk::open(reader, &fork, "attributes")?;
    attributes.node_log = walk.header.node_log;
    let mut count = 0usize;
    let mut names = crate::xattr::EntryNames::default();
    while let Some(node) = walk.next_leaf(reader, &fork, "attributes")? {
        for index in 0..node.records {
            let bytes = record(&walk.node, index);
            let attribute = attribute_record(bytes)?;
            ensure!(
                attribute.value != AttributeValue::Unsupported,
                "HFS file {} has an attribute in a form 7-Zip does not read; {BACKEND_HINT}",
                attribute.id
            );
            ensure!(
                count < tree::MAX_ENTRIES,
                "HFS volume has more than {} attributes",
                tree::MAX_ENTRIES
            );
            count += 1;
            if attributes.owners.last() != Some(&attribute.id) {
                attributes.owners.push(attribute.id);
                names.clear();
            }
            let at = AttributeAt {
                node: walk.current,
                record: index as u16,
            };
            let name = utf16_name(&bytes[attribute.name.clone()]).with_context(|| {
                format!(
                    "HFS entry {} has an extended attribute whose name is not valid UTF-16",
                    attribute.id
                )
            })?;
            names
                .add(name.as_bytes())
                .with_context(|| format!("HFS entry {}", attribute.id))?;
            if name != DECMPFS {
                if name != RESOURCE_FORK_ATTRIBUTE && crate::xattr::is_restored(&name) {
                    let size = match &attribute.value {
                        AttributeValue::Fork(fork) => fork.size,
                        AttributeValue::Inline(range) => range.len() as u64,
                        AttributeValue::Unsupported => 0,
                    };
                    crate::xattr::check_size(&name, size)
                        .with_context(|| format!("HFS entry {}", attribute.id))?;
                    attributes.restored.push((attribute.id, at));
                }
                continue;
            }
            ensure!(
                matches!(attribute.value, AttributeValue::Inline(_)),
                "HFS file {} keeps its decmpfs header in a fork; {BACKEND_HINT}",
                attribute.id
            );
            ensure!(
                attributes.compressed.insert(attribute.id, at).is_none(),
                "HFS file {} has two decmpfs headers",
                attribute.id
            );
        }
    }
    let mut runs = attributes.owners.clone();
    runs.sort_unstable();
    if let Some(pair) = runs.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!(
            "HFS entry {} has extended attribute records that lie apart",
            pair[0]
        );
    }
    attributes.restored.sort_by_key(|&(owner, _)| owner);
    Ok(attributes)
}

struct Catalog {
    items: Vec<Item>,
    names: String,
    index: HashMap<u32, usize>,
}

impl Catalog {
    fn name(&self, item: &Item) -> &str {
        &self.names[item.name.clone()]
    }
}

fn kind_of(entry: &CatalogEntry) -> Result<Kind> {
    if entry.folder {
        return Ok(Kind::Folder);
    }
    if entry.finder_type == HARD_LINK_TYPE && entry.creator == HARD_LINK_CREATOR {
        ensure!(
            entry.data.is_empty() && entry.resource.is_empty(),
            "HFS hard link {} holds data of its own",
            entry.id
        );
        return Ok(Kind::HardLink(entry.special));
    }
    ensure!(
        !(entry.finder_type == FOLDER_LINK_TYPE && entry.creator == FOLDER_LINK_CREATOR),
        "HFS volume holds a hard link to a folder, which Zipper cannot create; {BACKEND_HINT}"
    );
    Ok(if entry.mode & TYPE_MASK == SYMLINK {
        Kind::Symlink
    } else {
        Kind::File
    })
}

fn load_catalog(reader: &mut Reader, overflow: &Overflow) -> Result<Catalog> {
    let header = reader.header;
    let fork = special_fork(reader, &header.catalog, overflow, CATALOG_FILE, "catalog")?;
    ensure!(fork.blocks != 0, "HFS volume has no catalog");
    let mut walk = Walk::open(reader, &fork, "catalog")?;
    let mut catalog = Catalog {
        items: Vec::new(),
        names: String::new(),
        index: HashMap::new(),
    };
    let (mut files, mut folders) = (0u64, 0u64);
    while let Some(node) = walk.next_leaf(reader, &fork, "catalog")? {
        for index in 0..node.records {
            let bytes = record(&walk.node, index);
            let Some(entry) = catalog_record(bytes)? else {
                continue;
            };
            ensure!(
                catalog.items.len() < tree::MAX_ENTRIES,
                "HFS volume has more than {} entries",
                tree::MAX_ENTRIES
            );
            let kind = kind_of(&entry)?;
            let name = shown_name(&bytes[entry.name.clone()])?;
            ensure!(
                catalog.names.len() + name.len() <= MAX_NAMES,
                "HFS names exceed 64 MiB"
            );
            let start = catalog.names.len();
            catalog.names.push_str(&name);
            let (data, resource) = if entry.folder {
                folders += 1;
                (Fork::default(), Fork::default())
            } else {
                files += 1;
                let fork = |data: &ForkData, resource: bool, what: &str| -> Result<Fork> {
                    let fork =
                        fork_with(data, overflow, resource, entry.id).with_context(|| {
                            format!("HFS {what} of {name:?} does not add up to its blocks")
                        })?;
                    reader.check_fork(&fork, &format!("{what} of {name:?}"))?;
                    Ok(fork)
                };
                (
                    fork(&entry.data, false, "data")?,
                    fork(&entry.resource, true, "resource fork")?,
                )
            };
            ensure!(
                catalog
                    .index
                    .insert(entry.id, catalog.items.len())
                    .is_none(),
                "HFS catalog holds two entries numbered {}",
                entry.id
            );
            catalog.items.push(Item {
                id: entry.id,
                parent: entry.parent,
                name: start..catalog.names.len(),
                kind,
                mode: entry.mode,
                modified: entry.modified,
                data,
                resource,
            });
        }
    }
    ensure!(
        files == u64::from(header.files) && folders == u64::from(header.folders) + 1,
        "HFS volume header counts {} files and {} folders, and its catalog holds {files} and {folders}",
        header.files,
        u64::from(header.folders) + 1
    );
    Ok(catalog)
}

fn check_attribute_owners(catalog: &Catalog, attributes: &Attributes) -> Result<()> {
    for owner in &attributes.owners {
        ensure!(
            catalog.index.contains_key(owner),
            "HFS attribute belongs to entry {owner}, which the catalog does not hold"
        );
    }
    for id in attributes.compressed.keys() {
        let item = &catalog.items[catalog.index[id]];
        ensure!(
            item.data.blocks == 0 && item.data.size == 0,
            "HFS entry {:?} is compressed and has data of its own",
            catalog.name(item)
        );
        ensure!(
            !matches!(item.kind, Kind::HardLink(_)),
            "HFS hard link {:?} is compressed, so it holds data of its own",
            catalog.name(item)
        );
    }
    Ok(())
}

fn check_no_overlap(reader: &Reader, catalog: &Catalog, special: &[&Fork]) -> Result<()> {
    let mut spans: Vec<(u32, u32)> = special
        .iter()
        .flat_map(|fork| fork.extents.iter())
        .chain(
            catalog
                .items
                .iter()
                .flat_map(|item| item.data.extents.iter().chain(&item.resource.extents)),
        )
        .map(|extent| (extent.start, extent.count))
        .collect();
    spans.sort_unstable();
    for pair in spans.windows(2) {
        ensure!(
            u64::from(pair[0].0) + u64::from(pair[0].1) <= u64::from(pair[1].0),
            "HFS volume has two extents that share block {}",
            pair[1].0
        );
    }
    let _ = reader;
    Ok(())
}

struct ForkStream<'r, 'v> {
    reader: &'r mut Reader<'v>,
    fork: Fork,
    position: u64,
}

impl Read for ForkStream<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.fork.size - self.position;
        let take = (left.min(buffer.len() as u64) as usize).min(COPY_BUFFER);
        if take == 0 {
            return Ok(0);
        }
        self.reader
            .read_fork(&self.fork, self.position, &mut buffer[..take])
            .map_err(std::io::Error::other)?;
        self.position += take as u64;
        Ok(take)
    }
}

fn exact_blocks(fork: &Fork, block_log: u32, what: &str) -> Result<()> {
    let needed = fork.size.div_ceil(1 << block_log);
    ensure!(
        u64::from(fork.blocks) == needed,
        "HFS {what} holds {} blocks for {} bytes",
        fork.blocks,
        fork.size
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkTable {
    Zlib,
    Lzvn,
}

struct Compressed<'r, 'v> {
    reader: &'r mut Reader<'v>,
    fork: Fork,
    table: ChunkTable,
    size: u64,
    chunks: u64,
    chunk: u64,
    next_offset: u64,
    packed: Vec<u8>,
    decoded: Vec<u8>,
    held: Range<usize>,
}

impl<'r, 'v> Compressed<'r, 'v> {
    fn open(reader: &'r mut Reader<'v>, fork: Fork, table: ChunkTable, size: u64) -> Result<Self> {
        let chunks = size.div_ceil(COMPRESSED_CHUNK as u64);
        ensure!(
            chunks < MAX_CHUNKS,
            "HFS compressed file is larger than 7-Zip reads"
        );
        let mut stream = Compressed {
            reader,
            fork,
            table,
            size,
            chunks,
            chunk: 0,
            next_offset: 0,
            packed: vec![0; LARGEST_COMPRESSED_CHUNK],
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
        ensure!(
            at.checked_add(length as u64)
                .is_some_and(|end| end <= self.fork.size),
            "HFS compressed file's table reaches past its resource fork"
        );
        let mut bytes = vec![0; length];
        let fork = self.fork.clone();
        self.reader.read_fork(&fork, at, &mut bytes)?;
        Ok(bytes)
    }

    // EXTERNAL(7-Zip HfsHandler ExtractResourceFork_LZFSE): a table of chunk count + 1 offsets
    // that starts at its own size, rises strictly and ends at the resource fork's size.
    fn lzvn_table_start(&mut self) -> Result<u64> {
        let table = (self.chunks + 1) * 4;
        ensure!(
            table <= self.fork.size,
            "HFS compressed file's chunk table is larger than its resource fork"
        );
        let first = le32_at(&self.fork_bytes(0, 4)?, 0);
        ensure!(
            u64::from(first) == table,
            "HFS compressed file's chunk table does not start at its end"
        );
        Ok(table)
    }

    // EXTERNAL(7-Zip HfsHandler ExtractResourceFork_ZLIB): a resource fork with one 'cmpf'
    // resource, whose data starts with the chunk count and a table of offsets and sizes.
    fn zlib_table_start(&mut self) -> Result<u64> {
        let size = self.fork.size;
        let head = self.fork_bytes(0, ZLIB_FORK_HEADER + 8)?;
        let (data_at, map_at) = (u32_at(&head, 0), u32_at(&head, 4));
        let (data_size, map_size) = (u32_at(&head, 8), u32_at(&head, 12));
        ensure!(
            map_size == RESOURCE_MAP
                && data_at == ZLIB_FORK_HEADER as u32
                && data_at <= map_at
                && data_size == map_at - data_at
                && u64::from(map_at) + u64::from(map_size) == size,
            "HFS compressed file's resource fork has a bad header"
        );
        let resource_size = u32_at(&head, ZLIB_FORK_HEADER);
        let chunks = le32_at(&head, ZLIB_FORK_HEADER + 4);
        ensure!(
            4 + u64::from(resource_size) == u64::from(data_size)
                && resource_size >= 8
                && u64::from(chunks) <= u64::from((resource_size - 4) >> 3)
                && u64::from(chunks) == self.chunks,
            "HFS compressed file's resource holds the wrong number of chunks"
        );
        let map = self.fork_bytes(u64::from(map_at), RESOURCE_MAP as usize)?;
        let map_ok = u16_at(&map, 24) == 28
            && u16_at(&map, 26) == RESOURCE_MAP as u16
            && u16_at(&map, 28) == 0
            && &map[30..34] == b"cmpf"
            && u16_at(&map, 34) == 0
            && u16_at(&map, 36) == 10
            && u16_at(&map, 38) == 1
            && u16_at(&map, 40) == 0xFFFF
            && u32_at(&map, 42) & 0xFF_FFFF == 0;
        ensure!(
            map_ok,
            "HFS compressed file's resource map is not the one 7-Zip reads"
        );
        Ok(4 + self.chunks * 8)
    }

    fn chunk_span(&mut self) -> Result<(u64, usize)> {
        let index = self.chunk;
        let (start, length) = match self.table {
            ChunkTable::Lzvn => {
                let pair = self.fork_bytes(index * 4, 8)?;
                let (start, end) = (le32_at(&pair, 0), le32_at(&pair, 4));
                ensure!(
                    u64::from(start) == self.next_offset && end > start,
                    "HFS compressed file's chunk table does not rise"
                );
                (u64::from(start), u64::from(end - start))
            }
            ChunkTable::Zlib => {
                let at = (ZLIB_FORK_HEADER + 8) as u64 + index * 8;
                let pair = self.fork_bytes(at, 8)?;
                let (start, length) = (le32_at(&pair, 0), le32_at(&pair, 4));
                ensure!(
                    u64::from(start) == self.next_offset && length != 0,
                    "HFS compressed file's chunks do not follow one another"
                );
                (
                    (ZLIB_FORK_HEADER + 4) as u64 + u64::from(start),
                    u64::from(length),
                )
            }
        };
        ensure!(
            length <= LARGEST_COMPRESSED_CHUNK as u64,
            "HFS compressed chunk is larger than 64 KiB and one byte"
        );
        self.next_offset = match self.table {
            ChunkTable::Lzvn => start + length,
            ChunkTable::Zlib => start - (ZLIB_FORK_HEADER + 4) as u64 + length,
        };
        Ok((start, length as usize))
    }

    fn finish(&mut self) -> Result<()> {
        let end = match self.table {
            ChunkTable::Lzvn => self.fork.size,
            ChunkTable::Zlib => u64::from(u32_at(&self.fork_bytes(ZLIB_FORK_HEADER as u64, 4)?, 0)),
        };
        ensure!(
            self.next_offset == end,
            "HFS compressed file's chunks do not end where its table says"
        );
        Ok(())
    }

    fn decode_next(&mut self) -> Result<()> {
        let (start, length) = self.chunk_span()?;
        let fork = self.fork.clone();
        self.reader
            .read_fork(&fork, start, &mut self.packed[..length])?;
        let out = (self.size - self.chunk * COMPRESSED_CHUNK as u64).min(COMPRESSED_CHUNK as u64)
            as usize;
        let packed = &self.packed[..length];
        let stored = match self.table {
            ChunkTable::Lzvn => packed[0] == LZVN_STORED_MARKER,
            ChunkTable::Zlib => packed[0] & 0xF == ZLIB_STORED_MARKER,
        };
        if stored {
            ensure!(
                length - 1 == out,
                "HFS stored chunk is not the size of its block"
            );
            self.decoded[..out].copy_from_slice(&packed[1..]);
        } else {
            match self.table {
                ChunkTable::Lzvn => {
                    crate::lzvn::decode(
                        packed,
                        &mut self.decoded[..out],
                        0,
                        crate::lzvn::Tail::Ignored,
                    )?;
                }
                ChunkTable::Zlib => {
                    inflate_zlib(packed, &mut self.decoded[..out], ZlibEnd::Padded)?
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

impl Read for Compressed<'_, '_> {
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

fn inline_data(attribute: &[u8], header: &Decmpfs) -> Result<Vec<u8>> {
    let payload = &attribute[DECMPFS_HEADER..];
    let bound = |ratio: u64| {
        ensure!(
            header.size <= payload.len() as u64 * ratio,
            "HFS compressed file declares more bytes than its data can hold"
        );
        Ok(vec![0u8; header.size as usize])
    };
    match header.stored {
        Stored::Inline => Ok(payload[1..].to_vec()),
        Stored::ZlibInline => {
            let mut output = bound(DEFLATE_EXPANSION)?;
            inflate_zlib(payload, &mut output, ZlibEnd::Exact)?;
            Ok(output)
        }
        Stored::LzvnInline => {
            let mut output = bound(LZVN_EXPANSION)?;
            crate::lzvn::decode(payload, &mut output, 0, crate::lzvn::Tail::Ignored)?;
            Ok(output)
        }
        _ => unreachable!(),
    }
}

struct Writer<'o> {
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    leaders: HashMap<u32, String>,
    folders: HashMap<u32, (String, usize)>,
    folder_bytes: usize,
    inodes: HashMap<u32, usize>,
    names: HashSet<u64>,
    name_keys: std::hash::RandomState,
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }

    fn folder_path(&mut self, catalog: &Catalog, id: u32) -> Result<String> {
        let mut chain = Vec::new();
        let mut at = id;
        while !self.folders.contains_key(&at) {
            ensure!(
                chain.len() <= MAX_DEPTH,
                "HFS folders nest deeper than {MAX_DEPTH} or form a loop"
            );
            let index = *catalog.index.get(&at).with_context(|| {
                format!("HFS entry lies in folder {at}, which the catalog does not hold")
            })?;
            let item = &catalog.items[index];
            ensure!(
                item.kind == Kind::Folder,
                "HFS entry lies inside {:?}, which is not a folder",
                catalog.name(item)
            );
            chain.push(index);
            if item.parent == ROOT_PARENT {
                ensure!(
                    item.id == ROOT_FOLDER,
                    "HFS entry {:?} lies outside the root folder",
                    catalog.name(item)
                );
                break;
            }
            at = item.parent;
        }
        let (mut path, mut depth) = match self.folders.get(&at) {
            Some((path, depth)) => (Some(path.clone()), *depth),
            None => (None, 0),
        };
        for &index in chain.iter().rev() {
            let item = &catalog.items[index];
            let joined = match &path {
                Some(parent) => {
                    depth += 1;
                    format!("{parent}/{}", catalog.name(item))
                }
                None => catalog.name(item).to_owned(),
            };
            ensure!(
                depth <= MAX_DEPTH,
                "HFS folders nest deeper than {MAX_DEPTH} or form a loop"
            );
            self.folder_bytes += joined.len();
            ensure!(
                self.folder_bytes <= MAX_NAMES,
                "HFS folder paths exceed 64 MiB"
            );
            self.folders.insert(item.id, (joined.clone(), depth));
            path = Some(joined);
        }
        path.context("HFS root folder has no path")
    }

    fn path(&mut self, catalog: &Catalog, item: &Item) -> Result<String> {
        if item.parent == ROOT_PARENT {
            ensure!(
                item.id == ROOT_FOLDER && item.kind == Kind::Folder,
                "HFS entry {:?} lies outside the root folder",
                catalog.name(item)
            );
            return Ok(catalog.name(item).to_owned());
        }
        ensure!(
            self.names.insert(std::hash::BuildHasher::hash_one(
                &self.name_keys,
                (item.parent, catalog.name(item))
            )),
            "HFS folder {} holds two entries named {:?}",
            item.parent,
            catalog.name(item)
        );
        if item.kind == Kind::Folder {
            return self.folder_path(catalog, item.id);
        }
        let parent = self.folder_path(catalog, item.parent)?;
        Ok(format!("{parent}/{}", catalog.name(item)))
    }
}

// EXTERNAL(7-Zip Windows/FileDir SetFileAttrib_PosixHighDetect): only a regular file's or folder's own mode is
// applied, and a folder always keeps its owner's read, write and search rights.
fn file_mode(mode: u16) -> u32 {
    if mode & TYPE_MASK == REGULAR {
        u32::from(mode) & 0o777
    } else {
        DEFAULT_FILE_MODE
    }
}

fn folder_mode(mode: u16) -> u32 {
    if mode & TYPE_MASK == DIRECTORY {
        (u32::from(mode) | 0o700) & 0o777
    } else {
        DEFAULT_FOLDER_MODE
    }
}

struct Source<'a, 'v> {
    reader: &'a mut Reader<'v>,
    attributes: &'a Attributes,
}

fn attribute_node(volume: &mut Source, at: &AttributeAt) -> Result<(Vec<u8>, usize)> {
    let fork = volume.attributes.fork.clone();
    let mut node = vec![0u8; 1 << volume.attributes.node_log];
    volume.reader.read_fork(
        &fork,
        u64::from(at.node) << volume.attributes.node_log,
        &mut node,
    )?;
    let descriptor = node_descriptor(&node)?;
    ensure!(
        usize::from(at.record) < descriptor.records,
        "HFS attributes file changed while it was read"
    );
    Ok((node, usize::from(at.record)))
}

fn restore_attributes(
    volume: &mut Source,
    owner: u32,
    path: &str,
    tree: &mut tree::Tree,
    mut target: Option<tree::Owner>,
) -> Result<()> {
    let writes = target.is_some() && tree.writes_attributes();
    let restored = &volume.attributes.restored;
    let start = restored.partition_point(|&(id, _)| id < owner);
    let end = restored.partition_point(|&(id, _)| id <= owner);
    for (_, at) in &restored[start..end] {
        tree.check()?;
        let (node, index) = attribute_node(volume, at)?;
        let bytes = record(&node, index);
        let attribute = attribute_record(bytes)?;
        let name = utf16_name(&bytes[attribute.name.clone()]);
        ensure!(
            attribute.id == owner && name.is_some(),
            "HFS attributes file changed while it was read"
        );
        let name = name.unwrap();
        ensure!(
            name != DECMPFS && name != RESOURCE_FORK_ATTRIBUTE && crate::xattr::is_restored(&name),
            "HFS attributes file changed while it was read"
        );
        let value = match attribute.value {
            AttributeValue::Inline(range) => bytes[range].to_vec(),
            AttributeValue::Fork(data) => {
                let fork = Fork {
                    size: data.size,
                    blocks: data.blocks,
                    extents: used(&data.extents).collect(),
                };
                ensure!(
                    block_sum(&fork.extents) == u64::from(fork.blocks),
                    "HFS entry {path:?} has an extended attribute {name:?} whose extents do not add up to its blocks"
                );
                volume
                    .reader
                    .check_fork(&fork, &format!("attribute {name:?} of {path:?}"))?;
                if !writes {
                    continue;
                }
                let size = fork.size;
                let stream = ForkStream {
                    reader: &mut *volume.reader,
                    fork,
                    position: 0,
                };
                crate::xattr::read_value(stream, size, &name)
                    .with_context(|| format!("HFS entry {path:?}"))?
            }
            AttributeValue::Unsupported => bail!("HFS attributes file changed while it was read"),
        };
        if let Some(target) = &mut target {
            tree.set_attribute(target, &name, value)?;
        }
    }
    Ok(())
}

fn read_attribute(volume: &mut Source, at: &AttributeAt, id: u32) -> Result<Vec<u8>> {
    let (node, index) = attribute_node(volume, at)?;
    let bytes = record(&node, index);
    let attribute = attribute_record(bytes)?;
    let AttributeValue::Inline(range) = attribute.value else {
        bail!("HFS attributes file changed while it was read");
    };
    ensure!(
        attribute.id == id && utf16_name(&bytes[attribute.name]).as_deref() == Some(DECMPFS),
        "HFS attributes file changed while it was read"
    );
    Ok(bytes[range].to_vec())
}

fn content<'r, 'v>(
    volume: &'r mut Source<'_, 'v>,
    item: &Item,
    name: &str,
) -> Result<(u64, Box<dyn Read + 'r>)>
where
    'v: 'r,
{
    let block_log = volume.reader.header.block_log;
    let Some(at) = volume.attributes.compressed.get(&item.id) else {
        ensure!(
            item.resource.size == 0 && item.resource.blocks == 0,
            "HFS file {name:?} has a resource fork, which 7-Zip writes as {name:?}:rsrc and Zipper does not write; {BACKEND_HINT}"
        );
        exact_blocks(&item.data, block_log, &format!("file {name:?}"))?;
        let stream = ForkStream {
            reader: &mut *volume.reader,
            fork: item.data.clone(),
            position: 0,
        };
        return Ok((item.data.size, Box::new(stream)));
    };
    let attribute = read_attribute(volume, at, item.id)?;
    let header = decmpfs(&attribute)?;
    let fork_table = match header.stored {
        Stored::ZlibFork => Some(ChunkTable::Zlib),
        Stored::LzvnFork => Some(ChunkTable::Lzvn),
        Stored::Unsupported(method) => {
            bail!(
                "HFS file {name:?} is compressed with method {method}, which 7-Zip does not decode; {BACKEND_HINT}"
            )
        }
        _ => None,
    };
    let Some(table) = fork_table else {
        ensure!(
            item.resource.size == 0 && item.resource.blocks == 0,
            "HFS file {name:?} has a resource fork, which 7-Zip writes as {name:?}:rsrc and Zipper does not write; {BACKEND_HINT}"
        );
        let data = inline_data(&attribute, &header)?;
        return Ok((header.size, Box::new(std::io::Cursor::new(data))));
    };
    let stream = Compressed::open(
        &mut *volume.reader,
        item.resource.clone(),
        table,
        header.size,
    )?;
    Ok((header.size, Box::new(stream)))
}

fn link_target(mut data: Box<dyn Read + '_>, size: u64, name: &str) -> Result<String> {
    ensure!(
        size <= crate::MAX_LINK_TARGET,
        "HFS link {name:?} has a target longer than {} bytes",
        crate::MAX_LINK_TARGET
    );
    let mut target = Vec::with_capacity(size as usize);
    data.read_to_end(&mut target)?;
    ensure!(target.len() as u64 == size, "truncated HFS link {name:?}");
    String::from_utf8(target)
        .with_context(|| format!("HFS link {name:?} has a target that is not UTF-8"))
}

fn private_folder(catalog: &Catalog) -> Option<u32> {
    catalog
        .items
        .iter()
        .find(|item| {
            item.parent == ROOT_FOLDER
                && item.kind == Kind::Folder
                && catalog.name(item) == PRIVATE_DATA_NAME
        })
        .map(|item| item.id)
}

fn inodes(catalog: &Catalog) -> HashMap<u32, usize> {
    let Some(private) = private_folder(catalog) else {
        return HashMap::new();
    };
    catalog
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.parent == private && item.kind != Kind::Folder)
        .filter_map(|(index, item)| {
            let digits = catalog.name(item).strip_prefix(INODE_PREFIX)?;
            let number = digits
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| digits.parse().ok())??;
            Some((number, index))
        })
        .collect()
}

fn emit(
    reader: &mut Reader,
    catalog: &Catalog,
    attributes: &Attributes,
    tree: &mut tree::Tree,
    options: crate::native_cpio::ReadOptions,
) -> Result<()> {
    let mut writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        leaders: HashMap::new(),
        folders: HashMap::new(),
        folder_bytes: 0,
        inodes: inodes(catalog),
        names: HashSet::new(),
        name_keys: std::hash::RandomState::new(),
    };
    let inode_numbers: HashMap<u32, u32> = writer
        .inodes
        .iter()
        .map(|(&number, &index)| (catalog.items[index].id, number))
        .collect();
    let mut volume = Source { reader, attributes };
    for item in &catalog.items {
        tree.check()?;
        let path = writer.path(catalog, item)?;
        let time = hfs_time(item.modified);
        let (source, group) = match item.kind {
            Kind::Folder => {
                let placed = tree.placed_directory(&path, folder_mode(item.mode), time)?;
                let target = placed.as_deref().map(tree::Owner::Folder);
                restore_attributes(&mut volume, item.id, &path, tree, target)?;
                continue;
            }
            Kind::HardLink(number) => {
                let index = *writer.inodes.get(&number).with_context(|| {
                    format!("HFS hard link {path:?} names inode {number}, which the volume does not hold")
                })?;
                (&catalog.items[index], Some(number))
            }
            _ => (item, inode_numbers.get(&item.id).copied()),
        };
        if source.kind == Kind::Symlink {
            let (size, data) = content(&mut volume, source, &path)?;
            let target = link_target(data, size, &path)?;
            tree.check()?;
            let link = tree.placed_symlink(&path, target)?;
            restore_attributes(
                &mut volume,
                source.id,
                &path,
                tree,
                link.map(tree::Owner::Link),
            )?;
            continue;
        }
        ensure!(
            source.kind == Kind::File,
            "HFS hard link {path:?} names another hard link"
        );
        if let Some(leader) = group.and_then(|group| writer.leaders.get(&group))
            && writer.writes(&path)
        {
            let leader = leader.clone();
            tree.hard_link(&path, &leader)?;
            continue;
        }
        let (size, mut data) = content(&mut volume, source, &path)?;
        let mut written = tree.write_file(
            &path,
            size,
            file_mode(source.mode),
            hfs_time(source.modified),
            &mut data,
        )?;
        let mut rest = [0u8; 1];
        ensure!(
            data.read(&mut rest)? == 0,
            "HFS file {path:?} decodes to more than its declared size"
        );
        drop(data);
        let target = written.as_mut().map(tree::Owner::File);
        restore_attributes(&mut volume, source.id, &path, tree, target)?;
        tree.finish_file(written)?;
        if let Some(group) = group
            && writer.writes(&path)
        {
            writer.leaders.insert(group, path);
        }
    }
    Ok(())
}

pub(crate) fn read(
    volume: &mut dyn Volume,
    base: u64,
    tree: &mut tree::Tree,
    options: crate::native_cpio::ReadOptions,
) -> Result<()> {
    let mut start = [0u8; HEADER_AREA];
    ensure!(
        volume.size() >= base + HEADER_AREA as u64,
        "HFS volume is shorter than its header"
    );
    volume.read_exact_at(base, &mut start)?;
    let mut base = base;
    if &start[RESERVED..RESERVED + 2] == b"BD" {
        ensure!(
            matches!(&start[..2], b"LK" | [0, 0]),
            "HFS wrapper starts with neither boot blocks nor zeros"
        );
        let wrapper: &[u8; SECTOR] = start[RESERVED..HEADER_AREA].try_into().unwrap();
        base += embedded_volume(wrapper)?;
        ensure!(
            volume.size() >= base + HEADER_AREA as u64,
            "HFS wrapper's volume lies past the end of the image"
        );
        volume.read_exact_at(base, &mut start)?;
    }
    ensure!(
        start[..RESERVED].iter().all(|&byte| byte == 0),
        "HFS volume's first 1024 bytes are not zero"
    );
    let header = volume_header(start[RESERVED..HEADER_AREA].try_into().unwrap())?;
    ensure!(
        base + (u64::from(header.blocks) << header.block_log) <= volume.size(),
        "HFS volume holds more blocks than its image, which 7-Zip reports as an unexpected end"
    );
    let mut reader = Reader {
        volume,
        header,
        base,
    };
    let overflow = load_overflow(&mut reader)?;
    let attributes = load_attributes(&mut reader, &overflow)?;
    let catalog = load_catalog(&mut reader, &overflow)?;
    check_attribute_owners(&catalog, &attributes)?;
    let extents_fork = Fork {
        size: header.extents.size,
        blocks: header.extents.blocks,
        extents: used(&header.extents.extents).collect(),
    };
    let catalog_fork =
        fork_with(&header.catalog, &overflow, false, CATALOG_FILE).unwrap_or_default();
    check_no_overlap(
        &reader,
        &catalog,
        &[&extents_fork, &catalog_fork, &attributes.fork],
    )?;
    tree.check()?;
    emit(&mut reader, &catalog, &attributes, tree, options)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn reference_adler(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u64, 0u64);
        for &byte in bytes {
            a = (a + u64::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16 | a) as u32
    }

    #[test]
    fn adler32_matches_a_byte_at_a_time_reference_at_every_block_edge() {
        for length in [0, 1, 5551, 5552, 5553, 11104, 70000] {
            let bytes: Vec<u8> = (0..length).map(|index| 255 - (index % 7) as u8).collect();
            assert_eq!(adler32(&bytes), reference_adler(&bytes), "{length}");
        }
    }

    #[test]
    fn zlib_checksums_follow_7zips_optional_adler_rule() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"hello hello hello").unwrap();
        let stream = encoder.finish().unwrap();
        let mut output = [0u8; 17];
        inflate_zlib(&stream, &mut output, ZlibEnd::Exact).unwrap();
        assert_eq!(&output, b"hello hello hello");
        let bare = &stream[..stream.len() - 4];
        assert!(inflate_zlib(bare, &mut output, ZlibEnd::Exact).is_ok());
        assert!(inflate_zlib(&stream[..stream.len() - 1], &mut output, ZlibEnd::Exact).is_err());
        let mut wrong = stream.clone();
        *wrong.last_mut().unwrap() ^= 1;
        assert!(inflate_zlib(&wrong, &mut output, ZlibEnd::Padded).is_err());
        let mut padded = stream.clone();
        padded.extend([9, 9]);
        assert!(inflate_zlib(&padded, &mut output, ZlibEnd::Exact).is_err());
        assert!(inflate_zlib(&padded, &mut output, ZlibEnd::Padded).is_ok());
        let mut longer = [0u8; 18];
        assert!(inflate_zlib(&stream, &mut longer, ZlibEnd::Exact).is_err());
        let mut shorter = [0u8; 16];
        assert!(inflate_zlib(&stream, &mut shorter, ZlibEnd::Exact).is_err());
    }

    #[test]
    fn names_show_as_7zip_shows_them() {
        let units =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
        assert_eq!(shown_name(&units("a/b")).unwrap(), "a_b");
        assert_eq!(shown_name(&units("")).unwrap(), "[]");
        assert_eq!(shown_name(&units(" ")).unwrap(), "[]");
        assert_eq!(shown_name(&units("  ")).unwrap(), "  ");
        assert_eq!(shown_name(&units("x")).unwrap(), "x");
        assert_eq!(
            shown_name(&units("\0\0\0\0HFS+ Private Data")).unwrap(),
            "[HFS+ Private Data]"
        );
        assert!(shown_name(&units("a\0b")).is_err());
        assert!(shown_name(&[0xD8, 0x00]).is_err());
    }

    #[test]
    fn modes_apply_only_to_regular_files_and_folders() {
        assert_eq!(file_mode(0o100_755), 0o755);
        assert_eq!(file_mode(0o104_755), 0o755);
        assert_eq!(file_mode(0o644), DEFAULT_FILE_MODE);
        assert_eq!(file_mode(0o010_600), DEFAULT_FILE_MODE);
        assert_eq!(folder_mode(0o040_000), 0o700);
        assert_eq!(folder_mode(0o040_555), 0o755);
        assert_eq!(folder_mode(0o555), DEFAULT_FOLDER_MODE);
    }

    #[test]
    fn hfs_times_count_seconds_from_1904() {
        assert_eq!(hfs_time(0), None);
        assert_eq!(
            hfs_time(HFS_TO_UNIX_SECONDS as u32),
            Some(std::time::UNIX_EPOCH)
        );
        assert_eq!(
            hfs_time(1),
            std::time::UNIX_EPOCH
                .checked_sub(std::time::Duration::from_secs(HFS_TO_UNIX_SECONDS - 1))
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn volume_headers_never_panic_and_keep_their_block_size_and_counts_in_range() {
        let bytes: [u8; SECTOR] = kani::any();
        if let Ok(header) = volume_header(&bytes) {
            assert!((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&header.block_log));
            assert!(header.folders <= MAX_FOLDERS && header.files <= MAX_FILES);
        }
        if let Ok(offset) = embedded_volume(&bytes) {
            assert!(offset <= 0xFFFF * 512 + 0xFFFF * u64::from(u32::MAX));
        }
    }

    #[kani::proof]
    fn tree_headers_count_no_more_nodes_than_their_file_holds() {
        let bytes: [u8; NODE_DESCRIPTOR + HEADER_RECORD] = kani::any();
        let blocks: u32 = kani::any();
        let block_log: u32 = kani::any();
        kani::assume((SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_log));
        if let Ok(header) = tree_header(&bytes, blocks, block_log) {
            assert!((SMALLEST_NODE_LOG..=LARGEST_NODE_LOG).contains(&header.node_log));
            assert!(u64::from(header.nodes) << header.node_log <= u64::from(blocks) << block_log);
            assert!(header.nodes <= MAX_NODES);
        }
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn node_descriptors_keep_every_record_inside_the_node() {
        let bytes: [u8; 40] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let node = &bytes[..length];
        if let Ok(descriptor) = node_descriptor(node) {
            assert!(NODE_DESCRIPTOR + (descriptor.records + 1) * 2 <= length);
            let index: usize = kani::any();
            kani::assume(index < descriptor.records);
            let found = record(node, index);
            assert!(!found.is_empty() && found.len() <= length - NODE_DESCRIPTOR);
        }
    }

    #[kani::proof]
    fn catalog_records_never_panic_and_keep_their_names_inside_themselves() {
        let bytes: [u8; 270] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(Some(entry)) = catalog_record(&bytes[..length]) {
            assert!(entry.name.start == 8 && entry.name.end <= length);
            assert!(entry.name.len() % 2 == 0);
            let body = if entry.folder {
                FOLDER_RECORD
            } else {
                FILE_RECORD
            };
            assert!(entry.name.end + body == length);
        }
    }

    #[kani::proof]
    fn extent_records_never_panic() {
        let bytes: [u8; EXTENT_RECORD + 2] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(Some(found)) = extent_record(&bytes[..length]) {
            assert!(length == EXTENT_RECORD);
            assert!(found.resource == (bytes[2] == RESOURCE_FORK));
        }
    }

    #[kani::proof]
    fn attribute_records_keep_their_value_inside_the_record() {
        let bytes: [u8; 120] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(attribute) = attribute_record(&bytes[..length]) {
            assert!(attribute.name.end <= length);
            if let AttributeValue::Inline(range) = attribute.value {
                assert!(range.start >= attribute.name.end && range.end <= length);
            }
        }
    }

    #[kani::proof]
    fn decmpfs_headers_never_panic_and_store_inline_data_of_their_declared_size() {
        let bytes: [u8; 24] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(header) = decmpfs(&bytes[..length]) {
            assert!(length >= DECMPFS_HEADER);
            if header.stored == Stored::Inline {
                assert!(header.size + 1 + DECMPFS_HEADER as u64 == length as u64);
            }
            if matches!(header.stored, Stored::ZlibFork | Stored::LzvnFork) {
                assert!(length == DECMPFS_HEADER);
            }
        }
    }
}
