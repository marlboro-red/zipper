//! VirtualBox VDI images as a container layer of the disk reader.
//! `docs/reference/disk-reader.md` gives the rules.

use crate::{
    Field,
    disk::{Claims, Disk, Layer, Opened, Pages, read_at, read_file_exact},
    field_check,
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, ensure};
use std::io;

pub(crate) const SIGNATURE_AT: usize = 0x40;
pub(crate) const SIGNATURE: u32 = 0xBEDA_107F;
pub(crate) const HEADER_READ: usize = 0x1C8;
const MAJOR_VERSION: u32 = 1;
const SMALLEST_HEADER: u32 = 0x180;
const DYNAMIC: u32 = 1;
const DIFFERENCING: u32 = 4;
const BLOCK_LOG: u32 = 20;
const SECTOR: u32 = 512;
const HEADER_FIELDS_AT: u64 = 0x48;
const LARGEST_DISK_LOG: u32 = 56;
const FREE: u32 = u32::MAX;
const ZERO: u32 = u32::MAX - 1;
const ENTRY: u64 = 4;
const MAP_BATCH: usize = 1 << 16;

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) kind: u32,
    pub(crate) map_at: u64,
    pub(crate) data_at: u64,
    pub(crate) size: u64,
    pub(crate) blocks: u32,
    pub(crate) allocated: u32,
    pub(crate) parent: bool,
}

pub(crate) fn header(p: &[u8; HEADER_READ]) -> Field<Header> {
    field_check(le32(p, SIGNATURE_AT) == SIGNATURE, "not a VDI image")?;
    field_check(
        le32(p, 0x44) >> 16 == MAJOR_VERSION,
        "the VDI image has an unknown version",
    )?;
    let header_size = u64::from(le32(p, 0x48));
    field_check(
        header_size >= u64::from(SMALLEST_HEADER),
        "the VDI header is too short",
    )?;
    field_check(
        le32(p, 0x168) == SECTOR,
        "the VDI sectors are not 512 bytes, which 7-Zip does not read either",
    )?;
    let kind = le32(p, 0x4C);
    field_check(
        (DYNAMIC..=DIFFERENCING).contains(&kind),
        "the VDI image has an unknown type",
    )?;
    field_check(
        le32(p, 0x178) == 1 << BLOCK_LOG,
        "the VDI blocks are not 1 MiB, which 7-Zip does not read either",
    )?;
    field_check(
        le32(p, 0x17C) == 0,
        "the VDI blocks carry extra data, which 7-Zip reads as block data",
    )?;
    let header = Header {
        kind,
        map_at: u64::from(le32(p, 0x154)),
        data_at: u64::from(le32(p, 0x158)),
        size: le64(p, 0x170),
        blocks: le32(p, 0x180),
        allocated: le32(p, 0x184),
        parent: p[0x1A8..0x1C8].iter().any(|byte| *byte != 0),
    };
    field_check(
        header.size >> LARGEST_DISK_LOG == 0,
        "the VDI disk is larger than 2^56 bytes",
    )?;
    field_check(
        u64::from(header.blocks) << BLOCK_LOG >= header.size,
        "the VDI block count does not cover its disk",
    )?;
    field_check(
        header.map_at >= HEADER_FIELDS_AT + header_size
            && header.map_at + ENTRY * u64::from(header.blocks) <= header.data_at,
        "the VDI block map overlaps the header or the data",
    )?;
    Ok(header)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Zeros,
    At(u64),
}

pub(crate) fn block_place(entry: u32, header: &Header) -> Block {
    if entry == FREE || entry == ZERO {
        return Block::Zeros;
    }
    Block::At(header.data_at + (u64::from(entry) << BLOCK_LOG))
}

struct Vdi {
    header: Header,
    length: u64,
    pages: Pages,
}

impl Layer for Vdi {
    fn read(&mut self, file: &ArchiveFile, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let block_log = BLOCK_LOG;
        let index = offset >> block_log;
        let within = offset & ((1 << block_log) - 1);
        let want = (buffer.len() as u64).min((1 << block_log) - within) as usize;
        let mut entry = [0u8; ENTRY as usize];
        self.pages.read(
            file,
            self.length,
            self.header.map_at + ENTRY * index,
            &mut entry,
        )?;
        match block_place(u32::from_le_bytes(entry), &self.header) {
            Block::Zeros => buffer[..want].fill(0),
            Block::At(at) => read_file_exact(file, &mut buffer[..want], at + within)?,
        }
        Ok(want)
    }
}

fn check_map(file: &ArchiveFile, length: u64, header: &Header) -> Result<()> {
    let block = 1u64 << BLOCK_LOG;
    let used_blocks = (header.size >> BLOCK_LOG) + u64::from(header.size & (block - 1) != 0);
    ensure!(
        header.map_at + ENTRY * used_blocks <= length,
        "the VDI block map lies past the end of the image"
    );
    ensure!(
        header.data_at + (u64::from(header.allocated) << BLOCK_LOG) <= length,
        "the image ends inside its allocated VDI blocks"
    );
    let mut claimed = Claims::default();
    let mut batch = vec![0u8; MAP_BATCH];
    let mut read = 0u64;
    while read < used_blocks {
        let count = (used_blocks - read).min(MAP_BATCH as u64 / ENTRY);
        let bytes = &mut batch[..(count * ENTRY) as usize];
        read_file_exact(file, bytes, header.map_at + read * ENTRY)
            .context("cannot read the VDI block map")?;
        for entry in bytes.chunks_exact(ENTRY as usize) {
            let entry = le32(entry, 0);
            if let Block::At(_) = block_place(entry, header) {
                ensure!(
                    entry < header.allocated,
                    "a VDI block map entry names a block past the allocated ones"
                );
                ensure!(
                    claimed.claim(u64::from(entry)),
                    "two VDI blocks share their bytes in the file"
                );
            }
        }
        read += count;
    }
    Ok(())
}

pub(crate) fn open(file: ArchiveFile, length: u64) -> Result<Opened> {
    let bytes = read_at(&file, length, 0, HEADER_READ, "VDI header")?;
    let header = header(bytes[..].try_into().unwrap()).map_err(anyhow::Error::from)?;
    if header.kind == DIFFERENCING || header.parent {
        return Ok(Opened::Elsewhere(
            "the image is a differencing VDI, which needs its parent disk".to_owned(),
        ));
    }
    check_map(&file, length, &header)?;
    let layer = Vdi {
        header,
        length,
        pages: Pages::default(),
    };
    Ok(Opened::Disk(Disk::layered(
        file,
        header.size,
        Box::new(layer),
    )))
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(34)]
    fn headers_never_panic_and_every_block_of_the_disk_has_a_map_entry() {
        let bytes: [u8; HEADER_READ] = kani::any();
        if let Ok(found) = header(&bytes) {
            assert!(found.map_at + ENTRY * u64::from(found.blocks) <= found.data_at);
            let offset: u64 = kani::any();
            kani::assume(offset < found.size);
            assert!(offset >> BLOCK_LOG < u64::from(found.blocks));
            let entry: u32 = kani::any();
            if let Block::At(at) = block_place(entry, &found) {
                assert!(at >= found.data_at);
                assert!(at < 1 << 63);
            }
        }
    }
}
