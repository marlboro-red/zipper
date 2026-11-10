# Zipper's LZMA decoder against sevenz-rust2

Zipper's decoder takes 0.79 times the time of lzma-rust2, the decoder inside sevenz-rust2, on one raw LZMA stream, and its ranges do not overlap. At one thread, Zipper extracts the three Silesia 7z archives 1.25 to 1.27 times faster than sevenz-rust2. The two tie on the dependency sources, and 7-Zip is still 7% faster than Zipper on Silesia.

## Question

Is Zipper's safe Rust LZMA decoder faster than sevenz-rust2, the pure Rust 7z library? Does lzma-rust2's `optimization` feature, which adds inline assembly and `unsafe` code, change the answer?

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026. No builders, Kani or TLC ran. `fseventsd` was restarted before the runs, and no timed run lists a busy process.
- Zipper is main at `6fbbf60`, release build, binary SHA-256 prefix `74a2a771ce33b0e5`. 7-Zip 26.05 for arm64 is the reference.
- sevenz-rust2 0.24.2 always turns on lzma-rust2 0.23.0's `optimization` feature. That feature adds an inline assembly `decode_direct_bits` on arm64 and x86-64, and word-at-a-time match code that uses `unsafe`. Without it, lzma-rust2 forbids `unsafe` code.
- [`harness/`](harness/src/main.rs) is a 30-line extractor on `ArchiveReader`, built with Zipper's release profile, binary prefix `92b172a137ceba76`. It writes file bytes only, with no times, modes, staging or path checks. `auto` leaves sevenz-rust2's default, one LZMA2 worker per core.
- The raw probe is `decode_speed_against_lzma_rust2` in `src/lzma.rs`. It decodes the first 64 MiB of Silesia, stored as raw LZMA1 at preset 6 with an 8 MiB dictionary. Each run has six rounds, alternating which decoder goes first, and checks both outputs against the input. Two test binaries were built:
  - `probe-asm`, with `optimization` on as sevenz-rust2 ships it, prefix `58ccc03f52c8e948`;
  - `probe-safe`, with it off, prefix `e2c2108cd9cdd356`.
