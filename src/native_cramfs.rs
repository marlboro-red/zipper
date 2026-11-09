//! In-process extraction of CramFS images in either byte order, published
//! only after the whole image reads cleanly. See
//! `docs/reference/squashfs-reader.md` for what it accepts and restores.
use crate::hfs::Volume;
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree;
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashSet, VecDeque},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) const SUPERBLOCK: usize = 76;
pub(crate) const INODE: usize = 12;
const MAGIC: u32 = 0x28CD_3D45;
const SIGNATURE: &[u8; 16] = b"Compressed ROMFS";
const SIGNATURE_AT: usize = 16;
const CRC_AT: usize = 32;
const FILES_AT: usize = 44;
// EXTERNAL(7-Zip CramfsHandler): 7-Zip refuses an image whose superblock counts more than 2^19 files.
const MAX_FILES: u32 = 1 << 19;
const ROOT_AT: usize = 64;
const FSID_VERSION_2: u32 = 1;
// EXTERNAL(7-Zip CramfsHandler): 7-Zip reads blocks another way when the flags set 0x800,
// which Linux names extended block pointers, or 0x1000, 0x2000 or 0x8000.
const OTHER_LAYOUT_FLAGS: u32 = 0x800 | 0x1000 | 0x2000 | 0x8000;
pub(crate) const PAGE: u32 = 4096;
const LARGEST_COMPRESSED_PAGE: u32 = 2 * PAGE;
const NAME_UNIT: usize = 4;
const LARGEST_V1_IMAGE: u64 = 1 << 28;
const TYPE_BITS: u16 = 0o170_000;
const DIRECTORY: u16 = 0o040_000;
const REGULAR: u16 = 0o100_000;
const SYMLINK: u16 = 0o120_000;
const DEFAULT_FILE_MODE: u32 = 0o666;
const CRC_PIECE: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Order {
    pub(crate) big_endian: bool,
}

impl Order {
    fn word(self, bytes: &[u8], at: usize) -> u32 {
        let word: [u8; 4] = bytes[at..at + 4].try_into().unwrap();
        if self.big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Superblock {
    pub(crate) order: Order,
    pub(crate) size: Option<u64>,
    pub(crate) crc: Option<u32>,
    pub(crate) root: Inode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Inode {
    pub(crate) mode: u16,
    pub(crate) size: u32,
    pub(crate) name: usize,
    pub(crate) offset: u64,
}

// SPEC(linux include/uapi/linux/cramfs_fs.h, struct cramfs_inode): three words of bit fields,
// which a big-endian image packs from the top bit down.
pub(crate) fn inode(bytes: &[u8; INODE], order: Order) -> Inode {
    let [first, second, third] = [0, 4, 8].map(|at| order.word(bytes, at));
    if order.big_endian {
        Inode {
            mode: (first >> 16) as u16,
            size: second >> 8,
            name: (third >> 26) as usize * NAME_UNIT,
            offset: u64::from(third & 0x03FF_FFFF) * NAME_UNIT as u64,
        }
    } else {
        Inode {
            mode: first as u16,
            size: second & 0x00FF_FFFF,
            name: (third & 0x3F) as usize * NAME_UNIT,
            offset: u64::from(third >> 6) * NAME_UNIT as u64,
        }
    }
}

pub(crate) fn order_of(bytes: &[u8]) -> Option<Order> {
    let magic: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    if u32::from_le_bytes(magic) == MAGIC {
        Some(Order { big_endian: false })
    } else if u32::from_be_bytes(magic) == MAGIC {
        Some(Order { big_endian: true })
    } else {
        None
    }
}

// SPEC(linux include/uapi/linux/cramfs_fs.h, struct cramfs_super): magic, size, flags, a
// reserved word, the signature, the CRC, edition, block and file counts, a name and the root.
pub(crate) fn superblock(bytes: &[u8; SUPERBLOCK]) -> Field<Superblock> {
    let order = order_of(bytes).ok_or(BadField("not a CramFS image"))?;
    let flags = order.word(bytes, 8);
    field_check(
        flags & OTHER_LAYOUT_FLAGS == 0,
        "CramFS image sets flags for extended block pointers or another block layout, which Zipper does not read",
    )?;
    field_check(
        &bytes[SIGNATURE_AT..SIGNATURE_AT + SIGNATURE.len()] == SIGNATURE,
        "CramFS image lacks its signature",
    )?;
    let version_2 = flags & FSID_VERSION_2 != 0;
    let size = u64::from(order.word(bytes, 4));
    let root = inode(bytes[ROOT_AT..].try_into().unwrap(), order);
    field_check(
        root.mode & TYPE_BITS == DIRECTORY,
        "CramFS root inode is not a folder",
    )?;
    field_check(
        !version_2 || order.word(bytes, FILES_AT) <= MAX_FILES,
        "CramFS image counts more files than 7-Zip opens",
    )?;
    let superblock = Superblock {
        order,
        size: version_2.then_some(size),
        crc: version_2.then(|| order.word(bytes, CRC_AT)),
        root,
    };
    if let Some(size) = superblock.size {
        field_check(
            size >= SUPERBLOCK as u64,
            "CramFS image is smaller than its superblock",
        )?;
    }
    Ok(superblock)
}

pub(crate) fn block_count(size: u32) -> u64 {
    u64::from(size).div_ceil(u64::from(PAGE))
}

struct Image<'v> {
    volume: &'v mut dyn Volume,
    order: Order,
    end: u64,
    packed: Vec<u8>,
}

impl Image<'_> {
    fn read(&mut self, at: u64, length: usize) -> Result<&[u8]> {
        ensure!(
            at.checked_add(length as u64)
                .is_some_and(|end| end <= self.end),
            "CramFS image is truncated"
        );
        self.packed.resize(length, 0);
        self.volume.read_exact_at(at, &mut self.packed)?;
        Ok(&self.packed)
    }

    fn word(&mut self, at: u64) -> Result<u32> {
        let order = self.order;
        Ok(order.word(self.read(at, 4)?, 0))
    }

    fn inode(&mut self, at: u64) -> Result<(Inode, String)> {
        let order = self.order;
        let bytes: [u8; INODE] = self.read(at, INODE)?.try_into().unwrap();
        let inode = inode(&bytes, order);
        let raw = self.read(at + INODE as u64, inode.name)?;
        let name = &raw[..raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len())];
        ensure!(!name.is_empty(), "CramFS entry at {at} has no name");
        let name = std::str::from_utf8(name)
            .map_err(|_| anyhow::anyhow!("CramFS entry at {at} has a name that is not UTF-8"))?
            .to_owned();
        ensure!(
            !name.contains('/') && name != "." && name != "..",
            "CramFS entry at {at} has the unsafe name {name:?}"
        );
        ensure!(
            inode.offset <= self.end,
            "CramFS entry {name:?} points past the end of the image"
        );
        Ok((inode, name))
    }
}

