use crate::{
    BadField, Field,
    disk::{Disk, Layer, Opened, read_at, read_file_exact},
    field_check,
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, ensure};
use std::io;

pub(crate) const MAGIC: u32 = 0xED26_FF3A;
pub(crate) const HEADER: usize = 28;
pub(crate) const CHUNK: usize = 12;
const MAJOR: u16 = 1;
const RAW: u16 = 0xCAC1;
const FILL: u16 = 0xCAC2;
const DONT_CARE: u16 = 0xCAC3;
const CRC32: u16 = 0xCAC4;
const FILL_VALUE: u32 = 4;
const LARGEST_BLOCK: u32 = 1 << 28;
const GROUP: u64 = 256;
const LARGEST_DISK_LOG: u32 = 46;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) block: u32,
    pub(crate) blocks: u32,
    pub(crate) chunks: u32,
}

pub(crate) fn header(p: &[u8; HEADER]) -> Field<Header> {
    field_check(le32(p, 0) == MAGIC, "not an Android sparse image")?;
    field_check(
        le16(p, 4) == MAJOR,
        "the Android sparse image has an unknown major version",
    )?;
    field_check(
        usize::from(le16(p, 8)) == HEADER && usize::from(le16(p, 10)) == CHUNK,
        "the Android sparse image has header sizes Zipper does not know",
    )?;
    let header = Header {
        block: le32(p, 12),
        blocks: le32(p, 16),
        chunks: le32(p, 20),
    };
    field_check(
        header.block != 0 && header.block.is_multiple_of(4) && header.block <= LARGEST_BLOCK,
        "the Android sparse block size is not a multiple of 4 bytes up to 256 MiB",
    )?;
    field_check(
        (u64::from(header.blocks) * u64::from(header.block)) >> LARGEST_DISK_LOG == 0,
        "the Android sparse disk is larger than 64 TiB",
    )?;
    Ok(header)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Raw,
    Fill(u32),
    Zeros,
    Checksum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub(crate) kind: Kind,
    pub(crate) blocks: u32,
    pub(crate) bytes: u64,
}

pub(crate) fn chunk(p: &[u8; CHUNK + 4], block: u32) -> Field<Chunk> {
    let blocks = le32(p, 4);
    let bytes = u64::from(le32(p, 8));
    let (kind, body) = match le16(p, 0) {
        RAW => (Kind::Raw, u64::from(blocks) * u64::from(block)),
        FILL => (Kind::Fill(le32(p, CHUNK)), u64::from(FILL_VALUE)),
        DONT_CARE => (Kind::Zeros, 0),
        CRC32 => (Kind::Checksum, u64::from(FILL_VALUE)),
        _ => return Err(BadField("an Android sparse chunk has an unknown type")),
    };
    field_check(
        le16(p, 2) == 0,
        "an Android sparse chunk sets its reserved field",
    )?;
    field_check(
        bytes == CHUNK as u64 + body,
        "an Android sparse chunk's size does not match its type",
    )?;
    field_check(
        kind != Kind::Checksum || blocks == 0,
        "an Android sparse checksum chunk covers blocks",
    )?;
    Ok(Chunk {
        kind,
        blocks,
        bytes,
    })
}

#[derive(Clone, Copy)]
struct Placed {
    first: u64,
    blocks: u64,
    kind: Kind,
    data_at: u64,
}

struct Sparse {
    header: Header,
    length: u64,
    marks: Vec<(u64, u64)>,
    group: Option<(usize, Vec<Placed>)>,
}

fn read_chunk(file: &ArchiveFile, length: u64, at: u64, block: u32) -> Result<Chunk> {
    let mut bytes = [0u8; CHUNK + 4];
    let available = (length - at).min(bytes.len() as u64) as usize;
    ensure!(
        available >= CHUNK,
        "the image ends inside an Android sparse chunk header"
    );
    read_file_exact(file, &mut bytes[..available], at)
        .context("cannot read an Android sparse chunk header")?;
    let found = chunk(&bytes, block).map_err(anyhow::Error::from)?;
    ensure!(
        at.checked_add(found.bytes).is_some_and(|end| end <= length),
        "an Android sparse chunk lies past the end of the image"
    );
    Ok(found)
}