- [`run-probe.py`](run-probe.py) ran the two binaries alternately for eight runs. It dropped round 0 of each run, which leaves 40 Zipper samples and 20 for each lzma-rust2 build.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 7 --cooldown 2` extracted five archives from `target/bench-work/7z-probe/`. Every reader ran at one thread and at its default, after one warmup round, in shuffled order. The script checks every extracted byte against the source tree. The archives:
  - `silesia.7z`, solid LZMA2, 49,597,394 bytes;
  - `silesia-lzma1.7z`, solid LZMA, 49,563,337 bytes;
  - `silesia-nonsolid.7z`, 49,608,000 bytes;
  - `dependencies.7z`, solid, 6,156,722 bytes;
  - `dependencies-nonsolid.7z`, 12,169,886 bytes.

## Results

The raw probe, from [`samples-probe.json`](samples-probe.json):

| Decoder | Samples | Median | Range |
| --- | ---: | ---: | ---: |
| Zipper | 40 | 346.3 ms | 343.1 to 353.7 ms |
| lzma-rust2 with `optimization` | 20 | 440.6 ms | 439.0 to 441.4 ms |
| lzma-rust2 without `optimization` | 20 | 444.1 ms | 443.4 to 445.6 ms |

Extraction at one thread, median wall seconds with the range, then median CPU seconds and peak memory:

| Archive | Zipper | sevenz-rust2 | 7-Zip |
| --- | --- | --- | --- |
| `silesia.7z` | 1.134 (1.130 to 1.157), 1.13 s, 36 MiB | 1.423 (1.419 to 1.427), 1.42 s, 37 MiB | 1.055 (1.045 to 1.066), 1.05 s, 36 MiB |
| `silesia-lzma1.7z` | 1.130 (1.126 to 1.145), 1.12 s, 36 MiB | 1.415 (1.404 to 1.423), 1.41 s, 38 MiB | 1.052 (1.045 to 1.055), 1.05 s, 36 MiB |
| `silesia-nonsolid.7z` | 1.122 (1.119 to 1.148), 1.12 s, 46 MiB | 1.427 (1.421 to 1.434), 1.42 s, 225 MiB | 1.047 (1.037 to 1.047), 1.04 s, 36 MiB |
| `dependencies.7z` | 0.435 (0.415 to 0.469), 0.43 s, 40 MiB | 0.427 (0.424 to 0.452), 0.42 s, 40 MiB | 0.439 (0.433 to 0.479), 0.43 s, 38 MiB |
| `dependencies-nonsolid.7z` | 0.655 (0.634 to 0.741), 0.65 s, 11 MiB | 0.684 (0.662 to 0.691), 0.68 s, 9 MiB | 0.610 (0.584 to 0.638), 0.60 s, 6 MiB |

Extraction at each reader's default thread count:

| Archive | Zipper | sevenz-rust2 | 7-Zip |
| --- | --- | --- | --- |
| `silesia.7z` | 0.734 (0.724 to 0.737), 1.19 s, 143 MiB | 0.932 (0.923 to 0.936), 1.50 s, 536 MiB | 0.687 (0.683 to 0.698), 1.09 s, 256 MiB |
| `silesia-lzma1.7z` | 1.131 (1.115 to 1.152), 1.13 s, 36 MiB | 1.410 (1.399 to 1.429), 1.41 s, 39 MiB | 1.054 (1.051 to 1.091), 1.05 s, 36 MiB |
| `silesia-nonsolid.7z` | 0.386 (0.383 to 0.397), 1.53 s, 187 MiB | 1.466 (1.464 to 1.476), 1.46 s, 328 MiB | 1.040 (1.037 to 1.046), 1.03 s, 75 MiB |
| `dependencies.7z` | 0.423 (0.414 to 0.428), 0.53 s, 40 MiB | 0.441 (0.430 to 0.477), 0.44 s, 186 MiB | 0.444 (0.432 to 0.477), 0.44 s, 76 MiB |
| `dependencies-nonsolid.7z` | 0.286 (0.278 to 0.324), 1.73 s, 24 MiB | 0.745 (0.737 to 0.769), 0.79 s, 16 MiB | 0.603 (0.600 to 0.631), 0.60 s, 6 MiB |

The raw JSON is `<archive>-t1.json` and `<archive>-tauto.json` beside this page.

## Decision

Kept as a record. The raw probe and the Silesia rows hold with no range overlap. LZMA decoding limits all three Silesia archives, since wall time equals CPU time at one thread. The `optimization` feature saves lzma-rust2 0.8% on the probe, so its assembly does not explain the gap. The dependency rows at one thread overlap, so they show no difference. At default threads, Zipper is 3.79 times faster than sevenz-rust2 on non-solid Silesia and 2.60 times faster on non-solid dependencies, because it decodes folders in parallel and sevenz-rust2 does not.

The probe and the `lzma-rust2` test dependency stay, so the comparison can be rerun when either decoder changes.

## Limits

- One Mac and one probe stream. Another CPU, dictionary or data set may change the ratio.
- The sevenz-rust2 extractor does less work than Zipper. It sets no times or modes, skips staging and publishing, and checks no paths. That favours sevenz-rust2, most on the many small files of the dependency sources.
- sevenz-rust2 decodes solid LZMA2 with one worker per core and peaks at 536 MiB, against 143 MiB for Zipper.
- lzma-rust2 has no x86-64 run here, where its assembly path differs.

## Reproduce

1. Build Zipper with `cargo build --release`, and the probe with `cargo test --release --lib --no-run`. Copy the probe to `target/bench-work/sr2/probe-asm`. Change `"optimization"` out of the `lzma-rust2` features in `Cargo.toml`, rebuild the probe, and copy it to `probe-safe`.
2. Run `python3 benchmarks/lzma/sevenz-rust2/run-probe.py target/bench-work/sr2`. The Silesia files must be in `target/bench-corpora/silesia/silesia`.
3. Build the extractor with `CARGO_TARGET_DIR=$PWD/target/sevenz-rust2-harness cargo build --release` inside `benchmarks/lzma/sevenz-rust2/harness`.
4. For each archive, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/silesia.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader zipper native target/release/zipper \
  --reader sevenz-rust2 sevenz-rust2 target/sevenz-rust2-harness/release/sevenz-rust2-extract \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads zipper 1 --reader-threads sevenz-rust2 1 --reader-threads 7zip 1
```

Use `auto` in place of `1` for the default thread counts.
