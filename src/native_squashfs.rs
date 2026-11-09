//! In-process extraction of SquashFS 4.0 images, published only after the
//! whole image reads cleanly. See `docs/reference/squashfs-reader.md` for
//! what it accepts and restores.
use crate::hfs::Volume;
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, SystemTime},
};

pub(crate) const SUPERBLOCK: usize = 96;
const MAGIC: &[u8; 4] = b"hsqs";
// EXTERNAL(7-Zip SquashfsHandler): 7-Zip also opens big-endian images and the swapped LZMA
// magics some vendors wrote.
const OTHER_MAGICS: [&[u8; 4]; 3] = [b"sqsh", b"shsq", b"qshs"];
const MAJOR: u16 = 4;
const SMALLEST_BLOCK_LOG: u16 = 12;
const LARGEST_BLOCK_LOG: u16 = 20;
pub(crate) const METADATA_BLOCK: usize = 8192;
const METADATA_UNCOMPRESSED: u16 = 0x8000;
const METADATA_LENGTH: u16 = 0x7FFF;
const DATA_UNCOMPRESSED: u32 = 1 << 24;
const DATA_LENGTH: u32 = DATA_UNCOMPRESSED - 1;
const NO_FRAGMENT: u32 = u32::MAX;
const LARGEST_IMAGE: u64 = 1 << 62;
const FRAGMENT_ENTRY: usize = 16;
const FRAGMENTS_PER_BLOCK: u64 = (METADATA_BLOCK / FRAGMENT_ENTRY) as u64;
const ID_ENTRY: u64 = 4;
const TABLE_POINTER: usize = 8;
pub(crate) const BASE_INODE: usize = 16;
pub(crate) const LONGEST_INODE_BODY: usize = 40;
pub(crate) const DIRECTORY_HEADER: usize = 12;
pub(crate) const DIRECTORY_ENTRY: usize = 8;
const DIRECTORY_INDEX: usize = 12;
const DIRECTORY_RUN: u32 = 256;
const LONGEST_NAME: usize = 256;
const LISTING_DOTS: u32 = 3;
const LONGEST_LINK: u32 = 4096;
const MAX_INODES: u32 = 1_000_001;
const CACHED_BLOCKS: usize = 64;
const MAX_LISTING_BLOCKS: usize = 1 << 20;
const DEFAULT_FILE_MODE: u32 = 0o666;
const XZ_DICTIONARY_FLOOR: u64 = METADATA_BLOCK as u64;
const LZMA_HEADER: usize = 13;

const DIRECTORY: u16 = 1;
const EXTENDED: u16 = 7;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Gzip,
    Lzma,
    Lzo,
    Xz,
    Lz4,
    Zstd,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Superblock {
    pub(crate) inode_count: u32,
    pub(crate) block_size: u32,
    pub(crate) fragment_count: u32,
    pub(crate) method: Method,
    pub(crate) ids: u16,
    pub(crate) root: u64,
    pub(crate) bytes_used: u64,
    pub(crate) id_table: u64,
    pub(crate) inode_table: u64,
    pub(crate) directory_table: u64,
    pub(crate) fragment_table: u64,
}

