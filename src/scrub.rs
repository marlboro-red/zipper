#[cfg(any(feature = "zip-encryption", feature = "7z"))]
use zeroize::Zeroize;

const SCRUBBED_STACK: usize = 64 * 1024;

#[cfg(any(feature = "zip-encryption", feature = "7z"))]
#[inline(never)]
pub(crate) fn scrub_stack() {
    let mut area = [0u8; SCRUBBED_STACK];
    area.zeroize();
    std::hint::black_box(&area);
}

#[cfg(not(any(feature = "zip-encryption", feature = "7z")))]
pub(crate) use without_decryption::scrub_stack;

#[cfg(not(any(feature = "zip-encryption", feature = "7z")))]
mod without_decryption {
    pub(crate) fn scrub_stack() {}
}
