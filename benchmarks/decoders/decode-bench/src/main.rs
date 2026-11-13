mod decoders;
mod xpress;

use anyhow::{Context, Result, bail, ensure};
use bzip2_sys as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufReader, Read, Write},
    path::Path,
    time::Instant,
};

const USAGE: &str = "usage: zipper-decode-bench decoders\n\
                     \x20      zipper-decode-bench corpus ORIGINAL OUT_DIR\n\
                     \x20      zipper-decode-bench decode CODEC DECODER STREAM ORIGINAL REPORT";
const COMPARE_PIECE: usize = 1 << 20;
const PREFAULT: u8 = 0xA5;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["decoders"] => list(),
        ["corpus", original, out] => corpus(Path::new(original), Path::new(out)),
        ["decode", codec, decoder, stream, original, report] => decode(
            decoders::find(codec, decoder)?,
            Path::new(stream),
            Path::new(original),
            Path::new(report),
        ),
        _ => bail!(USAGE),
    }
}

fn list() -> Result<()> {
    let rows: Vec<Value> = decoders::DECODERS
        .iter()
        .map(|decoder| {
            json!({
                "codec": decoder.codec,
                "decoder": decoder.name,
                "crates": decoder.crates,
                "api": decoder.api,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn encode(codec: &str, data: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    let level6 = flate2::Compression::new(6);
    Ok(match codec {
        "deflate" => {
            let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), level6);
            encoder.write_all(data)?;
            (encoder.finish()?, "flate2 with zlib-rs, level 6, raw")
        }
        "gzip" => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), level6);
            encoder.write_all(data)?;
            (encoder.finish()?, "flate2 with zlib-rs, level 6")
        }
        "zstd" => {
            let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3)?;
            encoder.include_checksum(true)?;
            encoder.write_all(data)?;
            (
                encoder.finish()?,
                "libzstd level 3, one frame with its checksum",
            )
        }
        "xz" => {
            let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
            encoder.write_all(data)?;
            (encoder.finish()?, "liblzma preset 6, one block, CRC-64")
        }
        "lzma" => {
            let options = liblzma::stream::LzmaOptions::new_preset(6)?;
            let stream = liblzma::stream::Stream::new_lzma_encoder(&options)?;
            let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
            encoder.write_all(data)?;
            (
                encoder.finish()?,
                "liblzma preset 6, LZMA-alone with an end marker",
            )
        }
        "bzip2" => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
            encoder.write_all(data)?;
            (encoder.finish()?, "libbz2-rs-sys level 9")
        }
        "xpress" => (
            xpress::compress(data),
            "this bench's greedy encoder, 32 KiB chunks with one code each",
        ),
        _ => bail!("no encoder for {codec}"),
    })
}

fn corpus(original: &Path, out: &Path) -> Result<()> {
    let data = fs::read(original).with_context(|| format!("read {}", original.display()))?;
    let name = original
        .file_name()
        .and_then(|name| name.to_str())
        .context("the original's name is not UTF-8")?;
    fs::create_dir_all(out)?;
    let mut streams = serde_json::Map::new();
    for (codec, extension) in decoders::CODECS {
        let (bytes, encoder) = encode(codec, &data)?;
        let file = format!("{name}.{extension}");
        fs::write(out.join(&file), &bytes)?;
        streams.insert(
            codec.to_owned(),
            json!({
                "file": file,
                "bytes": bytes.len(),
                "sha256": sha256(&bytes),
                "encoder": encoder,
            }),
        );
        eprintln!("{file}: {} bytes", bytes.len());
    }
    let manifest = json!({
        "original": {"name": name, "bytes": data.len(), "sha256": sha256(&data)},
        "streams": streams,
    });
    fs::write(
        out.join("corpus.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(())
}

#[cfg(unix)]
fn cpu_seconds() -> Option<f64> {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    Some(now.tv_sec as f64 + now.tv_nsec as f64 / 1e9)
}

#[cfg(not(unix))]
fn cpu_seconds() -> Option<f64> {
    None
}

fn verify(original: &Path, output: &[u8]) -> Result<u64> {
    let file = fs::File::open(original)?;
    ensure!(
        file.metadata()?.len() == output.len() as u64,
        "the original changed size"
    );
    let mut file = BufReader::with_capacity(COMPARE_PIECE, file);
    let mut piece = vec![0; COMPARE_PIECE];
    let mut verified = 0;
    for expected in output.chunks(COMPARE_PIECE) {
        let piece = &mut piece[..expected.len()];
        file.read_exact(piece)?;
        ensure!(
            piece == expected,
            "decoded bytes differ from the original in the MiB at {verified}"
        );
        verified += expected.len() as u64;
    }
    Ok(verified)
}

fn decode(
    decoder: &decoders::Decoder,
    stream: &Path,
    original: &Path,
    report: &Path,
) -> Result<()> {
    let mut input = decoders::Input::new(fs::read(stream)?);
    (decoder.prepare)(&mut input)?;
    let size = usize::try_from(fs::metadata(original)?.len())?;
    let mut output = vec![0u8; size];
    output.fill(PREFAULT);
    let cpu_before = cpu_seconds();
    let start = Instant::now();
    (decoder.decode)(&input, &mut output)
        .with_context(|| format!("{} {} failed", decoder.codec, decoder.name))?;
    let seconds = start.elapsed().as_secs_f64();
    let cpu = cpu_seconds()
        .zip(cpu_before)
        .map(|(after, before)| after - before);
    let verified = verify(original, &output)?;
    let row = json!({
        "codec": decoder.codec,
        "decoder": decoder.name,
        "seconds": seconds,
        "cpu_seconds": cpu,
        "input_bytes": input.stream.len(),
        "verified_bytes": verified,
        "rayon_threads": rayon::current_num_threads(),
    });
    fs::write(report, serde_json::to_string(&row)? + "\n")?;
    Ok(())
}