fn check_crc(image: &mut Image, expected: u32) -> Result<()> {
    let mut hasher = crc32fast::Hasher::new();
    let mut at = 0;
    while at < image.end {
        let piece = (image.end - at).min(CRC_PIECE as u64) as usize;
        let bytes = image.read(at, piece)?;
        if at == 0 {
            let mut first = bytes.to_vec();
            first[CRC_AT..CRC_AT + 4].fill(0);
            hasher.update(&first);
        } else {
            hasher.update(bytes);
        }
        at += piece as u64;
    }
    ensure!(
        hasher.finalize() == expected,
        "CramFS image fails its CRC-32 check"
    );
    Ok(())
}

struct FileData<'i, 'v> {
    image: &'i mut Image<'v>,
    path: &'i str,
    table: u64,
    block: u64,
    blocks: u64,
    start: u64,
    remaining: u32,
    buffer: Vec<u8>,
    taken: usize,
}

impl FileData<'_, '_> {
    fn next_page(&mut self) -> Result<()> {
        let path = self.path;
        ensure!(
            self.block < self.blocks,
            "CramFS file {path:?} needs more blocks than its size gives"
        );
        let end = u64::from(self.image.word(self.table + 4 * self.block)?);
        ensure!(
            end >= self.start,
            "CramFS file {path:?} has a block that ends before it starts"
        );
        let length = end - self.start;
        let expected = self.remaining.min(PAGE) as usize;
        self.buffer.resize(expected, 0);
        self.taken = 0;
        self.block += 1;
        if length == 0 {
            self.buffer.fill(0);
            return Ok(());
        }
        ensure!(
            length <= u64::from(LARGEST_COMPRESSED_PAGE),
            "CramFS file {path:?} has a block larger than a compressed page"
        );
        let packed = self.image.read(self.start, length as usize)?;
        let mut inflater = flate2::Decompress::new(true);
        let status = inflater
            .decompress(packed, &mut self.buffer, flate2::FlushDecompress::Finish)
            .with_context(|| format!("CramFS file {path:?} has a damaged block"))?;
        ensure!(
            status == flate2::Status::StreamEnd
                && inflater.total_out() == expected as u64
                && inflater.total_in() == length,
            "CramFS file {path:?} has a block that does not decode to its page"
        );
        self.start = end;
        Ok(())
    }
}

