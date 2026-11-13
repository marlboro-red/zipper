use crate::native_tar::Compression;
use anyhow::{Context, Result, ensure};
use std::{
    io::{Cursor, Read},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    RawDeflate,
    Gzip,
    Zstd,
    Xz,
    LzmaAlone,
    Bzip2,
}

pub fn reader_on_current_pool(stream: Stream, input: Arc<[u8]>) -> Result<Box<dyn Read + Send>> {
    let input = Cursor::new(input);
    let compression = match stream {
        Stream::RawDeflate => return Ok(Box::new(crate::framing::Reader::raw(input))),
        Stream::Gzip => Compression::Gzip,
        Stream::Zstd => Compression::Zstd,
        Stream::Xz => Compression::Xz,
        Stream::LzmaAlone => Compression::Lzma,
        Stream::Bzip2 => Compression::Bzip2,
    };
    crate::native_tar::decode(input, compression)
}

#[derive(Clone, Copy, Debug)]
pub enum Chunk<'a> {
    Stored(&'a [u8]),
    Coded { packed: &'a [u8], size: usize },
}

pub fn xpress(chunks: &[Chunk], output: &mut [u8]) -> Result<()> {
    let mut huffman = crate::xpress::Huffman::new();
    let mut at = 0;
    for chunk in chunks {
        let size = match *chunk {
            Chunk::Stored(bytes) => bytes.len(),
            Chunk::Coded { size, .. } => size,
        };
        let decoded = output
            .get_mut(at..at + size)
            .context("XPRESS chunks decode past the end of the output")?;
        match *chunk {
            Chunk::Stored(bytes) => decoded.copy_from_slice(bytes),
            Chunk::Coded { packed, .. } => crate::xpress::decode(&mut huffman, packed, decoded)?,
        }
        at += size;
    }
    ensure!(
        at == output.len(),
        "XPRESS chunks decode to {at} bytes, not {}",
        output.len()
    );
    Ok(())
}
