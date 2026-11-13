use anyhow::{Context, Result, anyhow, ensure};
use std::{
    ffi::{c_char, c_int, c_uint},
    io::Read,
    ops::Range,
    sync::Arc,
};
use zipper::bench::Stream;

/// Each codec and the extension its stream file carries.
pub const CODECS: [(&str, &str); 7] = [
    ("deflate", "deflate"),
    ("gzip", "gz"),
    ("zstd", "zst"),
    ("xz", "xz"),
    ("lzma", "lzma"),
    ("bzip2", "bz2"),
    ("xpress", "xpress"),
];

pub struct Input {
    pub stream: Arc<[u8]>,
    chunks: Vec<(Range<usize>, usize)>,
    padded: Vec<Vec<u8>>,
    pool: Option<rayon::ThreadPool>,
}

impl Input {
    pub fn new(stream: Vec<u8>) -> Self {
        Input {
            stream: stream.into(),
            chunks: Vec::new(),
            padded: Vec::new(),
            pool: None,
        }
    }
}

pub struct Decoder {
    pub codec: &'static str,
    pub name: &'static str,
    pub crates: &'static [&'static str],
    pub api: &'static str,
    pub prepare: fn(&mut Input) -> Result<()>,
    pub decode: fn(&Input, &mut [u8]) -> Result<()>,
}

pub fn find(codec: &str, name: &str) -> Result<&'static Decoder> {
    DECODERS
        .iter()
        .find(|decoder| decoder.codec == codec && decoder.name == name)
        .with_context(|| format!("no decoder {name} for {codec}"))
}

fn nothing(_: &mut Input) -> Result<()> {
    Ok(())
}

fn fill(mut reader: impl Read, output: &mut [u8]) -> Result<()> {
    let mut at = 0;
    while at < output.len() {
        let read = reader.read(&mut output[at..])?;
        ensure!(
            read > 0,
            "the stream ends after {at} of {} bytes",
            output.len()
        );
        at += read;
    }
    let mut probe = [0u8; 1];
    ensure!(
        reader.read(&mut probe)? == 0,
        "the stream decodes past {} bytes",
        output.len()
    );
    Ok(())
}

fn zipper_reader(stream: Stream, input: &Input, output: &mut [u8]) -> Result<()> {
    fill(zipper::bench::reader(stream, input.stream.clone())?, output)
}

fn one_thread(input: &mut Input) -> Result<()> {
    input.pool = Some(rayon::ThreadPoolBuilder::new().num_threads(1).build()?);
    Ok(())
}

fn zipper_bzip2_one_thread(input: &Input, output: &mut [u8]) -> Result<()> {
    input
        .pool
        .as_ref()
        .context("no one-thread pool")?
        .install(|| zipper_reader(Stream::Bzip2, input, output))
}

fn zlib_rs(window_bits: i32, input: &Input, output: &mut [u8]) -> Result<()> {
    let size = output.len();
    let config = zlib_rs::InflateConfig { window_bits };
    let (decoded, code) = zlib_rs::decompress_slice(output, &input.stream, config);
    ensure!(code == zlib_rs::ReturnCode::Ok, "zlib-rs returned {code:?}");
    ensure!(
        decoded.len() == size,
        "zlib-rs decoded {} bytes",
        decoded.len()
    );
    Ok(())
}

fn miniz_raw(stream: &[u8], output: &mut [u8]) -> Result<usize> {
    use miniz_oxide::inflate::{TINFLStatus, core};
    let mut state = Box::<core::DecompressorOxide>::default();
    let flags = core::inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, consumed, written) = core::decompress(&mut state, stream, output, 0, flags);
    ensure!(
        status == TINFLStatus::Done,
        "miniz_oxide returned {status:?}"
    );
    ensure!(
        written == output.len(),
        "miniz_oxide decoded {written} bytes"
    );
    Ok(consumed)
}

