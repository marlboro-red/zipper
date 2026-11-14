# M23. The LZMA decoder as its own crate

Zipper's LZMA, LZMA2 and xz decoder is published on crates.io as a crate of its own, so other Rust programs can read these formats without liblzma.

The [tree-preload record](../../benchmarks/lzma/tree-preload/README.md) shows the decoder taking 350.0 ms on the probe stream against 398.4 ms for liblzma in the same runs. That is one Mac and one stream. The crate is worth publishing only if the speed holds on other machines and every use M21 covers.

M31 found that it does not. Zipper's decoder trails liblzma on x86-64 Linux by 1.09 to 1.20 times on Silesia and the dependency sources, as the [LZMA and xz record](../../benchmarks/decoders/lzma-and-xz/README.md) shows, so this milestone waits for [M35](m35-zstandard-and-lzma-at-c-speed-on-x86-64.md) to close that gap.

Scope:

- `src/lzma.rs` and `src/xz_filters.rs` move into a crate in this workspace, with Zipper depending on it. Its proofs, tests and coverage entries stay in this repository's gate.
- A public API of `Read` adapters for raw LZMA, LZMA2 and xz, with a memory limit, documented as a public contract.
- liblzma stays a test dependency of the crate, as the oracle for its differential tests.

Exit checks:

1. M21 is done, so Zipper itself reads every LZMA format with the crate and the release build has no liblzma.
2. A benchmark record compares the crate with liblzma on Linux x86-64, macOS arm64 and Windows, on the probe stream and on xz files of Silesia, with ranges apart or the verdict stated as a tie.
3. A fuzzing run of the public API on every format finds no panic, hang or memory beyond the limit, and its corpus is kept.
4. `cargo publish --dry-run` passes, and the crate's README states what it checks and what it does not.
