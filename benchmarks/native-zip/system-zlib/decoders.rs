use std::{io::Read, time::Instant};

fn stream<D: FnMut(&[u8], &mut [u8]) -> (usize, usize, bool)>(input: &[u8], mut step: D) -> u64 {
    let mut out = vec![0u8; 192 * 1024];
    let (mut at, mut total) = (0, 0u64);
    loop {
        let end = (at + 64 * 1024).min(input.len());
        let (used, made, done) = step(&input[at..end], &mut out);
        at += used;
        total += made as u64;
        if done { return total; }
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut raw = archive.by_index_raw(i).unwrap();
        if raw.is_dir() { continue; }
        let size = raw.size() as usize;
        let mut data = Vec::new();
        raw.read_to_end(&mut data).unwrap();
        entries.push((data, size));
    }
    let backend = if cfg!(feature = "ng") { "zlib-ng" } else if cfg!(feature = "system") { "system zlib" } else { "zlib-rs" };
    let decoders: Vec<(&str, Box<dyn Fn(&[u8], usize) -> u64>)> = vec![
        (backend, Box::new(|input, _| {
            let mut d = flate2::Decompress::new(false);
            stream(input, |i, o| { let (a, b) = (d.total_in(), d.total_out());
                let s = d.decompress(i, o, flate2::FlushDecompress::None).unwrap();
                ((d.total_in() - a) as usize, (d.total_out() - b) as usize, s == flate2::Status::StreamEnd) })
        })),
        ("libdeflate whole", Box::new(|input, size| {
            let mut out = vec![0u8; size];
            libdeflater::Decompressor::new().deflate_decompress(input, &mut out).unwrap() as u64
        })),
    ];
    let expected: u64 = entries.iter().map(|e| e.1 as u64).sum();
    for round in 0..6 {
        for (name, decode) in &decoders {
            let t = Instant::now();
            let total: u64 = entries.iter().map(|(data, size)| decode(data, *size)).sum();
            assert_eq!(total, expected);
            let crc = Instant::now();
            if round > 0 { println!("{name:20} {:7.1} ms", (crc - t).as_secs_f64() * 1e3); }
        }
    }
}
