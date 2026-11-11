//! Adler-32 candidates in safe Rust. `current` is the code in Zipper's `src/framing.rs`.

const MODULUS: u32 = 65521;
const BYTES_BEFORE_A_REDUCTION: usize = 5552;

pub fn bytewise(state: u32, bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (state & 0xffff, state >> 16);
    for &byte in bytes {
        a = (a + u32::from(byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    b << 16 | a
}

pub fn current(state: u32, bytes: &[u8]) -> u32 {
    const LANES: usize = 16;
    let (mut a, mut b) = (state & 0xffff, state >> 16);
    for run in bytes.chunks(BYTES_BEFORE_A_REDUCTION) {
        let mut lanes = run.chunks_exact(LANES);
        for lane in &mut lanes {
            let (mut sum, mut weighted) = (0u32, 0u32);
            for (index, &byte) in lane.iter().enumerate() {
                sum += u32::from(byte);
                weighted += (LANES - index) as u32 * u32::from(byte);
            }
            b += LANES as u32 * a + weighted;
            a += sum;
        }
        for &byte in lanes.remainder() {
            a += u32::from(byte);
            b += a;
        }
        a %= MODULUS;
        b %= MODULUS;
    }
    b << 16 | a
}

/// The same sums with wider blocks, so one block's `sum` and `weighted` fill whole vectors.
pub fn nmax_blocks<const BLOCK: usize>(state: u32, bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (state & 0xffff, state >> 16);
    let run_length = BYTES_BEFORE_A_REDUCTION / BLOCK * BLOCK;
    for run in bytes.chunks(run_length) {
        let mut blocks = run.chunks_exact(BLOCK);
        for block in &mut blocks {
            let (mut sum, mut weighted) = (0u32, 0u32);
            for (index, &byte) in block.iter().enumerate() {
                sum += u32::from(byte);
                weighted += (BLOCK - index) as u32 * u32::from(byte);
            }
            b += BLOCK as u32 * a + weighted;
            a += sum;
        }
        for &byte in blocks.remainder() {
            a += u32::from(byte);
            b += a;
        }
        a %= MODULUS;
        b %= MODULUS;
    }
    b << 16 | a
}

/// One 16-bit sum for each byte position of a block, weighted once a run, as NEON code does it.
/// A run of 5,552 bytes adds a position at most 347 times, and 347 times 255 fits 16 bits.
pub fn columns<const BLOCK: usize>(state: u32, bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (state & 0xffff, state >> 16);
    let run_length = BYTES_BEFORE_A_REDUCTION / BLOCK * BLOCK;
    for run in bytes.chunks(run_length) {
        let mut blocks = run.chunks_exact(BLOCK);
        let mut columns = [0u16; BLOCK];
        let mut a_before_each_block = 0u32;
        for block in &mut blocks {
            let block: &[u8; BLOCK] = block.try_into().expect("a whole block");
            a_before_each_block += a;
            let mut sum = 0u32;
            for index in 0..BLOCK {
                sum += u32::from(block[index]);
                columns[index] += u16::from(block[index]);
            }
            a += sum;
        }
        b += BLOCK as u32 * a_before_each_block;
        for (index, &column) in columns.iter().enumerate() {
            b += (BLOCK - index) as u32 * u32::from(column);
        }
        for &byte in blocks.remainder() {
            a += u32::from(byte);
            b += a;
        }
        a %= MODULUS;
        b %= MODULUS;
    }
    b << 16 | a
}
