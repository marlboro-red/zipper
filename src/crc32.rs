#[derive(Clone, Default)]
pub(crate) struct Crc32 {
    sum: u32,
}

impl Crc32 {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        #[cfg(target_arch = "aarch64")]
        if bytes.len() >= folding::SHORTEST && folding::cpu_folds() {
            // SAFETY: `cpu_folds` found `neon`, `aes` and `sha3` on this CPU.
            self.sum = unsafe { folding::folded(self.sum, bytes) };
            return;
        }
        self.sum = continued(self.sum, bytes);
    }

    pub(crate) fn finalize(self) -> u32 {
        self.sum
    }
}

fn continued(sum: u32, bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(sum);
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(target_arch = "aarch64")]
mod folding {
    use super::continued;
    use std::arch::aarch64::{
        uint64x2_t, vcombine_p64, vcombine_u64, vcreate_p64, vcreate_u64, vdupq_n_u64, veor3q_u64,
        veorq_u64, vgetq_lane_p64, vgetq_lane_u64, vmull_high_p64, vmull_p64,
        vreinterpretq_p64_u64, vreinterpretq_u64_p128,
    };

    // RECORD(benchmarks/deflate/gzip-file/README.md): no fold of 4 to 16 lanes timed on an
    // M2 Max was faster than 8 lanes joined by `eor3`.
    const LANES: usize = 8;
    const WORD: usize = 8;
    const BLOCK: usize = 2 * WORD;
    const STRIDE: usize = LANES * BLOCK;
    pub(super) const SHORTEST: usize = 4 * STRIDE;

    const POLYNOMIAL: u64 = 0x1_04c1_1db7;
    const SUM_OF_A_ZERO_REGISTER: u32 = !0;
    const BITS_A_REFLECTED_PRODUCT_COMES_OUT_LOW: usize = 1;
    const EARLIER_WORD_A_STRIDE_ON: u64 =
        x_to_the((STRIDE + WORD) * 8 - BITS_A_REFLECTED_PRODUCT_COMES_OUT_LOW);
    const LATER_WORD_A_STRIDE_ON: u64 =
        x_to_the(STRIDE * 8 - BITS_A_REFLECTED_PRODUCT_COMES_OUT_LOW);

    const fn x_to_the(power: usize) -> u64 {
        let mut remainder = 1u64;
        let mut multiplied = 0;
        while multiplied < power {
            remainder <<= 1;
            if remainder >> 32 == 1 {
                remainder ^= POLYNOMIAL;
            }
            multiplied += 1;
        }
        ((remainder as u32).reverse_bits() as u64) << 32
    }

    pub(super) fn cpu_folds() -> bool {
        std::arch::is_aarch64_feature_detected!("neon")
            && std::arch::is_aarch64_feature_detected!("aes")
            && std::arch::is_aarch64_feature_detected!("sha3")
    }

    fn word(bytes: &[u8]) -> u64 {
        u64::from_le_bytes(bytes.try_into().expect("eight bytes"))
    }

    #[target_feature(enable = "neon,aes,sha3")]
    fn block(bytes: &[u8]) -> uint64x2_t {
        vcombine_u64(
            vcreate_u64(word(&bytes[..WORD])),
            vcreate_u64(word(&bytes[WORD..BLOCK])),
        )
    }