impl Sparse {
    fn decode_group(&self, file: &ArchiveFile, mark: usize) -> io::Result<Vec<Placed>> {
        let (mut first, mut at) = self.marks[mark];
        let index = mark as u64 * GROUP;
        let count = (u64::from(self.header.chunks) - index).min(GROUP);
        let mut placed = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let found = read_chunk(file, self.length, at, self.header.block)
                .map_err(|error| io::Error::other(format!("{error:#}")))?;
            placed.push(Placed {
                first,
                blocks: u64::from(found.blocks),
                kind: found.kind,
                data_at: at + CHUNK as u64,
            });
            first += u64::from(found.blocks);
            at += found.bytes;
        }
        Ok(placed)
    }
}

impl Layer for Sparse {
    fn read(&mut self, file: &ArchiveFile, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let block = u64::from(self.header.block);
        let index = offset / block;
        let changed = || io::Error::other("the Android sparse image changed while it was read");
        let mark = self
            .marks
            .partition_point(|(first, _)| *first <= index)
            .checked_sub(1)
            .ok_or_else(changed)?;
        if self.group.as_ref().is_none_or(|(held, _)| *held != mark) {
            let placed = self.decode_group(file, mark)?;
            self.group = Some((mark, placed));
        }
        let placed = &self.group.as_ref().unwrap().1;
        let found = *placed
            .get(placed.partition_point(|chunk| chunk.first + chunk.blocks <= index))
            .filter(|chunk| chunk.first <= index)
            .ok_or_else(changed)?;
        let within = offset - found.first * block;
        let want = (buffer.len() as u64).min(found.blocks * block - within) as usize;
        match found.kind {
            Kind::Raw => read_file_exact(file, &mut buffer[..want], found.data_at + within)?,
            Kind::Fill(value) => {
                let word = value.to_le_bytes();
                for (place, byte) in buffer[..want].iter_mut().enumerate() {
                    *byte = word[(within as usize + place) % 4];
                }
            }
            Kind::Zeros | Kind::Checksum => buffer[..want].fill(0),
        }
        Ok(want)
    }
}

pub(crate) fn open(file: ArchiveFile, length: u64) -> Result<Opened> {
    let bytes = read_at(&file, length, 0, HEADER, "Android sparse header")?;
    let header = header(bytes[..].try_into().unwrap()).map_err(anyhow::Error::from)?;
    let mut marks = Vec::new();
    let (mut first, mut at) = (0u64, HEADER as u64);
    for index in 0..u64::from(header.chunks) {
        if index % GROUP == 0 {
            marks.push((first, at));
        }
        let found = read_chunk(&file, length, at, header.block)?;
        first += u64::from(found.blocks);
        at += found.bytes;
        ensure!(
            first <= u64::from(header.blocks),
            "the Android sparse chunks cover more blocks than the image holds"
        );
    }
    ensure!(
        first == u64::from(header.blocks),
        "the Android sparse chunks cover fewer blocks than the image holds"
    );
    let size = u64::from(header.blocks) * u64::from(header.block);
    let layer = Sparse {
        header,
        length,
        marks,
        group: None,
    };
    Ok(Opened::Disk(Disk::layered(file, size, Box::new(layer))))
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_and_chunks_never_panic_and_chunks_hold_exactly_their_bytes() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(found) = header(&bytes) {
            assert!(found.block >= 4 && found.block % 4 == 0 && found.block <= LARGEST_BLOCK);
        }
        let block: u32 = kani::any();
        kani::assume(block >= 4 && block <= LARGEST_BLOCK);
        let bytes: [u8; CHUNK + 4] = kani::any();
        if let Ok(found) = chunk(&bytes, block) {
            match found.kind {
                Kind::Raw => {
                    assert!(
                        found.bytes == CHUNK as u64 + u64::from(found.blocks) * u64::from(block)
                    )
                }
                Kind::Zeros => assert!(found.bytes == CHUNK as u64),
                Kind::Fill(_) => assert!(found.bytes == CHUNK as u64 + 4),
                Kind::Checksum => assert!(found.bytes == CHUNK as u64 + 4 && found.blocks == 0),
            }
        }
    }
}