// SPEC(squashfs-tools squashfs_fs.h, struct squashfs_super_block): 96 little-endian bytes that
// give the counts, the block size and the start of each table.
pub(crate) fn superblock(bytes: &[u8; SUPERBLOCK]) -> Field<Superblock> {
    field_check(&bytes[..4] == MAGIC, "not a little-endian SquashFS image")?;
    field_check(le16(bytes, 28) == MAJOR, "SquashFS image is not version 4")?;
    let block_size = le32(bytes, 12);
    let block_log = le16(bytes, 22);
    field_check(
        (SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG).contains(&block_log)
            && block_size == 1 << block_log,
        "SquashFS block size is not a power of two from 4 KiB to 1 MiB",
    )?;
    let method = match le16(bytes, 20) {
        1 => Method::Gzip,
        2 => Method::Lzma,
        3 => Method::Lzo,
        4 => Method::Xz,
        5 => Method::Lz4,
        6 => Method::Zstd,
        _ => return Err(BadField("SquashFS image names an unknown compressor")),
    };
    let inode_count = le32(bytes, 4);
    field_check(
        (1..=MAX_INODES).contains(&inode_count),
        "SquashFS image holds no inodes or more than the entry limit",
    )?;
    let fragment_count = le32(bytes, 16);
    field_check(
        fragment_count <= inode_count,
        "SquashFS image has more fragments than inodes",
    )?;
    let ids = le16(bytes, 26);
    field_check(ids > 0, "SquashFS image has no owner identifiers")?;
    let superblock = Superblock {
        inode_count,
        block_size,
        fragment_count,
        method,
        ids,
        root: le64(bytes, 32),
        bytes_used: le64(bytes, 40),
        id_table: le64(bytes, 48),
        inode_table: le64(bytes, 64),
        directory_table: le64(bytes, 72),
        fragment_table: le64(bytes, 80),
    };
    let end = superblock.bytes_used;
    field_check(
        (SUPERBLOCK as u64..=LARGEST_IMAGE).contains(&end),
        "SquashFS image size is out of range",
    )?;
    field_check(
        SUPERBLOCK as u64 <= superblock.inode_table
            && superblock.inode_table < superblock.directory_table
            && superblock.directory_table <= superblock.id_table
            && superblock.id_table < end,
        "SquashFS tables are out of order or past the end of the image",
    )?;
    field_check(
        (superblock.directory_table..=end).contains(&superblock.fragment_table),
        "SquashFS fragment table lies outside the image",
    )?;
    let inode_span = superblock.directory_table - superblock.inode_table;
    field_check(
        superblock.root >> 16 < inode_span && (superblock.root & 0xFFFF) < METADATA_BLOCK as u64,
        "SquashFS root inode lies outside the inode table",
    )?;
    Ok(superblock)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Directory {
        start: u32,
        offset: u16,
        listing: u32,
        parent: u32,
        indexes: u16,
    },
    File {
        start: u64,
        size: u64,
        fragment: u32,
        offset: u32,
    },
    Symlink {
        target: u32,
    },
    Special,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Inode {
    pub(crate) kind: u16,
    pub(crate) mode: u16,
    pub(crate) modified: u32,
    pub(crate) links: u32,
    pub(crate) body: Body,
    pub(crate) extended: bool,
}

// SPEC(squashfs-tools squashfs_fs.h, struct squashfs_*_inode_header): the fixed bytes that
// follow the 16-byte base header for each inode type, basic types 1 to 7 and extended 8 to 14.
pub(crate) fn body_length(kind: u16) -> Field<usize> {
    Ok(match kind {
        1 => 16,
        2 => 16,
        3..=5 => 8,
        6 | 7 => 4,
        8 => 24,
        9 => LONGEST_INODE_BODY,
        10 => 8,
        11 | 12 => 12,
        13 | 14 => 8,
        _ => return Err(BadField("SquashFS inode has an unknown type")),
    })
}

pub(crate) fn inode(base: &[u8; BASE_INODE], body: &[u8]) -> Field<Inode> {
    let kind = le16(base, 0);
    field_check(
        body.len() == body_length(kind)?,
        "SquashFS inode body has the wrong length",
    )?;
    let extended = kind > EXTENDED;
    let basic = if extended { kind - EXTENDED } else { kind };
    let (links, body) = match kind {
        1 => (
            le32(body, 4),
            Body::Directory {
                start: le32(body, 0),
                offset: le16(body, 10),
                listing: u32::from(le16(body, 8)),
                parent: le32(body, 12),
                indexes: 0,
            },
        ),
        8 => (
            le32(body, 0),
            Body::Directory {
                start: le32(body, 8),
                offset: le16(body, 18),
                listing: le32(body, 4),
                parent: le32(body, 12),
                indexes: le16(body, 16),
            },
        ),
        2 => (
            1,
            Body::File {
                start: u64::from(le32(body, 0)),
                size: u64::from(le32(body, 12)),
                fragment: le32(body, 4),
                offset: le32(body, 8),
            },
        ),
        9 => (
            le32(body, 24),
            Body::File {
                start: le64(body, 0),
                size: le64(body, 8),
                fragment: le32(body, 28),
                offset: le32(body, 32),
            },
        ),
        3 | 10 => (
            le32(body, 0),
            Body::Symlink {
                target: le32(body, 4),
            },
        ),
        _ => (le32(body, 0), Body::Special),
    };
    match body {
        Body::Directory {
            offset, listing, ..
        } => field_check(
            usize::from(offset) < METADATA_BLOCK && listing >= LISTING_DOTS,
            "SquashFS folder listing is out of range",
        )?,
        Body::File { size, .. } => field_check(
            size <= LARGEST_IMAGE,
            "SquashFS file is larger than the format allows",
        )?,
        Body::Symlink { target } => field_check(
            (1..=LONGEST_LINK).contains(&target),
            "SquashFS link target is empty or too long",
        )?,
        Body::Special => {}
    }
    Ok(Inode {
        kind: basic,
        mode: le16(base, 2),
        modified: le32(base, 8),
        links,
        body,
        extended,
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RunHeader {
    pub(crate) entries: u32,
    pub(crate) block: u32,
}

// SPEC(squashfs-tools squashfs_fs.h, struct squashfs_dir_header): a run of up to 256 entries
// whose inodes share one inode table block, with the count stored less one.
pub(crate) fn run_header(bytes: &[u8; DIRECTORY_HEADER]) -> Field<RunHeader> {
    let entries = le32(bytes, 0)
        .checked_add(1)
        .filter(|entries| *entries <= DIRECTORY_RUN)
        .ok_or(BadField("SquashFS folder run holds more than 256 entries"))?;
    Ok(RunHeader {
        entries,
        block: le32(bytes, 4),
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DirectoryEntry {
    pub(crate) offset: u16,
    pub(crate) name: usize,
}

// SPEC(squashfs-tools squashfs_fs.h, struct squashfs_dir_entry): the inode's offset in its block,
// its number as a signed difference from the run's, its basic type and the name length less one.
// EXTERNAL(7-Zip SquashfsHandler): 7-Zip reads the type and number from the inode, not the entry.
pub(crate) fn directory_entry(bytes: &[u8; DIRECTORY_ENTRY]) -> Field<DirectoryEntry> {
    let offset = le16(bytes, 0);
    field_check(
        usize::from(offset) < METADATA_BLOCK,
        "SquashFS folder entry points past its inode block",
    )?;
    Ok(DirectoryEntry {
        offset,
        name: usize::from(le16(bytes, 6)) + 1,
    })
}

pub(crate) fn file_blocks(size: u64, block_log: u32, fragment: u32) -> u64 {
    if fragment == NO_FRAGMENT {
        size.div_ceil(1 << block_log)
    } else {
        size >> block_log
    }
}

pub(crate) fn data_block(word: u32, block_size: u32) -> Field<Option<(u32, bool)>> {
    if word == 0 {
        return Ok(None);
    }
    let length = word & DATA_LENGTH;
    field_check(
        word & !(DATA_LENGTH | DATA_UNCOMPRESSED) == 0 && (1..=block_size).contains(&length),
        "SquashFS data block is larger than the block size",
    )?;
    Ok(Some((length, word & DATA_UNCOMPRESSED != 0)))
}

fn decode_exact(
    method: Method,
    dictionary_limit: u64,
    packed: &[u8],
    output: &mut [u8],
) -> Result<usize> {
    match method {
        Method::Gzip => {
            let mut inflater = flate2::Decompress::new(true);
            let status = inflater
                .decompress(packed, output, flate2::FlushDecompress::Finish)
                .context("SquashFS zlib block is damaged")?;
            ensure!(
                status == flate2::Status::StreamEnd && inflater.total_in() == packed.len() as u64,
                "SquashFS zlib block decodes past its size or ends early"
            );
            Ok(inflater.total_out() as usize)
        }
        Method::Lzo => Ok(crate::lzo::decode(packed, output)
            .map_err(|error| anyhow::anyhow!("SquashFS LZO block is damaged: {error}"))?),
        Method::Lz4 => {
            let mut decoded = Vec::with_capacity(output.len());
            crate::lz4::decode_block(packed, &mut decoded, output.len())
                .context("SquashFS LZ4 block is damaged or decodes past its size")?;
            output[..decoded.len()].copy_from_slice(&decoded);
            Ok(decoded.len())
        }
        Method::Zstd => {
            let mut decoder = zstd::bulk::Decompressor::new()?;
            decoder
                .decompress_to_buffer(packed, output)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "SquashFS zstd block is damaged or decodes past its size: {error}"
                    )
                })
        }
        Method::Xz => {
            let reader = crate::lzma::XzReader::new(packed)
                .single_stream()
                .with_dictionary_limit(dictionary_limit);
            fill(reader, output, "xz")
        }
        Method::Lzma => {
            let header: [u8; LZMA_HEADER] = packed
                .get(..LZMA_HEADER)
                .and_then(|header| header.try_into().ok())
                .context("SquashFS LZMA block is shorter than its header")?;
            let header = crate::lzma::lzma_file_header(header)
                .map_err(|error| anyhow::anyhow!("SquashFS LZMA block is damaged: {error}"))?;
            let size = header
                .size
                .filter(|size| *size <= output.len() as u64)
                .context("SquashFS LZMA block declares a size past its block")?;
            let reader = crate::lzma::RawLzmaReader::new(
                &packed[LZMA_HEADER..],
                header.properties,
                header.dictionary,
                size,
            )
            .requiring_a_clean_end();
            fill(reader, &mut output[..size as usize], "LZMA")
        }
    }
}

fn fill(mut reader: impl Read, output: &mut [u8], name: &str) -> Result<usize> {
    let mut filled = 0;
    while filled < output.len() {
        let read = reader
            .read(&mut output[filled..])
            .with_context(|| format!("SquashFS {name} block is damaged"))?;
        if read == 0 {
            return Ok(filled);
        }
        filled += read;
    }
    let mut rest = [0u8; 1];
    ensure!(
        reader
            .read(&mut rest)
            .with_context(|| format!("SquashFS {name} block is damaged"))?
            == 0,
        "SquashFS {name} block decodes past its size"
    );
    Ok(filled)
}

struct Cached {
    at: u64,
    next: u64,
    data: Vec<u8>,
    used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cursor {
    block: u64,
    offset: usize,
}

impl Cursor {
    fn of(reference: u64) -> Cursor {
        Cursor {
            block: reference >> 16,
            offset: (reference & 0xFFFF) as usize,
        }
    }

    fn reference(self) -> u64 {
        (self.block << 16) | self.offset as u64
    }
}

struct Metadata {
    name: &'static str,
    start: u64,
    end: u64,
    cache: Vec<Cached>,
    clock: u64,
}

struct Disk<'v> {
    volume: &'v mut dyn Volume,
    superblock: Superblock,
    packed: Vec<u8>,
}

impl Disk<'_> {
    fn read_at(&mut self, offset: u64, length: usize) -> Result<&[u8]> {
        ensure!(
            offset
                .checked_add(length as u64)
                .is_some_and(|end| end <= self.volume.size()),
            "SquashFS image is truncated"
        );
        self.packed.resize(length, 0);
        self.volume.read_exact_at(offset, &mut self.packed)?;
        Ok(&self.packed)
    }

    fn dictionary_limit(&self) -> u64 {
        u64::from(self.superblock.block_size).max(XZ_DICTIONARY_FLOOR)
    }
}

impl Metadata {
    fn new(name: &'static str, start: u64, end: u64) -> Metadata {
        Metadata {
            name,
            start,
            end,
            cache: Vec::new(),
            clock: 0,
        }
    }

    fn block(&mut self, disk: &mut Disk, at: u64) -> Result<usize> {
        self.clock += 1;
        if let Some(index) = self.cache.iter().position(|cached| cached.at == at) {
            self.cache[index].used = self.clock;
            return Ok(index);
        }
        let name = self.name;
        let position = self
            .start
            .checked_add(at)
            .filter(|position| position.checked_add(2).is_some_and(|end| end <= self.end))
            .with_context(|| format!("SquashFS {name} runs past its table"))?;
        let header = le16(disk.read_at(position, 2)?, 0);
        let length = usize::from(header & METADATA_LENGTH);
        ensure!(
            (1..=METADATA_BLOCK).contains(&length) && position + 2 + length as u64 <= self.end,
            "SquashFS {name} block has a bad length"
        );
        let method = disk.superblock.method;
        let limit = disk.dictionary_limit();
        let packed = disk.read_at(position + 2, length)?;
        let mut data = vec![0u8; METADATA_BLOCK];
        let decoded = if header & METADATA_UNCOMPRESSED != 0 {
            data[..length].copy_from_slice(packed);
            length
        } else {
            decode_exact(method, limit, packed, &mut data)
                .with_context(|| format!("SquashFS {name} block is damaged"))?
        };
        data.truncate(decoded);
        let cached = Cached {
            at,
            next: at + 2 + length as u64,
            data,
            used: self.clock,
        };
        if self.cache.len() < CACHED_BLOCKS {
            self.cache.push(cached);
            return Ok(self.cache.len() - 1);
        }
        let oldest = (0..self.cache.len())
            .min_by_key(|index| self.cache[*index].used)
            .unwrap();
        self.cache[oldest] = cached;
        Ok(oldest)
    }

    fn visit(
        &mut self,
        disk: &mut Disk,
        cursor: &mut Cursor,
        mut length: usize,
        mut take: impl FnMut(&[u8]),
    ) -> Result<()> {
        while length > 0 {
            let index = self.block(disk, cursor.block)?;
            let cached = &self.cache[index];
            ensure!(
                cursor.offset <= cached.data.len(),
                "SquashFS {} reference lies past its block",
                self.name
            );
            if cursor.offset == cached.data.len() {
                *cursor = Cursor {
                    block: cached.next,
                    offset: 0,
                };
                continue;
            }
            let piece = length.min(cached.data.len() - cursor.offset);
            take(&cached.data[cursor.offset..cursor.offset + piece]);
            cursor.offset += piece;
            length -= piece;
        }
        Ok(())
    }

    fn read(&mut self, disk: &mut Disk, cursor: &mut Cursor, out: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        self.visit(disk, cursor, out.len(), |piece| {
            out[filled..filled + piece.len()].copy_from_slice(piece);
            filled += piece.len();
        })
    }

    fn skip(&mut self, disk: &mut Disk, cursor: &mut Cursor, mut length: u64) -> Result<()> {
        while length > 0 {
            let piece = length.min(METADATA_BLOCK as u64) as usize;
            self.visit(disk, cursor, piece, |_| {})?;
            length -= piece as u64;
        }
        Ok(())
    }

    fn check_lengths(
        &mut self,
        disk: &mut Disk,
        blocks: &[u64],
        mut left: u64,
        what: &str,
    ) -> Result<()> {
        for (index, at) in blocks.iter().enumerate() {
            let piece = left.min(METADATA_BLOCK as u64);
            let block = self.block(disk, *at)?;
            ensure!(
                self.cache[block].data.len() as u64 == piece,
                "SquashFS {what} table block {index} has the wrong length"
            );
            left -= piece;
        }
        Ok(())
    }

    fn past_a_full_block(&mut self, disk: &mut Disk, cursor: &mut Cursor) -> Result<()> {
        let index = self.block(disk, cursor.block)?;
        let cached = &self.cache[index];
        if cursor.offset == cached.data.len() {
            *cursor = Cursor {
                block: cached.next,
                offset: 0,
            };
        }
        Ok(())
    }

    fn at_end(&mut self, disk: &mut Disk, cursor: Cursor) -> Result<bool> {
        let index = self.block(disk, cursor.block)?;
        let cached = &self.cache[index];
        Ok(cursor.offset == cached.data.len() && self.start + cached.next == self.end)
    }
}

struct Fragment {
    index: u32,
    data: Vec<u8>,
}

struct Image<'v> {
    disk: Disk<'v>,
    inodes: Metadata,
    listings: Metadata,
    tables: Metadata,
    fragment_blocks: Vec<u64>,
    starts: Vec<u64>,
    listing_blocks: Vec<(u64, usize)>,
    fragment: Option<Fragment>,
}

fn read_index(disk: &mut Disk, at: u64, count: u64, what: &str) -> Result<Vec<u64>> {
    let length = usize::try_from(count * TABLE_POINTER as u64)?;
    let bytes = disk
        .read_at(at, length)
        .with_context(|| format!("SquashFS {what} index lies past the image"))?;
    let pointers: Vec<u64> = bytes
        .chunks_exact(TABLE_POINTER)
        .map(|pointer| le64(pointer, 0))
        .collect();
    ensure!(
        pointers
            .iter()
            .all(|pointer| (SUPERBLOCK as u64..at).contains(pointer)),
        "SquashFS {what} index points outside the image"
    );
    Ok(pointers)
}

impl<'v> Image<'v> {
    fn open(volume: &'v mut dyn Volume) -> Result<Image<'v>> {
        ensure!(
            volume.size() >= SUPERBLOCK as u64,
            "SquashFS image is truncated"
        );
        let mut bytes = [0u8; SUPERBLOCK];
        volume.read_exact_at(0, &mut bytes)?;
        if !native_version(&bytes) {
            bail!(
                "SquashFS image is not version 4.0 in little-endian order, which Zipper does not read in process; {BACKEND_HINT}"
            );
        }
        let superblock = superblock(&bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
        ensure!(
            superblock.bytes_used <= volume.size(),
            "SquashFS image is truncated: it declares {} bytes and holds {}",
            superblock.bytes_used,
            volume.size()
        );
        let mut disk = Disk {
            volume,
            superblock,
            packed: Vec::new(),
        };
        let fragment_blocks = read_index(
            &mut disk,
            superblock.fragment_table,
            u64::from(superblock.fragment_count).div_ceil(FRAGMENTS_PER_BLOCK),
            "fragment",
        )?;
        let id_blocks = read_index(
            &mut disk,
            superblock.id_table,
            (u64::from(superblock.ids) * ID_ENTRY).div_ceil(METADATA_BLOCK as u64),
            "owner",
        )?;
        let mut tables = Metadata::new("table", 0, superblock.bytes_used);
        let fragment_bytes = u64::from(superblock.fragment_count) * FRAGMENT_ENTRY as u64;
        tables.check_lengths(&mut disk, &fragment_blocks, fragment_bytes, "fragment")?;
        let id_bytes = u64::from(superblock.ids) * ID_ENTRY;
        tables.check_lengths(&mut disk, &id_blocks, id_bytes, "owner")?;
        let mut image = Image {
            inodes: Metadata::new(
                "inode table",
                superblock.inode_table,
                superblock.directory_table,
            ),
            listings: Metadata::new(
                "folder table",
                superblock.directory_table,
                superblock.fragment_table,
            ),
            tables,
            disk,
            fragment_blocks,
            starts: Vec::new(),
            listing_blocks: Vec::new(),
            fragment: None,
        };
        image.scan_inodes()?;
        image.scan_listings()?;
        Ok(image)
    }

    fn superblock(&self) -> Superblock {
        self.disk.superblock
    }

    fn read_inode(&mut self, cursor: &mut Cursor) -> Result<Inode> {
        let mut base = [0u8; BASE_INODE];
        self.inodes.read(&mut self.disk, cursor, &mut base)?;
        let length = body_length(le16(&base, 0)).map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut body = [0u8; LONGEST_INODE_BODY];
        self.inodes
            .read(&mut self.disk, cursor, &mut body[..length])?;
        inode(&base, &body[..length]).map_err(|error| anyhow::anyhow!("{error}"))
    }

    fn skip_tail(&mut self, inode: &Inode, cursor: &mut Cursor) -> Result<()> {
        let block_log = self.superblock().block_size.trailing_zeros();
        match inode.body {
            Body::File { size, fragment, .. } => {
                let blocks = file_blocks(size, block_log, fragment);
                self.inodes.skip(&mut self.disk, cursor, blocks * 4)?;
            }
            Body::Symlink { target } => {
                self.inodes
                    .skip(&mut self.disk, cursor, u64::from(target))?;
                if inode.extended {
                    self.inodes.skip(&mut self.disk, cursor, 4)?;
                }
            }
            Body::Directory { indexes, .. } => {
                for _ in 0..indexes {
                    let mut index = [0u8; DIRECTORY_INDEX];
                    self.inodes.read(&mut self.disk, cursor, &mut index)?;
                    let name = u64::from(le32(&index, 8)) + 1;
                    ensure!(
                        name <= LONGEST_NAME as u64,
                        "SquashFS folder index names are longer than 256 bytes"
                    );
                    self.inodes.skip(&mut self.disk, cursor, name)?;
                }
            }
            Body::Special => {}
        }
        Ok(())
    }

    // EXTERNAL(7-Zip SquashfsHandler): 7-Zip parses the whole inode table before it lists a folder,
    // and refuses an image whose table holds another count of inodes than the superblock gives.
    fn scan_inodes(&mut self) -> Result<()> {
        let count = self.superblock().inode_count;
        let mut cursor = Cursor {
            block: 0,
            offset: 0,
        };
        let mut starts = Vec::new();
        loop {
            ensure!(
                starts.len() < count as usize,
                "SquashFS inode table holds more inodes than the superblock counts"
            );
            starts.push(cursor.reference());
            let inode = self.read_inode(&mut cursor)?;
            self.skip_tail(&inode, &mut cursor)?;
            if self.inodes.at_end(&mut self.disk, cursor)? {
                break;
            }
            self.inodes.past_a_full_block(&mut self.disk, &mut cursor)?;
        }
        ensure!(
            starts.len() == count as usize,
            "SquashFS inode table holds {} inodes, not the {count} the superblock counts",
            starts.len()
        );
        self.starts = starts;
        Ok(())
    }

    // EXTERNAL(7-Zip SquashfsHandler): 7-Zip decodes every block from the folder
    // table to the fragment table, and a listing must start inside one of those blocks.
    fn scan_listings(&mut self) -> Result<()> {
        let superblock = self.superblock();
        let span = superblock.fragment_table - superblock.directory_table;
        let mut at = 0;
        while at < span {
            ensure!(
                self.listing_blocks.len() < MAX_LISTING_BLOCKS,
                "SquashFS folder table has more than {MAX_LISTING_BLOCKS} blocks"
            );
            let index = self.listings.block(&mut self.disk, at)?;
            let cached = &self.listings.cache[index];
            self.listing_blocks.push((at, cached.data.len()));
            at = cached.next;
        }
        Ok(())
    }

    fn listing_start(&self, start: u32, offset: u16, path: &str) -> Result<Cursor> {
        let block = u64::from(start);
        let offset = usize::from(offset);
        let found = self
            .listing_blocks
            .binary_search_by_key(&block, |(at, _)| *at)
            .ok()
            .map(|index| self.listing_blocks[index].1);
        ensure!(
            found.is_some_and(|length| offset <= length),
            "SquashFS folder {path:?} starts outside the folder table"
        );
        Ok(Cursor { block, offset })
    }

    fn inode_at(&mut self, reference: u64) -> Result<(Inode, Cursor)> {
        ensure!(
            self.starts.binary_search(&reference).is_ok(),
            "SquashFS reference {reference:#x} does not name the start of an inode"
        );
        let mut cursor = Cursor::of(reference);
        let inode = self.read_inode(&mut cursor)?;
        Ok((inode, cursor))
    }

    fn fragment_entry(&mut self, index: u32) -> Result<(u64, u32, bool)> {
        let superblock = self.superblock();
        ensure!(
            index < superblock.fragment_count,
            "SquashFS file names fragment {index}, past the {} the image holds",
            superblock.fragment_count
        );
        let at = self.fragment_blocks[(u64::from(index) / FRAGMENTS_PER_BLOCK) as usize];
        let mut cursor = Cursor {
            block: at,
            offset: (u64::from(index) % FRAGMENTS_PER_BLOCK) as usize * FRAGMENT_ENTRY,
        };
        let mut entry = [0u8; FRAGMENT_ENTRY];
        self.tables.read(&mut self.disk, &mut cursor, &mut entry)?;
        let start = le64(&entry, 0);
        let (length, stored) = data_block(le32(&entry, 8), superblock.block_size)
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .context("SquashFS fragment block is empty")?;
        ensure!(
            start
                .checked_add(u64::from(length))
                .is_some_and(|end| start >= SUPERBLOCK as u64 && end <= superblock.inode_table),
            "SquashFS fragment block {index} lies outside the data area"
        );
        Ok((start, length, stored))
    }

    fn fragment_data(&mut self, index: u32) -> Result<&[u8]> {
        if self
            .fragment
            .as_ref()
            .is_none_or(|fragment| fragment.index != index)
        {
            let (start, length, stored) = self.fragment_entry(index)?;
            let method = self.superblock().method;
            let limit = self.disk.dictionary_limit();
            let block_size = self.superblock().block_size as usize;
            let packed = self.disk.read_at(start, length as usize)?;
            let data = if stored {
                packed.to_vec()
            } else {
                let mut data = vec![0u8; block_size];
                let decoded = decode_exact(method, limit, packed, &mut data)
                    .with_context(|| format!("SquashFS fragment block {index} is damaged"))?;
                data.truncate(decoded);
                data
            };
            self.fragment = Some(Fragment { index, data });
        }
        Ok(&self.fragment.as_ref().unwrap().data)
    }

    fn read_text(&mut self, cursor: &mut Cursor, length: usize, what: &str) -> Result<String> {
        let mut bytes = vec![0u8; length];
        self.inodes.read(&mut self.disk, cursor, &mut bytes)?;
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("SquashFS {what} is not UTF-8"))
    }
}

struct FileData<'i, 'v> {
    image: &'i mut Image<'v>,
    path: &'i str,
    blocks: Cursor,
    blocks_left: u64,
    position: u64,
    remaining: u64,
    fragment: Option<(u32, u32)>,
    buffer: Vec<u8>,
    taken: usize,
}

impl FileData<'_, '_> {
    fn next_piece(&mut self) -> Result<()> {
        let superblock = self.image.superblock();
        let block_size = u64::from(superblock.block_size);
        let path = self.path;
        self.taken = 0;
        if self.blocks_left > 0 {
            self.blocks_left -= 1;
            let expected = self.remaining.min(block_size) as usize;
            let mut word = [0u8; 4];
            self.image
                .inodes
                .read(&mut self.image.disk, &mut self.blocks, &mut word)?;
            self.buffer.resize(expected, 0);
            let Some((length, stored)) =
                data_block(u32::from_le_bytes(word), superblock.block_size)
                    .map_err(|error| anyhow::anyhow!("{error} in {path:?}"))?
            else {
                self.buffer.fill(0);
                return Ok(());
            };
            let end = self
                .position
                .checked_add(u64::from(length))
                .filter(|end| self.position >= SUPERBLOCK as u64 && *end <= superblock.inode_table)
                .with_context(|| {
                    format!("SquashFS file {path:?} has a block outside the data area")
                })?;
            let method = superblock.method;
            let limit = self.image.disk.dictionary_limit();
            let packed = self.image.disk.read_at(self.position, length as usize)?;
            if stored {
                ensure!(
                    packed.len() == expected,
                    "SquashFS file {path:?} has a stored block of the wrong size"
                );
                self.buffer.copy_from_slice(packed);
            } else {
                let decoded = decode_exact(method, limit, packed, &mut self.buffer)
                    .with_context(|| format!("SquashFS file {path:?} is damaged"))?;
                ensure!(
                    decoded == expected,
                    "SquashFS file {path:?} has a block that decodes to {decoded} bytes, not {expected}"
                );
            }
            self.position = end;
            return Ok(());
        }
        let (index, offset) = self
            .fragment
            .take()
            .with_context(|| format!("SquashFS file {path:?} has no data for its tail"))?;
        let tail = self.remaining as usize;
        let data = self.image.fragment_data(index)?;
        let piece = (offset as usize)
            .checked_add(tail)
            .and_then(|end| data.get(offset as usize..end))
            .with_context(|| {
                format!("SquashFS file {path:?} has a tail past its fragment block")
            })?;
        self.buffer.clear();
        self.buffer.extend_from_slice(piece);
        Ok(())
    }
}

impl Read for FileData<'_, '_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        if self.taken == self.buffer.len() {
            self.next_piece().map_err(std::io::Error::other)?;
        }
        let piece = out
            .len()
            .min(self.buffer.len() - self.taken)
            .min(self.remaining as usize);
        out[..piece].copy_from_slice(&self.buffer[self.taken..self.taken + piece]);
        self.taken += piece;
        self.remaining -= piece as u64;
        Ok(piece)
    }
}

fn file_mode(mode: u16) -> u32 {
    u32::from(mode) & 0o777
}

fn folder_mode(mode: u16) -> u32 {
    (u32::from(mode) | 0o700) & 0o777
}

fn squashfs_time(seconds: u32) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(seconds)))
}