    #[target_feature(enable = "neon,aes,sha3")]
    pub(super) fn folded(sum: u32, bytes: &[u8]) -> u32 {
        let a_stride_on = vcombine_p64(
            vcreate_p64(EARLIER_WORD_A_STRIDE_ON),
            vcreate_p64(LATER_WORD_A_STRIDE_ON),
        );
        let (first, rest) = bytes.split_at(STRIDE);
        let mut lanes = [vdupq_n_u64(0); LANES];
        for (lane, bytes) in lanes.iter_mut().zip(first.chunks_exact(BLOCK)) {
            *lane = block(bytes);
        }
        let register = vcombine_u64(vcreate_u64(u64::from(!sum)), vcreate_u64(0));
        lanes[0] = veorq_u64(lanes[0], register);
        let mut strides = rest.chunks_exact(STRIDE);
        for stride in &mut strides {
            for (lane, bytes) in lanes.iter_mut().zip(stride.chunks_exact(BLOCK)) {
                let held = vreinterpretq_p64_u64(*lane);
                let earlier = vmull_p64(vgetq_lane_p64::<0>(held), EARLIER_WORD_A_STRIDE_ON);
                let later = vmull_high_p64(held, a_stride_on);
                *lane = veor3q_u64(
                    vreinterpretq_u64_p128(earlier),
                    vreinterpretq_u64_p128(later),
                    block(bytes),
                );
            }
        }
        let mut congruent = [0u8; STRIDE];
        for (lane, bytes) in lanes.iter().zip(congruent.chunks_exact_mut(BLOCK)) {
            bytes[..WORD].copy_from_slice(&vgetq_lane_u64::<0>(*lane).to_le_bytes());
            bytes[WORD..].copy_from_slice(&vgetq_lane_u64::<1>(*lane).to_le_bytes());
        }
        continued(
            continued(SUM_OF_A_ZERO_REGISTER, &congruent),
            strides.remainder(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(length: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn bit_at_a_time(bytes: &[u8]) -> u32 {
        let mut register = u32::MAX;
        for &byte in bytes {
            register ^= u32::from(byte);
            for _ in 0..8 {
                register = register >> 1 ^ 0xedb8_8320 & (register & 1).wrapping_neg();
            }
        }
        !register
    }

    #[test]
    fn every_length_offset_and_split_gives_the_bit_at_a_time_crc() {
        let longest = 1700;
        for data in [
            noise(longest + 64),
            vec![0xff; longest + 64],
            vec![0; longest + 64],
        ] {
            for length in 0..=longest {
                for offset in [0, 1, 7, 15] {
                    let bytes = &data[offset..offset + length];
                    let expected = bit_at_a_time(bytes);
                    let mut whole = Crc32::default();
                    whole.update(bytes);
                    assert_eq!(whole.finalize(), expected, "{length} bytes from {offset}");
                    for split in [length / 3, length.saturating_sub(513), length.min(512)] {
                        let mut parts = Crc32::default();
                        parts.update(&bytes[..split]);
                        parts.update(&bytes[split..]);
                        assert_eq!(parts.finalize(), expected, "{length} split at {split}");
                    }
                }
            }
        }
    }

    #[test]
    fn long_inputs_in_pieces_of_every_kind_give_crc32fasts_sum() {
        let data = noise(3_000_000);
        let expected = crc32fast::hash(&data);
        assert_eq!(
            bit_at_a_time(&data[..100_000]),
            crc32fast::hash(&data[..100_000])
        );
        for piece in [
            1,
            127,
            128,
            511,
            512,
            513,
            4096,
            65_537,
            544 * 1024,
            data.len(),
        ] {
            let mut crc = Crc32::default();
            for part in data.chunks(piece) {
                crc.update(part);
            }
            assert_eq!(crc.finalize(), expected, "pieces of {piece}");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn the_fold_runs_on_this_cpu_and_takes_every_length_it_is_given() {
        if !folding::cpu_folds() {
            println!("skipped: this CPU lacks aes or sha3");
            return;
        }
        let data = noise(folding::SHORTEST + 700);
        for length in folding::SHORTEST..data.len() {
            for start in [0u32, 1, 0xdead_beef, u32::MAX] {
                // SAFETY: `cpu_folds` found `neon`, `aes` and `sha3` on this CPU.
                let folded = unsafe { folding::folded(start, &data[..length]) };
                assert_eq!(
                    folded,
                    continued(start, &data[..length]),
                    "{length} from {start:#x}"
                );
            }
        }
    }
}
