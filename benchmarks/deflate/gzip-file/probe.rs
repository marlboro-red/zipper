//! Checksum speeds for the gzip and zlib trailers: CRC-32 and Adler-32 candidates in safe Rust
//! against crc32fast and, with the `libdeflate` feature, libdeflate's own.
//!
//! Usage: checksum-probe FILE [ROUNDS]
//! It checks every candidate against its reference on many lengths and offsets, then times each
//! over the file in pieces of 32 KiB, 512 KiB and the whole file, and prints the best of ROUNDS.
use std::time::Instant;

mod adler;
mod crc;
mod crc_vectors;

#[cfg(feature = "libdeflate")]
mod libdeflate {
    unsafe extern "C" {
        fn libdeflate_crc32(crc: u32, buffer: *const u8, len: usize) -> u32;
        fn libdeflate_adler32(adler: u32, buffer: *const u8, len: usize) -> u32;
    }
    pub fn crc32(crc: u32, data: &[u8]) -> u32 {
        // SAFETY: the pointer and length describe `data`, which libdeflate only reads.
        unsafe { libdeflate_crc32(crc, data.as_ptr(), data.len()) }
    }
    pub fn adler32(adler: u32, data: &[u8]) -> u32 {
        // SAFETY: the pointer and length describe `data`, which libdeflate only reads.
        unsafe { libdeflate_adler32(adler, data.as_ptr(), data.len()) }
    }
}

fn crc32fast_update(state: u32, data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(state);
    hasher.update(data);
    hasher.finalize()
}

type Sum = (&'static str, u32, fn(u32, &[u8]) -> u32);

fn candidates() -> Vec<Sum> {
    #[cfg_attr(not(feature = "libdeflate"), expect(unused_mut))]
    let mut all: Vec<Sum> = vec![
        ("crc32 crc32fast 1.5.3", 0, crc32fast_update),
        ("crc32 fold 4 lanes", 0, crc::update::<4>),
        ("crc32 fold 8 lanes", 0, crc::update::<8>),
        ("crc32 fold 12 lanes", 0, crc::update::<12>),
        ("crc32 fold 16 lanes", 0, crc::update::<16>),
        ("crc32 vector fold 4", 0, crc_vectors::update::<4, false>),
        ("crc32 vector fold 8", 0, crc_vectors::update::<8, false>),
        ("crc32 vector fold 12", 0, crc_vectors::update::<12, false>),
        ("crc32 vector fold 4 eor3", 0, crc_vectors::update::<4, true>),
        ("crc32 vector fold 8 eor3", 0, crc_vectors::update::<8, true>),
        ("crc32 vector fold 12 eor3", 0, crc_vectors::update::<12, true>),
        ("crc32 vector fold 16 eor3", 0, crc_vectors::update::<16, true>),
        ("adler32 zipper 16 lanes", 1, adler::current),
        ("adler32 columns of 128", 1, adler::columns::<128>),
        ("adler32 columns of 32", 1, adler::columns::<32>),
        ("adler32 columns of 64", 1, adler::columns::<64>),
        ("adler32 nmax 16", 1, adler::nmax_blocks::<16>),
        ("adler32 nmax 32", 1, adler::nmax_blocks::<32>),
        ("adler32 nmax 64", 1, adler::nmax_blocks::<64>),
    ];
    #[cfg(feature = "libdeflate")]
    {
        all.push(("crc32 libdeflate", 0, libdeflate::crc32));
        all.push(("adler32 libdeflate", 1, libdeflate::adler32));
    }
    all
}

fn check(all: &[Sum]) {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut noise = vec![0u8; 70_016];
    for byte in &mut noise {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    let ones = vec![0xffu8; 70_016];
    for (name, start, sum) in all {
        let reference: fn(u32, &[u8]) -> u32 = match name.starts_with("crc32") {
            true => crc32fast_update,
            false => adler::bytewise,
        };
        for data in [&noise, &ones] {
            for length in (0..1500).chain([4095, 4096, 5551, 5552, 5553, 11_104, 65_535, 69_999]) {
                for offset in [0, 1, 7] {
                    let piece = &data[offset..offset + length];
                    assert_eq!(sum(*start, piece), reference(*start, piece), "{name} on {length} bytes");
                    let cut = length / 3;
                    let split = sum(sum(*start, &piece[..cut]), &piece[cut..]);
                    assert_eq!(split, reference(*start, piece), "{name} split at {cut} of {length}");
                }
            }
        }
    }
    println!("every candidate matches its reference");
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let path = arguments.next().expect("usage: checksum-probe FILE [ROUNDS]");
    let rounds: usize = arguments.next().map_or(7, |text| text.parse().expect("ROUNDS is a number"));
    let all = candidates();
    check(&all);
    let data = std::fs::read(path).expect("FILE reads");
    println!("{} bytes, best of {rounds} rounds, milliseconds for the whole file and GB/s", data.len());
    println!("{:28} {:>18} {:>18} {:>18}", "", "32 KiB pieces", "512 KiB pieces", "one piece");
    for (name, start, sum) in &all {
        let mut row = format!("{name:28}");
        let mut value = None;
        for piece in [32 << 10, 512 << 10, data.len()] {
            let mut best = f64::MAX;
            for _ in 0..rounds {
                let begun = Instant::now();
                let mut running = *start;
                for part in data.chunks(piece) {
                    running = sum(running, std::hint::black_box(part));
                }
                best = best.min(begun.elapsed().as_secs_f64());
                assert_eq!(*value.get_or_insert(running), running);
            }
            row += &format!(" {:8.2} ms {:5.1}", best * 1e3, data.len() as f64 / best / 1e9);
        }
        println!("{row}");
    }
}
