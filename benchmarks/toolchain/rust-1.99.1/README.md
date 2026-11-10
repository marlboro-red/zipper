# Rust 1.99.1 in place of 1.96.0

Kept. The same commit built with Rust 1.99.1 extracts every one of twelve Silesia workloads as fast as with 1.96.0, with ratios of 0.996 to 1.052 and every range overlapping. The upgrade is for one toolchain on every machine, not for speed.

## Question

Does moving the pinned toolchain from Rust 1.96.0 to 1.99.1 change extraction speed? The second machine's default 1.99.1 had broken a remote gate, and one toolchain everywhere was the reason to move.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026. No builders, Kani or TLC ran, and no timed run lists a busy process.
- Both binaries build main at `bd44b35` with the release profile, before any lint change:
  - `rust-1.96.0`, binary SHA-256 prefix `6539f8bf8364ecb3`, the `plain` binary of the [PGO record](../../pgo/lzma-and-readers/README.md);
  - `rust-1.99.1`, rustc `58cf8f9c3`, prefix `f626963195d25b26`.
- The twelve workloads, archives and settings are those of the PGO record. `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.
- Five workloads first refused to start while a commit hook compiled `xtask`. They ran again on the idle machine, and only those runs are recorded.

## Results

Median wall seconds with the range, at one thread unless the row says default threads:

| Workload | `rust-1.96.0` | `rust-1.99.1` | Ratio |
| --- | --- | --- | ---: |
| 7z LZMA2 solid | 1.132 (1.122 to 1.177) | 1.136 (1.132 to 1.171) | 1.003 |
| 7z LZMA solid | 1.135 (1.123 to 1.144) | 1.134 (1.123 to 1.146) | 1.000 |
| 7z non-solid, default threads | 0.390 (0.385 to 0.397) | 0.389 (0.383 to 0.399) | 0.998 |
| tar xz | 1.212 (1.200 to 1.219) | 1.210 (1.201 to 1.216) | 0.999 |
| tar lzip | 1.126 (1.114 to 1.139) | 1.128 (1.123 to 1.140) | 1.001 |
| tar BZip2 | 2.915 (2.902 to 2.932) | 2.903 (2.892 to 2.938) | 0.996 |
| tar gzip | 0.200 (0.187 to 0.202) | 0.201 (0.194 to 0.202) | 1.003 |
| tar zstd | 0.154 (0.151 to 0.156) | 0.154 (0.121 to 0.158) | 0.998 |
| ZIP Deflate | 0.223 (0.217 to 0.224) | 0.224 (0.214 to 0.225) | 1.004 |
| ZIP Deflate, default threads | 0.087 (0.081 to 0.098) | 0.091 (0.081 to 0.093) | 1.052 |
| ZIP LZMA | 1.149 (1.134 to 1.157) | 1.156 (1.148 to 1.159) | 1.006 |
| ZIP zstd | 0.175 (0.169 to 0.178) | 0.175 (0.167 to 0.179) | 1.005 |

The raw JSON is beside this page, one file per row.

## Decision

Kept. `rust-toolchain.toml` and every workflow now name 1.99.1. Clippy 1.99 added two lints that fail the gate:

- `byte_char_slices`, at five arrays of byte characters in tests, now written as byte strings.
- `chunks_exact_to_as_chunks`, at 47 sites, many inside parsers with Kani proofs. It is allowed in `Cargo.toml` until then, because rewriting those sites would make their proof sections stale and could change speed. Moving them to `as_chunks` is its own experiment.

One deprecation warning, `fetch_update`, became `try_update` in the ZIP output budget. The LZMA decoder uses no API newer than 1.96.

## Limits

- One Mac. A one-off run on a hosted x86-64 Linux runner times the same pair and will be recorded on its own.
- The binaries predate the lint changes, which touch tests and one atomic call outside the timed paths.

## Reproduce

Build `bd44b35` with `cargo +1.96.0 build --release` and with `CARGO_TARGET_DIR=target/rust-1.99.1 cargo +1.99.1 build --release`. Then run the commands in the [PGO record](../../pgo/lzma-and-readers/README.md), with the two binaries as the readers.
