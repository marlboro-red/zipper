//! The fold of `crc.rs` with the accumulators kept in vector registers. `THREE_WAY` joins the
//! two products and the next block with one `eor3`.
use crate::crc::x_to_the;

pub fn update<const LANES: usize, const THREE_WAY: bool>(state: u32, data: &[u8]) -> u32 {
    // SAFETY: this probe is built only for targets whose baseline has `neon`, `aes` and `sha3`.
    unsafe { folded::<LANES, THREE_WAY>(state, data) }
}

#[target_feature(enable = "neon,aes,sha3")]
fn folded<const LANES: usize, const THREE_WAY: bool>(state: u32, data: &[u8]) -> u32 {
    use std::arch::aarch64::*;
    let stride = LANES * 16;
    if data.len() < 4 * stride {
        let mut hasher = crc32fast::Hasher::new_with_initial(state);
        hasher.update(data);
        return hasher.finalize();
    }
    let word = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().expect("8 bytes"));
    let load = |bytes: &[u8]| {
        vcombine_u64(
            vcreate_u64(word(&bytes[..8])),
            vcreate_u64(word(&bytes[8..16])),
        )
    };
    let earlier = const { x_to_the(LANES * 128 + 63) };
    let later = const { x_to_the(LANES * 128 - 1) };
    let both = vcombine_p64(vcreate_p64(earlier), vcreate_p64(later));
    let (first, rest) = data.split_at(stride);
    let mut lanes = [vdupq_n_u64(0); LANES];
    for (lane, bytes) in lanes.iter_mut().zip(first.chunks_exact(16)) {
        *lane = load(bytes);
    }
    lanes[0] = veorq_u64(
        lanes[0],
        vcombine_u64(vcreate_u64(u64::from(!state)), vcreate_u64(0)),
    );
    let mut strides = rest.chunks_exact(stride);
    for bytes in &mut strides {
        for (lane, bytes) in lanes.iter_mut().zip(bytes.chunks_exact(16)) {
            let held = vreinterpretq_p64_u64(*lane);
            let low = vreinterpretq_u64_p128(vmull_p64(vgetq_lane_p64::<0>(held), earlier));
            let high = vreinterpretq_u64_p128(vmull_high_p64(held, both));
            *lane = match THREE_WAY {
                true => veor3q_u64(low, high, load(bytes)),
                false => veorq_u64(veorq_u64(low, high), load(bytes)),
            };
        }
    }
    let mut hasher = crc32fast::Hasher::new_with_initial(!0);
    for lane in lanes {
        hasher.update(&vgetq_lane_u64::<0>(lane).to_le_bytes());
        hasher.update(&vgetq_lane_u64::<1>(lane).to_le_bytes());
    }
    hasher.update(strides.remainder());
    hasher.finalize()
}
