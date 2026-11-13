//! Zipper's own decoders over bytes held in memory, for the decode bench in
//! `benchmarks/decoders/decode-bench`. This module is hidden and outside the
//! semver contract.
use crate::native_tar::Compression;
use anyhow::{Context, Result, ensure};
use std::{
    io::{Cursor, Read},
    sync::Arc,
};

/// A stream format the decode bench reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Raw Deflate without framing.
    Deflate,
    Gzip,
    Zstd,
    Xz,
    /// The `.lzma` format of LZMA Utils, with a 13-byte header.
    Lzma,
    Bzip2,
}

/// A reader of the decoded bytes of `input`, built as tar extraction builds
/// it. Bzip2 decodes blocks on the current Rayon pool.
pub fn reader(stream: Stream, input: Arc<[u8]>) -> Result<Box<dyn Read + Send>> {
    let input = Cursor::new(input);
    let compression = match stream {
        Stream::Deflate => return Ok(Box::new(crate::framing::Reader::raw(input))),
        Stream::Gzip => Compression::Gzip,
        Stream::Zstd => Compression::Zstd,
        Stream::Xz => Compression::Xz,
        Stream::Lzma => Compression::Lzma,
        Stream::Bzip2 => Compression::Bzip2,
    };
    crate::native_tar::decode(input, compression)
}

/// Decodes XPRESS chunks as the WIM reader decodes them. Each chunk is its
/// packed bytes and its decoded size, and a chunk whose packed bytes are as
/// long as its decoded size is stored. The decoded chunks fill `output` in
/// order.
pub fn xpress(chunks: &[(&[u8], usize)], output: &mut [u8]) -> Result<()> {
    let mut huffman = crate::xpress::Huffman::new();
    let mut at = 0;
    for &(packed, size) in chunks {
        let decoded = output
            .get_mut(at..at + size)
            .context("XPRESS chunks decode past the end of the output")?;
        if packed.len() == size {
            decoded.copy_from_slice(packed);
        } else {
            crate::xpress::decode(&mut huffman, packed, decoded)?;
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