// SPEC(RFC 1952 2.3.1): FEXTRA, FNAME, FCOMMENT and FHCRC follow the 10-byte header in that order.
fn gzip_header(stream: &[u8]) -> Result<usize> {
    ensure!(
        stream.len() >= 10 && stream[..3] == [0x1f, 0x8b, 8],
        "not a gzip member"
    );
    let flags = stream[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let length = stream.get(at..at + 2).context("truncated gzip header")?;
        at += 2 + usize::from(u16::from_le_bytes([length[0], length[1]]));
    }
    for field in [8, 16] {
        if flags & field != 0 {
            let end = stream
                .get(at..)
                .and_then(|rest| rest.iter().position(|&byte| byte == 0))
                .context("truncated gzip header")?;
            at += end + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    ensure!(at <= stream.len(), "truncated gzip header");
    Ok(at)
}

fn miniz_gzip(input: &Input, output: &mut [u8]) -> Result<()> {
    let header = gzip_header(&input.stream)?;
    let body = header + miniz_raw(&input.stream[header..], output)?;
    let trailer = input
        .stream
        .get(body..body + 8)
        .context("truncated gzip trailer")?;
    ensure!(
        body + 8 == input.stream.len(),
        "bytes follow the gzip member"
    );
    let stored_crc = u32::from_le_bytes(trailer[..4].try_into()?);
    let stored_size = u32::from_le_bytes(trailer[4..].try_into()?);
    ensure!(crc32fast::hash(output) == stored_crc, "gzip CRC-32 differs");
    ensure!(stored_size == output.len() as u32, "gzip size differs");
    Ok(())
}

fn libdeflate(gzip: bool, input: &Input, output: &mut [u8]) -> Result<()> {
    let mut decompressor = libdeflater::Decompressor::new();
    let written = if gzip {
        decompressor.gzip_decompress(&input.stream, output)
    } else {
        decompressor.deflate_decompress(&input.stream, output)
    }
    .map_err(|error| anyhow!("libdeflate: {error:?}"))?;
    ensure!(
        written == output.len(),
        "libdeflate decoded {written} bytes"
    );
    Ok(())
}

fn ruzstd(input: &Input, output: &mut [u8]) -> Result<()> {
    let mut decoder = ruzstd::decoding::FrameDecoder::new();
    let written = decoder
        .decode_all(&input.stream, output)
        .map_err(|error| anyhow!("ruzstd: {error:?}"))?;
    ensure!(written == output.len(), "ruzstd decoded {written} bytes");
    if let (Some(stored), Some(computed)) = (
        decoder.get_checksum_from_data(),
        decoder.get_calculated_checksum(),
    ) {
        ensure!(stored == computed, "ruzstd: the frame checksum differs");
    }
    Ok(())
}

fn libzstd(input: &Input, output: &mut [u8]) -> Result<()> {
    let written = zstd::bulk::Decompressor::new()?.decompress_to_buffer(&input.stream, output)?;
    ensure!(written == output.len(), "libzstd decoded {written} bytes");
    Ok(())
}

fn lzma_rs(xz: bool, input: &Input, output: &mut [u8]) -> Result<()> {
    let mut reader = &input.stream[..];
    let mut writer = &mut output[..];
    if xz {
        lzma_rs::xz_decompress(&mut reader, &mut writer)
    } else {
        lzma_rs::lzma_decompress(&mut reader, &mut writer)
    }
    .map_err(|error| anyhow!("lzma-rs: {error:?}"))?;
    ensure!(
        writer.is_empty(),
        "lzma-rs left {} bytes unwritten",
        writer.len()
    );
    Ok(())
}

fn lzma_rust2(xz: bool, input: &Input, output: &mut [u8]) -> Result<()> {
    if xz {
        fill(lzma_rust2::XzReader::new(&input.stream[..], false), output)
    } else {
        fill(
            lzma_rust2::LzmaReader::new_mem_limit(&input.stream[..], u32::MAX, None)?,
            output,
        )
    }
}

fn liblzma(xz: bool, input: &Input, output: &mut [u8]) -> Result<()> {
    if xz {
        fill(liblzma::bufread::XzDecoder::new(&input.stream[..]), output)
    } else {
        let stream = liblzma::stream::Stream::new_lzma_decoder(u64::MAX)?;
        fill(
            liblzma::bufread::XzDecoder::new_stream(&input.stream[..], stream),
            output,
        )
    }
}

fn libbz2_rs_sys(input: &Input, output: &mut [u8]) -> Result<()> {
    fill(bzip2::bufread::BzDecoder::new(&input.stream[..]), output)
}

const BZ_OK: c_int = 0;

unsafe extern "C" {
    fn BZ2_bzBuffToBuffDecompress(
        dest: *mut c_char,
        dest_len: *mut c_uint,
        source: *mut c_char,
        source_len: c_uint,
        small: c_int,
        verbosity: c_int,
    ) -> c_int;
}

fn libbzip2(input: &Input, output: &mut [u8]) -> Result<()> {
    let mut written = c_uint::try_from(output.len())?;
    let source_len = c_uint::try_from(input.stream.len())?;
    // SAFETY: each pointer is valid for the length passed beside it and `output` is not
    // aliased; libbzip2 declares `source` as `char *` but only reads it.
    let code = unsafe {
        BZ2_bzBuffToBuffDecompress(
            output.as_mut_ptr().cast(),
            &mut written,
            input.stream.as_ptr().cast_mut().cast(),
            source_len,
            0,
            0,
        )
    };
    ensure!(code == BZ_OK, "libbzip2 returned {code}");
    ensure!(
        written as usize == output.len(),
        "libbzip2 decoded {written} bytes"
    );
    Ok(())
}

fn xpress_chunks(input: &mut Input) -> Result<()> {
    input.chunks = crate::xpress::chunks(&input.stream)?;
    Ok(())
}

fn zipper_xpress(input: &Input, output: &mut [u8]) -> Result<()> {
    let chunks: Vec<(&[u8], usize)> = input
        .chunks
        .iter()
        .map(|(range, size)| (&input.stream[range.clone()], *size))
        .collect();
    zipper::bench::xpress(&chunks, output)
}

// EXTERNAL(xpress-huffman 0.1.3): it stops once its input cursor reaches the end, which a
// chunk that ends as WIM chunks end reaches before its last symbols, so each coded chunk
// gets two more zero bytes, added before timing.
fn padded_chunks(input: &mut Input) -> Result<()> {
    xpress_chunks(input)?;
    input.padded = input
        .chunks
        .iter()
        .map(|(range, size)| {
            let mut packed = input.stream[range.clone()].to_vec();
            if packed.len() != *size {
                packed.extend([0, 0]);
            }
            packed
        })
        .collect();
    Ok(())
}

fn xpress_huffman(input: &Input, output: &mut [u8]) -> Result<()> {
    let mut at = 0;
    for ((range, size), packed) in input.chunks.iter().zip(&input.padded) {
        let decoded = output
            .get_mut(at..at + size)
            .context("XPRESS chunks decode past the end of the output")?;
        if range.len() == *size {
            decoded.copy_from_slice(packed);
        } else {
            let chunk = xpress_huffman::decompress(packed, *size)
                .map_err(|error| anyhow!("xpress-huffman: {error:?}"))?;
            ensure!(
                chunk.len() == *size,
                "xpress-huffman decoded {} of {size} bytes",
                chunk.len()
            );
            decoded.copy_from_slice(&chunk);
        }
        at += size;
    }
    ensure!(at == output.len(), "XPRESS chunks decode to {at} bytes");
    Ok(())
}

const ZIPPER: &[&str] = &["zipper"];
const LIBDEFLATE: &[&str] = &["libdeflater", "libdeflate-sys"];
const LIBZSTD: &[&str] = &["zstd", "zstd-safe", "zstd-sys"];
const LIBLZMA: &[&str] = &["liblzma", "liblzma-sys"];
const READER: &str = "std::io::Read into the output";

pub const DECODERS: &[Decoder] = &[
    Decoder {
        codec: "deflate",
        name: "zipper",
        crates: ZIPPER,
        api: READER,
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Deflate, input, output),
    },
    Decoder {
        codec: "deflate",
        name: "zlib-rs",
        crates: &["zlib-rs"],
        api: "zlib_rs::decompress_slice",
        prepare: nothing,
        decode: |input, output| zlib_rs(-15, input, output),
    },
    Decoder {
        codec: "deflate",
        name: "miniz_oxide",
        crates: &["miniz_oxide"],
        api: "miniz_oxide::inflate::core::decompress into the whole output",
        prepare: nothing,
        decode: |input, output| miniz_raw(&input.stream, output).map(drop),
    },
    Decoder {
        codec: "deflate",
        name: "libdeflate",
        crates: LIBDEFLATE,
        api: "libdeflater::Decompressor::deflate_decompress",
        prepare: nothing,
        decode: |input, output| libdeflate(false, input, output),
    },
    Decoder {
        codec: "gzip",
        name: "zipper",
        crates: ZIPPER,
        api: READER,
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Gzip, input, output),
    },
    Decoder {
        codec: "gzip",
        name: "zlib-rs",
        crates: &["zlib-rs"],
        api: "zlib_rs::decompress_slice with window bits 31",
        prepare: nothing,
        decode: |input, output| zlib_rs(31, input, output),
    },
    Decoder {
        codec: "gzip",
        name: "miniz_oxide",
        crates: &["miniz_oxide", "crc32fast"],
        api: "this bench's gzip framing around miniz_oxide::inflate::core::decompress",
        prepare: nothing,
        decode: miniz_gzip,
    },
    Decoder {
        codec: "gzip",
        name: "libdeflate",
        crates: LIBDEFLATE,
        api: "libdeflater::Decompressor::gzip_decompress",
        prepare: nothing,
        decode: |input, output| libdeflate(true, input, output),
    },
    Decoder {
        codec: "zstd",
        name: "zipper",
        crates: ZIPPER,
        api: READER,
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Zstd, input, output),
    },
    Decoder {
        codec: "zstd",
        name: "ruzstd",
        crates: &["ruzstd"],
        api: "ruzstd::decoding::FrameDecoder::decode_all",
        prepare: nothing,
        decode: ruzstd,
    },
    Decoder {
        codec: "zstd",
        name: "libzstd",
        crates: LIBZSTD,
        api: "zstd::bulk::Decompressor::decompress_to_buffer",
        prepare: nothing,
        decode: libzstd,
    },
    Decoder {
        codec: "xz",
        name: "zipper",
        crates: ZIPPER,
        api: READER,
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Xz, input, output),
    },
    Decoder {
        codec: "xz",
        name: "lzma-rs",
        crates: &["lzma-rs"],
        api: "lzma_rs::xz_decompress into the output",
        prepare: nothing,
        decode: |input, output| lzma_rs(true, input, output),
    },
    Decoder {
        codec: "xz",
        name: "lzma-rust2",
        crates: &["lzma-rust2"],
        api: "lzma_rust2::XzReader, read into the output",
        prepare: nothing,
        decode: |input, output| lzma_rust2(true, input, output),
    },
    Decoder {
        codec: "xz",
        name: "liblzma",
        crates: LIBLZMA,
        api: "liblzma::bufread::XzDecoder, read into the output",
        prepare: nothing,
        decode: |input, output| liblzma(true, input, output),
    },
    Decoder {
        codec: "lzma",
        name: "zipper",
        crates: ZIPPER,
        api: READER,
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Lzma, input, output),
    },
    Decoder {
        codec: "lzma",
        name: "lzma-rs",
        crates: &["lzma-rs"],
        api: "lzma_rs::lzma_decompress into the output",
        prepare: nothing,
        decode: |input, output| lzma_rs(false, input, output),
    },
    Decoder {
        codec: "lzma",
        name: "lzma-rust2",
        crates: &["lzma-rust2"],
        api: "lzma_rust2::LzmaReader, read into the output",
        prepare: nothing,
        decode: |input, output| lzma_rust2(false, input, output),
    },
    Decoder {
        codec: "lzma",
        name: "liblzma",
        crates: LIBLZMA,
        api: "liblzma::bufread::XzDecoder over an LZMA-alone stream",
        prepare: nothing,
        decode: |input, output| liblzma(false, input, output),
    },
    Decoder {
        codec: "bzip2",
        name: "zipper",
        crates: &["zipper", "rayon"],
        api: "std::io::Read into the output, on a one-thread Rayon pool",
        prepare: one_thread,
        decode: zipper_bzip2_one_thread,
    },
    Decoder {
        codec: "bzip2",
        name: "zipper-all-threads",
        crates: &["zipper", "rayon"],
        api: "std::io::Read into the output, on the global Rayon pool",
        prepare: nothing,
        decode: |input, output| zipper_reader(Stream::Bzip2, input, output),
    },
    Decoder {
        codec: "bzip2",
        name: "libbz2-rs-sys",
        crates: &["bzip2", "libbz2-rs-sys"],
        api: "bzip2::bufread::BzDecoder, read into the output",
        prepare: nothing,
        decode: libbz2_rs_sys,
    },
    Decoder {
        codec: "bzip2",
        name: "libbzip2",
        crates: &["bzip2-sys"],
        api: "BZ2_bzBuffToBuffDecompress",
        prepare: nothing,
        decode: libbzip2,
    },
    Decoder {
        codec: "xpress",
        name: "zipper",
        crates: ZIPPER,
        api: "zipper::bench::xpress over every chunk",
        prepare: xpress_chunks,
        decode: zipper_xpress,
    },
    Decoder {
        codec: "xpress",
        name: "xpress-huffman",
        crates: &["xpress-huffman"],
        api: "xpress_huffman::decompress per chunk, copied into the output",
        prepare: padded_chunks,
        decode: xpress_huffman,
    },
];