struct Pending {
    path: String,
    reference: u64,
}

struct Writer<'o> {
    selection: crate::Selection<'o>,
    placement: crate::placement::Placement<'o>,
    leaders: HashMap<u64, String>,
    folders: HashSet<u64>,
}

impl Writer<'_> {
    fn writes(&self, name: &str) -> bool {
        self.selection.selects(name) && self.placement.place(name).is_some()
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn read_listing(
    image: &mut Image,
    folder: &Pending,
    inode: &Inode,
    mut entry: impl FnMut(&mut Image, String, u64) -> Result<()>,
) -> Result<()> {
    let Body::Directory {
        start,
        offset,
        listing,
        ..
    } = inode.body
    else {
        bail!("SquashFS entry {:?} is not a folder", folder.path);
    };
    let path = &folder.path;
    let mut cursor = image.listing_start(start, offset, path)?;
    let mut left = u64::from(listing - LISTING_DOTS);
    let mut names = HashSet::new();
    while left > 0 {
        ensure!(
            left >= DIRECTORY_HEADER as u64,
            "SquashFS folder {path:?} ends inside a run header"
        );
        let mut header = [0u8; DIRECTORY_HEADER];
        image
            .listings
            .read(&mut image.disk, &mut cursor, &mut header)?;
        left -= DIRECTORY_HEADER as u64;
        let run = run_header(&header).map_err(|error| anyhow::anyhow!("{error} in {path:?}"))?;
        for _ in 0..run.entries {
            ensure!(
                left >= DIRECTORY_ENTRY as u64,
                "SquashFS folder {path:?} ends inside an entry"
            );
            let mut bytes = [0u8; DIRECTORY_ENTRY];
            image
                .listings
                .read(&mut image.disk, &mut cursor, &mut bytes)?;
            left -= DIRECTORY_ENTRY as u64;
            let found =
                directory_entry(&bytes).map_err(|error| anyhow::anyhow!("{error} in {path:?}"))?;
            ensure!(
                found.name as u64 <= left && found.name <= LONGEST_NAME,
                "SquashFS folder {path:?} has a name past its listing"
            );
            let mut name = vec![0u8; found.name];
            image
                .listings
                .read(&mut image.disk, &mut cursor, &mut name)?;
            left -= found.name as u64;
            let name = String::from_utf8(name).map_err(|_| {
                anyhow::anyhow!("SquashFS folder {path:?} holds a name that is not UTF-8")
            })?;
            ensure!(
                !name.contains('/') && name != "." && name != "..",
                "SquashFS folder {path:?} holds the unsafe name {name:?}"
            );
            ensure!(
                names.insert(name.clone()),
                "SquashFS folder {path:?} holds two entries named {name:?}"
            );
            let reference = (u64::from(run.block) << 16) | u64::from(found.offset);
            entry(image, name, reference)?;
        }
    }
    Ok(())
}

fn emit(image: &mut Image, tree: &mut tree::Tree, options: ReadOptions) -> Result<()> {
    let block_log = image.superblock().block_size.trailing_zeros();
    let mut writer = Writer {
        selection: crate::Selection::new(options.paths, options.include, options.exclude)?,
        placement: crate::placement::Placement::new(options.strip)?,
        leaders: HashMap::new(),
        folders: HashSet::new(),
    };
    let root = image.superblock().root;
    let (root_inode, _) = image.inode_at(root)?;
    ensure!(
        root_inode.kind == DIRECTORY,
        "SquashFS root inode is not a folder"
    );
    writer.folders.insert(root);
    let mut queue = VecDeque::from([Pending {
        path: String::new(),
        reference: root,
    }]);
    while let Some(folder) = queue.pop_front() {
        tree.check()?;
        let (inode, _) = image.inode_at(folder.reference)?;
        read_listing(image, &folder, &inode, |image, name, reference| {
            tree.check()?;
            let path = join(&folder.path, &name);
            let (inode, mut cursor) = image.inode_at(reference)?;
            let modified = squashfs_time(inode.modified);
            match inode.body {
                Body::Directory { .. } => {
                    ensure!(
                        writer.folders.insert(reference),
                        "SquashFS folder {path:?} appears twice or forms a loop"
                    );
                    tree.directory(&path, folder_mode(inode.mode), modified)?;
                    queue.push_back(Pending { path, reference });
                }
                Body::Symlink { target } => {
                    let target = image.read_text(&mut cursor, target as usize, "link target")?;
                    tree.symlink(&path, target)?;
                }
                Body::Special => {
                    tree.file(&path, 0, DEFAULT_FILE_MODE, modified, &mut std::io::empty())?;
                }
                Body::File {
                    start,
                    size,
                    fragment,
                    offset,
                } => {
                    if let Some(leader) = writer.leaders.get(&reference)
                        && writer.writes(&path)
                    {
                        let leader = leader.clone();
                        return tree.hard_link(&path, &leader);
                    }
                    let blocks = file_blocks(size, block_log, fragment);
                    let mut data = FileData {
                        image,
                        path: &path,
                        blocks: cursor,
                        blocks_left: blocks,
                        position: start,
                        remaining: size,
                        fragment: (fragment != NO_FRAGMENT).then_some((fragment, offset)),
                        buffer: Vec::new(),
                        taken: 0,
                    };
                    tree.file(&path, size, file_mode(inode.mode), modified, &mut data)?;
                    ensure!(
                        data.remaining == 0,
                        "SquashFS file {path:?} is shorter than its size"
                    );
                    if inode.links > 1 && writer.writes(&path) {
                        writer.leaders.insert(reference, path);
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn native_version(bytes: &[u8; SUPERBLOCK]) -> bool {
    &bytes[..4] == MAGIC && le16(bytes, 28) == MAJOR
}

pub(crate) fn is_image(start: &[u8]) -> bool {
    start.len() >= SUPERBLOCK
        && (start[..4] == *MAGIC || OTHER_MAGICS.iter().any(|magic| start[..4] == **magic))
}

pub(crate) fn read(
    volume: &mut dyn Volume,
    tree: &mut tree::Tree,
    options: ReadOptions,
) -> Result<()> {
    let mut image = Image::open(volume)?;
    emit(&mut image, tree, options)
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
            "read past the end of the SquashFS image"
        );
        let mut buffer = buffer;
        let mut offset = offset;
        while !buffer.is_empty() {
            let read = self
                .file
                .read_at(buffer, offset)
                .context("cannot read the SquashFS image")?;
            ensure!(read != 0, "truncated SquashFS image");
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
            "read past the end of the SquashFS partition"
        );
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner
            .read_exact(buffer)
            .context("cannot read the SquashFS partition")
    }
}

/// Returns whether the file starts with a SquashFS superblock signature
/// that 7-Zip recognizes, of any version or byte order.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    if file.len()? < SUPERBLOCK as u64 {
        return Ok(false);
    }
    let mut start = [0u8; SUPERBLOCK];
    let mut volume = FileVolume {
        file: &file,
        length: SUPERBLOCK as u64,
    };
    Ok(volume.read_exact_at(0, &mut start).is_ok() && is_image(&start))
}

/// Returns whether the default engine sends this image to 7-Zip: one that
/// is not SquashFS 4.0 in little-endian order.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    let mut start = [0u8; SUPERBLOCK];
    let mut volume = FileVolume {
        file: &file,
        length: file.len()?,
    };
    Ok(volume.read_exact_at(0, &mut start).is_ok() && is_image(&start) && !native_version(&start))
}

pub(crate) fn needs_backend_in<V: Read + Seek>(mut volume: V) -> Result<bool> {
    let mut start = [0u8; SUPERBLOCK];
    volume.seek(SeekFrom::Start(0))?;
    Ok(volume.read_exact(&mut start).is_ok() && is_image(&start) && !native_version(&start))
}

/// Verify and optionally extract a SquashFS 4.0 image held in a bounded
/// view, such as one GPT partition, into a new directory, in the tree
/// `7zz x` writes. Every check of `docs/reference/squashfs-reader.md` holds
/// before anything is published.
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
    tree::extract("SquashFS", archive, destination, options, |tree| {
        read(&mut volume, tree, options)
    })
}

/// Verify and optionally extract a raw SquashFS 4.0 image into a new
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
    tree::extract("SquashFS", path, destination, options, |tree| {
        read(&mut volume, tree, options)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn link_and_inode_limits_match_every_reader() {
        assert_eq!(u64::from(LONGEST_LINK), crate::MAX_LINK_TARGET);
        assert_eq!(MAX_INODES as usize, tree::MAX_ENTRIES + 1);
    }

    #[test]
    fn file_blocks_cover_the_file_or_leave_only_a_tail_for_its_fragment() {
        for block_log in SMALLEST_BLOCK_LOG..=LARGEST_BLOCK_LOG {
            let block = 1u64 << block_log;
            for size in [
                0,
                1,
                block - 1,
                block,
                block + 1,
                7 * block + 3,
                LARGEST_IMAGE,
            ] {
                let whole = file_blocks(size, u32::from(block_log), NO_FRAGMENT);
                assert!(whole * block >= size && whole * block < size + block);
                let leading = file_blocks(size, u32::from(block_log), 0);
                assert!(leading * block <= size && size - leading * block < block);
            }
        }
    }

    #[test]
    fn block_words_keep_their_length_only_inside_the_block_size() {
        for word in [
            0u32,
            1,
            4096,
            4097,
            DATA_UNCOMPRESSED,
            DATA_UNCOMPRESSED | 4096,
            1 << 25,
            u32::MAX,
        ] {
            match data_block(word, 4096) {
                Ok(None) => assert_eq!(word, 0),
                Ok(Some((length, stored))) => {
                    assert!((1..=4096).contains(&length));
                    assert_eq!(stored, word & DATA_UNCOMPRESSED != 0);
                    assert_eq!(word & DATA_LENGTH, length);
                }
                Err(_) => {
                    assert!(word & DATA_LENGTH == 0 || word & DATA_LENGTH > 4096 || word >> 25 != 0)
                }
            }
        }
    }

    #[test]
    fn every_inode_type_reads_exactly_its_body() {
        for kind in 0..=16u16 {
            let mut base = [0u8; BASE_INODE];
            base[..2].copy_from_slice(&kind.to_le_bytes());
            let Ok(length) = body_length(kind) else {
                assert!(kind == 0 || kind > 14);
                continue;
            };
            let mut body = vec![0u8; length];
            if matches!(kind, 3 | 10) {
                body[4] = 1;
            }
            if kind == 1 {
                body[8] = 3;
            }
            if kind == 8 {
                body[4] = 3;
            }
            let inode = inode(&base, &body).unwrap();
            assert_eq!(inode.extended, kind > EXTENDED);
            assert!(inode.kind <= EXTENDED);
            assert!(super::inode(&base, &body[..length - 1]).is_err());
        }
    }
}

#[cfg(kani)]
mod field_proofs {
    use super::*;

    #[kani::proof]
    fn superblocks_never_panic_and_keep_their_tables_in_order() {
        let bytes: [u8; SUPERBLOCK] = kani::any();
        if let Ok(superblock) = superblock(&bytes) {
            assert!(superblock.block_size.is_power_of_two());
            assert!(
                (1 << SMALLEST_BLOCK_LOG..=1 << LARGEST_BLOCK_LOG).contains(&superblock.block_size)
            );
            assert!(superblock.inode_table >= SUPERBLOCK as u64);
            assert!(superblock.inode_table < superblock.directory_table);
            assert!(superblock.directory_table <= superblock.fragment_table);
            assert!(superblock.fragment_table <= superblock.bytes_used);
            assert!(superblock.id_table < superblock.bytes_used);
            assert!(superblock.bytes_used <= LARGEST_IMAGE);
            assert!(superblock.root >> 16 < superblock.directory_table - superblock.inode_table);
            assert!(
                superblock.inode_count >= 1 && superblock.fragment_count <= superblock.inode_count
            );
        }
    }

    #[kani::proof]
    fn inodes_never_panic_and_keep_their_fields_in_range() {
        let base: [u8; BASE_INODE] = kani::any();
        let body: [u8; LONGEST_INODE_BODY] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= LONGEST_INODE_BODY);
        if let Ok(inode) = inode(&base, &body[..length]) {
            assert!(body_length(le16(&base, 0)).is_ok_and(|expected| expected == length));
            assert!((DIRECTORY..=EXTENDED).contains(&inode.kind));
            match inode.body {
                Body::Directory {
                    offset, listing, ..
                } => {
                    assert!(usize::from(offset) < METADATA_BLOCK && listing >= LISTING_DOTS)
                }
                Body::File { size, .. } => assert!(size <= LARGEST_IMAGE),
                Body::Symlink { target } => assert!(target >= 1 && target <= LONGEST_LINK),
                Body::Special => {}
            }
        }
    }

    #[kani::proof]
    fn folder_runs_and_entries_never_panic_and_stay_in_their_block() {
        let header: [u8; DIRECTORY_HEADER] = kani::any();
        if let Ok(run) = run_header(&header) {
            assert!(run.entries >= 1 && run.entries <= DIRECTORY_RUN);
        }
        let entry: [u8; DIRECTORY_ENTRY] = kani::any();
        if let Ok(entry) = directory_entry(&entry) {
            assert!(usize::from(entry.offset) < METADATA_BLOCK);
            assert!(entry.name >= 1 && entry.name <= 1 << 16);
        }
    }

    #[kani::proof]
    fn block_words_and_counts_stay_inside_the_block_size() {
        let word: u32 = kani::any();
        let block_log: u32 = kani::any();
        kani::assume(
            block_log >= u32::from(SMALLEST_BLOCK_LOG) && block_log <= u32::from(LARGEST_BLOCK_LOG),
        );
        let block_size = 1u32 << block_log;
        match data_block(word, block_size) {
            Ok(Some((length, _))) => assert!(length >= 1 && length <= block_size),
            Ok(None) => assert!(word == 0),
            Err(_) => {}
        }
        let size: u64 = kani::any();
        kani::assume(size <= LARGEST_IMAGE);
        let fragment: u32 = kani::any();
        let blocks = file_blocks(size, block_log, fragment);
        let covered = blocks << block_log;
        if fragment == NO_FRAGMENT {
            assert!(covered >= size && covered - size < u64::from(block_size));
        } else {
            assert!(covered <= size && size - covered < u64::from(block_size));
        }
    }
}
