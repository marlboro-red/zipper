#![allow(dead_code)]
#[path = "/path/to/zipper/src/inflate.rs"]
mod inflate;

use std::io::Read;

const INPUT: usize = 512 * 1024;
const OUTPUT: usize = 512 * 1024;

fn ours(packed: &[u8], window: &mut [u8], mut sink: impl FnMut(&[u8])) {
    let mut inflate = inflate::Inflate::new(window);
    let mut at = 0;
    loop {
        let end = (at + INPUT).min(packed.len());
        let step = inflate.decode(&packed[at..end]).unwrap();
        at += step.consumed;
        sink(step.output);
        if step.finished {
            return;
        }
        assert!(!step.needs_input());
    }
}

fn zlib(packed: &[u8], window: &mut [u8], mut sink: impl FnMut(&[u8])) {
    let mut inflater = flate2::Decompress::new(false);
    let mut at = 0;
    loop {
        let end = (at + INPUT).min(packed.len());
        let (consumed, produced) = (inflater.total_in(), inflater.total_out());
        let status = inflater
            .decompress(&packed[at..end], window, flate2::FlushDecompress::None)
            .unwrap();
        at += (inflater.total_in() - consumed) as usize;
        let made = (inflater.total_out() - produced) as usize;
        sink(&window[..made]);
        if status == flate2::Status::StreamEnd {
            return;
        }
    }
}

#[cfg(feature = "libdeflate")]
fn whole(packed: &[u8], size: usize, out: &mut Vec<u8>) {
    out.resize(size, 0);
    libdeflater::Decompressor::new()
        .deflate_decompress(packed, out)
        .unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let decoder = args.next().unwrap();
    let rounds: usize = args.next().unwrap().parse().unwrap();
    let mut entries = Vec::new();
    for path in args {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        for index in 0..archive.len() {
            let mut raw = archive.by_index_raw(index).unwrap();
            if raw.compression() == zip::CompressionMethod::Stored || raw.is_dir() {
                continue;
            }
            let (size, crc) = (raw.size(), raw.crc32());
            let mut packed = Vec::new();
            raw.read_to_end(&mut packed).unwrap();
            entries.push((packed, size, crc));
        }
    }
    let mut window = vec![0u8; OUTPUT];
    let mut scratch = Vec::new();
    let run = |packed: &[u8],
               size: u64,
               window: &mut [u8],
               scratch: &mut Vec<u8>,
               sink: &mut dyn FnMut(&[u8])| {
        let _ = (size, &scratch);
        match decoder.as_str() {
            "ours" => ours(packed, window, sink),
            "zlib" => zlib(packed, window, sink),
            #[cfg(feature = "libdeflate")]
            "libdeflate" => {
                whole(packed, size as usize, scratch);
                sink(scratch);
            }
            other => panic!("unknown decoder {other}"),
        }
    };
    let (mut packed_total, mut size_total) = (0u64, 0u64);
    for (packed, size, crc) in &entries {
        let mut hasher = crc32fast::Hasher::new();
        let mut total = 0u64;
        run(packed, *size, &mut window, &mut scratch, &mut |bytes| {
            hasher.update(bytes);
            total += bytes.len() as u64;
        });
        assert_eq!((total, hasher.finalize()), (*size, *crc));
        packed_total += packed.len() as u64;
        size_total += size;
    }
    for _ in 0..rounds {
        let mut total = 0u64;
        for (packed, size, _) in &entries {
            run(packed, *size, &mut window, &mut scratch, &mut |bytes| {
                total += bytes.len() as u64
            });
        }
        assert_eq!(total, size_total);
    }
    eprintln!(
        "{decoder}: {} entries, {packed_total} packed, {size_total} bytes, {rounds} rounds",
        entries.len()
    );
}
