#![allow(dead_code)]
mod inflate;

use std::io::Read;

const INPUT: usize = 512 * 1024;

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

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let rounds: usize = args.next().unwrap().parse().unwrap();
    let window_size: usize = args.next().unwrap().parse().unwrap();
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
    let mut window = vec![0u8; window_size];
    let mut size_total = 0u64;
    for (packed, size, crc) in &entries {
        let mut hasher = crc32fast::Hasher::new();
        let mut total = 0u64;
        ours(packed, &mut window, |bytes| {
            hasher.update(bytes);
            total += bytes.len() as u64;
        });
        assert_eq!((total, hasher.finalize()), (*size, *crc));
        size_total += size;
    }
    let start = std::time::Instant::now();
    let mut check = 0u32;
    for _ in 0..rounds {
        let mut total = 0u64;
        for (packed, _, _) in &entries {
            if mode == "crc" {
                let mut hasher = crc32fast::Hasher::new();
                ours(packed, &mut window, |bytes| {
                    hasher.update(bytes);
                    total += bytes.len() as u64
                });
                check ^= hasher.finalize();
            } else {
                ours(packed, &mut window, |bytes| total += bytes.len() as u64);
            }
        }
        assert_eq!(total, size_total);
    }
    let nanos = start.elapsed().as_nanos() as f64;
    println!("{:.3}", nanos / rounds.max(1) as f64 / 1e6);
    std::hint::black_box(check);
}