impl Read for FileData<'_, '_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        if self.taken == self.buffer.len() {
            self.next_page().map_err(std::io::Error::other)?;
        }
        let piece = out.len().min(self.buffer.len() - self.taken);
        out[..piece].copy_from_slice(&self.buffer[self.taken..self.taken + piece]);
        self.taken += piece;
        self.remaining -= piece as u32;
        Ok(piece)
    }
}

fn file_data<'i, 'v>(image: &'i mut Image<'v>, inode: &Inode, path: &'i str) -> FileData<'i, 'v> {
    let blocks = block_count(inode.size);
    FileData {
        image,
        path,
        table: inode.offset,
        block: 0,
        blocks,
        start: inode.offset + 4 * blocks,
        remaining: inode.size,
        buffer: Vec::new(),
        taken: 0,
    }
}

fn file_mode(mode: u16) -> u32 {
    u32::from(mode) & 0o777
}

fn folder_mode(mode: u16) -> u32 {
    (u32::from(mode) | 0o700) & 0o777
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn emit(image: &mut Image, root: Inode, tree: &mut tree::Tree) -> Result<()> {
    let mut queue = VecDeque::from([(String::new(), root)]);
    let mut folders = HashSet::new();
    while let Some((folder, inode)) = queue.pop_front() {
        tree.check()?;
        if inode.size > 0 {
            ensure!(
                folders.insert(inode.offset),
                "CramFS folder {folder:?} shares its entries with another folder or forms a loop"
            );
        }
        let mut at = inode.offset;
        let end = inode.offset + u64::from(inode.size);
        let mut names = HashSet::new();
        while at < end {
            tree.check()?;
            let (child, name) = image.inode(at)?;
            at += (INODE + child.name) as u64;
            ensure!(
                at <= end,
                "CramFS folder {folder:?} has an entry past its end"
            );
            let path = join(&folder, &name);
            ensure!(
                names.insert(name),
                "CramFS folder {folder:?} holds two entries named {path:?}"
            );
            match child.mode & TYPE_BITS {
                DIRECTORY => {
                    tree.directory(&path, folder_mode(child.mode), None)?;
                    queue.push_back((path, child));
                }
                SYMLINK => {
                    ensure!(child.offset != 0, "CramFS link {path:?} has no target");
                    ensure!(
                        u64::from(child.size) <= crate::MAX_LINK_TARGET,
                        "CramFS link {path:?} has a target longer than {} bytes",
                        crate::MAX_LINK_TARGET
                    );
                    let mut target = Vec::with_capacity(child.size as usize);
                    file_data(image, &child, &path)
                        .take(u64::from(child.size))
                        .read_to_end(&mut target)
                        .with_context(|| format!("CramFS link {path:?} is damaged"))?;
                    ensure!(
                        target.len() == child.size as usize && !target.is_empty(),
                        "CramFS link {path:?} is shorter than its size"
                    );
                    let target = String::from_utf8(target).map_err(|_| {
                        anyhow::anyhow!("CramFS link {path:?} has a target that is not UTF-8")
                    })?;
                    tree.symlink(&path, target)?;
                }
                _ if child.offset == 0 => {
                    tree.file(&path, 0, DEFAULT_FILE_MODE, None, &mut std::io::empty())?
                }
                kind => {
                    let mode = if kind == REGULAR {
                        file_mode(child.mode)
                    } else {
                        DEFAULT_FILE_MODE
                    };
                    let mut data = file_data(image, &child, &path);
                    tree.file(&path, u64::from(child.size), mode, None, &mut data)?;
                    ensure!(
                        data.remaining == 0,
                        "CramFS file {path:?} is shorter than its size"
                    );
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn read(volume: &mut dyn Volume, tree: &mut tree::Tree) -> Result<()> {
    ensure!(
        volume.size() >= SUPERBLOCK as u64,
        "CramFS image is truncated"
    );
    let mut bytes = [0u8; SUPERBLOCK];
    volume.read_exact_at(0, &mut bytes)?;
    let superblock = superblock(&bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
    let end = match superblock.size {
        Some(size) => {
            ensure!(
                size <= volume.size(),
                "CramFS image is truncated: it declares {size} bytes and holds {}",
                volume.size()
            );
            size
        }
        None => volume.size().min(LARGEST_V1_IMAGE),
    };
    let mut image = Image {
        volume,
        order: superblock.order,
        end,
        packed: Vec::new(),
    };
    if let Some(crc) = superblock.crc {
        check_crc(&mut image, crc)?;
    }
    ensure!(
        superblock.root.offset <= end,
        "CramFS root folder points past the end of the image"
    );
    emit(&mut image, superblock.root, tree)
}

pub(crate) fn is_image(start: &[u8]) -> bool {
    order_of(start).is_some()
        && start.get(SIGNATURE_AT..SIGNATURE_AT + SIGNATURE.len()) == Some(SIGNATURE)
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
            "read past the end of the CramFS image"
        );
        let mut buffer = buffer;
        let mut offset = offset;
        while !buffer.is_empty() {
            let read = self
                .file
                .read_at(buffer, offset)
                .context("cannot read the CramFS image")?;
            ensure!(read != 0, "truncated CramFS image");
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
            "read past the end of the CramFS partition"
        );
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner
            .read_exact(buffer)
            .context("cannot read the CramFS partition")
    }
}

/// Returns whether the file starts with a CramFS superblock in either byte
/// order, with its `Compressed ROMFS` signature.
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

/// Verify and optionally extract a CramFS image held in a bounded view,
/// such as one GPT partition, into a new directory, in the tree `7zz x`
/// writes. Every check of `docs/reference/squashfs-reader.md` holds before
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
    tree::extract("CramFS", archive, destination, options, |tree| {
        read(&mut volume, tree)
    })
}

/// Verify and optionally extract a raw CramFS image into a new directory,
/// as [`read_volume`] describes.
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
    tree::extract("CramFS", path, destination, options, |tree| {
        read(&mut volume, tree)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn packed(inode: Inode, order: Order) -> [u8; INODE] {
        let words = if order.big_endian {
            [
                u32::from(inode.mode) << 16,
                inode.size << 8,
                ((inode.name / NAME_UNIT) as u32) << 26 | (inode.offset / 4) as u32,
            ]
        } else {
            [
                u32::from(inode.mode),
                inode.size,
                (inode.name / NAME_UNIT) as u32 | ((inode.offset / 4) as u32) << 6,
            ]
        };
        let mut bytes = [0u8; INODE];
        for (index, word) in words.into_iter().enumerate() {
            let word = if order.big_endian {
                word.to_be_bytes()
            } else {
                word.to_le_bytes()
            };
            bytes[4 * index..4 * index + 4].copy_from_slice(&word);
        }
        bytes
    }

    #[test]
    fn inodes_read_back_their_fields_in_both_byte_orders() {
        let mut state = 0x1234_5678u32;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let fields = Inode {
                mode: state as u16,
                size: state >> 8,
                name: (state >> 26) as usize * NAME_UNIT,
                offset: u64::from(state & 0x03FF_FFFF) * 4,
            };
            for big_endian in [false, true] {
                let order = Order { big_endian };
                assert_eq!(inode(&packed(fields, order), order), fields);
            }
        }
    }

    #[test]
    fn block_counts_cover_every_size_in_pages() {
        for size in [0u32, 1, PAGE - 1, PAGE, PAGE + 1, 0x00FF_FFFF] {
            let blocks = block_count(size);
            assert!(blocks * u64::from(PAGE) >= u64::from(size));
            assert!(blocks * u64::from(PAGE) < u64::from(size) + u64::from(PAGE));
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn inodes_never_panic_and_fit_their_bit_fields() {
        let bytes: [u8; INODE] = kani::any();
        let order = Order {
            big_endian: kani::any(),
        };
        let inode = inode(&bytes, order);
        assert!(inode.size < 1 << 24);
        assert!(inode.name <= 63 * NAME_UNIT && inode.name % NAME_UNIT == 0);
        assert!(inode.offset < 1 << 28 && inode.offset % NAME_UNIT as u64 == 0);
    }

    #[kani::proof]
    fn superblocks_never_panic_and_keep_a_folder_root() {
        let bytes: [u8; SUPERBLOCK] = kani::any();
        if let Ok(superblock) = superblock(&bytes) {
            assert!(superblock.root.mode & TYPE_BITS == DIRECTORY);
            assert!(superblock.size.is_none_or(|size| size >= SUPERBLOCK as u64));
            assert!(superblock.size.is_some() == superblock.crc.is_some());
        }
    }
}
