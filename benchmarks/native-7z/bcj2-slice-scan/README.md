# BCJ2 main stream copied a slice at a time

Kept. Copying the BCJ2 main stream a slice at a time between branch opcodes makes a BCJ2 7z of Silesia extract 1.38 times faster at one thread, 1.614 s against 2.225 s, with ranges apart. 7-Zip is still faster, at 1.311 s, and plain LZMA2 is unchanged.

## Question

Does scanning the main stream for branch opcodes, and copying the bytes between them in one call, remove the remaining per-byte cost of BCJ2? After the [buffered branch streams](../bcj2-buffered-branches/README.md), the decoder still pulled the main stream one byte at a time. Its instruction count was 31.1 G against 7-Zip's 11.9 G on the codec archive. The hypothesis was about 45% fewer instructions and a matching drop in wall time.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `main` is `dc60a40`, binary SHA-256 prefix `2283d2b452c667aa`. `scan` is `dc60a40` with the change shipped as `88d04c9`, prefix `c3d4539ee3c2f9c3`. Both are release builds. 7-Zip is 26.06, prefix `f45be05e8a8dbc25`.
- `7z-bcj2.7z` is the codec scoreboard's archive, Silesia (211.9 MB) through 7-Zip's BCJ2 filter and LZMA2, 49.3 MB. `silesia.7z` is plain solid LZMA2 of the same tree.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range, at one thread:

| Archive | `main` | `scan` | 7-Zip |
| --- | --- | --- | --- |
| BCJ2 | 2.225 (2.223 to 2.230) | 1.614 (1.609 to 1.616) | 1.311 (1.310 to 1.314) |
| Plain LZMA2 | 1.492 (1.489 to 1.494) | 1.494 (1.488 to 1.508) | not run |

CPU time follows wall time, 2.21 s against 1.60 s on BCJ2. Peak memory is 41 MiB in both. Retired instructions on the BCJ2 archive fell from 31.10 G to 17.06 G. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** `Bcj2` in `src/bcj2.rs` takes its main stream as a `BufRead`. It scans each slice for the next `E8`, `E9` or `0F 8x`, carrying the previous byte across slice edges, copies everything up to that opcode at once, and runs the range-coder step only there. `Exact` in `src/native_7z.rs` implements `BufRead` and never shows bytes past a branch's size.
- **Tests.** The old decoder is kept as a test-only reference, and two tests compare it with the new one on random and crafted streams split at every offset. Three planted breaks were each caught, one only after a test of a branch larger than its 1 MiB buffer was added. The Kani proof of `[bcj2-decoder]` covers a main stream in two slices cut at any point.
- **Review.** An adversarial review ran 83 archives, including refill edges at 1 MiB with opcodes on each side, and found nothing.

## Limits

- One machine and one BCJ2 archive. The earlier record measured on an Apple M5, so its numbers are not compared with these.
- The remaining gap to 7-Zip, 1.23 times, sits mostly in the LZMA2 decode of the main stream, which the LZMA records cover.

## Reproduce

Build `dc60a40`, and `dc60a40` after `git cherry-pick 88d04c9`, with `cargo build --release`. Then:

```sh
python3 scripts/compare_zip_readers.py target/bench-corpora/codecs/7z-bcj2.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader scan native <scan binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads main 1 --reader-threads scan 1 --reader-threads 7zip 1
```
