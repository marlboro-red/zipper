//! CRC-32 by carry-less multiplication, in safe Rust.
//!
//! The message is a polynomial over GF(2), first bit highest. `LANES` 128-bit accumulators each
//! take every `LANES`th 16-byte block. Moving an accumulator `DISTANCE` bits ahead multiplies it
//! by `x^DISTANCE`, which is done modulo the CRC polynomial with two 64-bit carry-less products.
//! What is left, the accumulators and then the tail, has the CRC of the whole message, and
//! crc32fast finishes it.

#[cfg(all(target_arch = "aarch64", target_feature = "aes", target_feature = "neon"))]
#[target_feature(enable = "neon,aes")]
#[inline]
fn carryless(a: u64, b: u64) -> u128 {
    std::arch::aarch64::vmull_p64(a, b)
}

#[cfg(not(all(target_arch = "aarch64", target_feature = "aes", target_feature = "neon")))]
fn carryless(a: u64, b: u64) -> u128 {
    let mut product = 0u128;
    for bit in 0..64 {
        if b >> bit & 1 == 1 {
            product ^= u128::from(a) << bit;
        }
    }
    product
}

const POLYNOMIAL: u64 = 0x1_04c1_1db7;

/// `x^power` modulo the CRC-32 polynomial, bit-reflected into the high half of a word, as the
/// reflected carry-less product needs it.
pub const fn x_to_the(power: usize) -> u64 {
    let mut remainder = 1u64;
    let mut done = 0;
    while done < power {
        remainder <<= 1;
        if remainder >> 32 == 1 {
            remainder ^= POLYNOMIAL;
        }
        done += 1;
    }
    ((remainder as u32).reverse_bits() as u64) << 32
}

struct Fold<const LANES: usize>;

impl<const LANES: usize> Fold<LANES> {
    const STRIDE: usize = LANES * 16;
    const DISTANCE: usize = LANES * 128;
    const EARLIER_HALF: u64 = x_to_the(Self::DISTANCE + 63);
    const LATER_HALF: u64 = x_to_the(Self::DISTANCE - 1);
}

fn block(bytes: &[u8]) -> u128 {
    u128::from_le_bytes(bytes.try_into().expect("16 bytes"))
}

pub fn update<const LANES: usize>(state: u32, data: &[u8]) -> u32 {
    // SAFETY: this probe is built only for targets whose baseline has `neon` and `aes`.
    unsafe { folded::<LANES>(state, data) }
}

#[cfg_attr(target_arch = "aarch64", target_feature(enable = "neon,aes"))]
fn folded<const LANES: usize>(state: u32, data: &[u8]) -> u32 {
    let stride = Fold::<LANES>::STRIDE;
    if data.len() < 4 * stride {
        let mut hasher = crc32fast::Hasher::new_with_initial(state);
        hasher.update(data);
        return hasher.finalize();
    }
    let (first, rest) = data.split_at(stride);
    let mut lanes = [0u128; LANES];
    for (lane, bytes) in lanes.iter_mut().zip(first.chunks_exact(16)) {
        *lane = block(bytes);
    }
    lanes[0] ^= u128::from(!state);
    let mut strides = rest.chunks_exact(stride);
    for bytes in &mut strides {
        for (lane, bytes) in lanes.iter_mut().zip(bytes.chunks_exact(16)) {
            *lane = carryless(*lane as u64, Fold::<LANES>::EARLIER_HALF)
                ^ carryless((*lane >> 64) as u64, Fold::<LANES>::LATER_HALF)
                ^ block(bytes);
        }
    }
    let mut hasher = crc32fast::Hasher::new_with_initial(!0);
    for lane in lanes {
        hasher.update(&lane.to_le_bytes());
    }
    hasher.update(strides.remainder());
    hasher.finalize()
}
