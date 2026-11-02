# System zlib for streamed Deflate on macOS

**On macOS, streaming Deflate through Apple's system zlib extracts Silesia 1.23× faster at one thread and 1.18× faster at four, with 22% less CPU at four threads.** Dependency sources gain 4% at one thread. Linux and Windows keep zlib-rs, and this record measures only macOS.

## Question

At one thread, native extraction of Silesia took 240.8 ms, and 218.7 ms of that was spent without writing anything. bsdtar took 184 ms. Is the streaming decoder the gap, and does a faster one keep memory bounded?

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. Release builds.
- First, a decoder probe outside Zipper decoded every Silesia entry in memory on one thread, six rounds. Its source is [`decoders.rs`](decoders.rs) with [`decoders.Cargo.toml`](decoders.Cargo.toml), built once per flate2 backend.
- Baseline is `c7b019b`. The candidate adds a macOS-only dependency that enables flate2's `zlib` feature. flate2 prefers C zlib over zlib-rs when both are enabled, and the binary then links `/usr/lib/libz.1.dylib`.
- `scripts/compare_zip_readers.py` ran both binaries with `native` at one and four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.

## Results

Decoder probe, Silesia entries decoded in memory on one thread, ranges across five measured rounds:

| Decoder | Milliseconds | Bounded memory |
| --- | ---: | --- |
| libdeflate, whole entry | 146.0–150.6 | No, it needs the whole entry in memory |
| Apple system zlib, streaming | 158.6–162.6 | Yes |
| zlib-rs, streaming | 206.0–211.1 | Yes |
| zlib-ng, streaming | 244.1–245.1 | Yes |

Extraction, medians of seven rounds with the wall range. Raw samples are in this folder.

| Workload | Threads | Before ms | After ms | CPU ms before / after |
| --- | ---: | ---: | ---: | ---: |
| Silesia | 1 | 240.8 [240.2–260.0] | 195.9 [193.7–216.8] | 239.0 / 192.2 |
| Silesia | 4 | 88.5 [87.2–89.2] | 75.0 [68.6–81.9] | 257.0 / 200.8 |
| Dependency sources | 1 | 341.9 [339.4–361.0] | 327.7 [326.0–338.0] | 338.1 / 325.1 |
| Dependency sources | 4 | 187.3 [185.0–193.2] | 185.4 [182.6–208.6] | 600.6 / 592.4 |
| 10,000 flat tiny files | 1 | 528.4 [505.3–569.1] | 521.2 [496.6–529.7] | 496.7 / 494.4 |
| 10,000 flat tiny files | 4 | 339.2 [325.3–348.7] | 341.8 [330.8–345.1] | 1,177.1 / 1,174.9 |
| 256 MiB compressible | 1 | 62.9 [60.1–98.2] | 60.3 [60.0–94.4] | 58.7 / 58.8 |
| 256 MiB compressible | 4 | 45.2 [44.8–48.3] | 45.0 [44.9–47.1] | 61.4 / 61.5 |

Peak memory did not change on any row.

## Decision

Kept on macOS. Silesia at both thread counts and dependency sources at one thread beat their baseline ranges. Every other range overlaps, so the change is neutral there.

A 50,000-case run of `tests/zip_fuzz.rs` with seed 2000000 passed with the new decoder, and the hostile ZIP tests pass in the gate.

Silesia at one thread is still about 12 ms behind bsdtar's 184 ms. The decoder is the same library, so the rest is the copy path and the CRC.

## Limits

One Mac, warm caches, one session. The archives come from Python `zipfile` at level 6. Linux and Windows still use zlib-rs, and zlib-ng was slower here than on its own benchmarks, so neither result transfers. The system zlib version comes from the operating system, so a macOS update can change these numbers.

## Reproduce

```sh
cargo xtask bench --only silesia dependencies flat-tiny large-compressible
python3 scripts/compare_zip_readers.py target/bench-work/silesia.zip \
  target/bench-corpora/silesia/silesia \
  --reader before native zipper-before --reader after native zipper-after \
  --runs 7 --threads 1 > silesia-1.json
```

Build `zipper-before` at `c7b019b` and `zipper-after` at this commit. For the probe, copy `decoders.rs` to `src/main.rs` beside `decoders.Cargo.toml` renamed to `Cargo.toml`, then build with `--no-default-features --features rs`, `system` or `ng`.
