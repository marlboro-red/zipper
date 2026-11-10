use zeroize::Zeroize;

const SCRUBBED_STACK: usize = 64 * 1024;

#[inline(never)]
pub(crate) fn scrub_stack() {
    let mut area = [0u8; SCRUBBED_STACK];
    area.zeroize();
    std::hint::black_box(&area);
}
