# Doubling copy for long LZMA matches at a short distance

Kept. A 7z of 256 MiB of zeros extracts 7.3 times faster at one thread, 0.086 s against 0.630 s, and now faster than 7-Zip at 0.117 s. Silesia is unchanged, 1.149 s against 1.147 s, with ranges overlapping.

## Question

Can long matches at a distance below 16 stop costing one `memmove` per period, without slowing Silesia? `Window::copy` in `src/lzma.rs` copied such a match one period at a time, so a 273-byte run at distance 1 was 273 calls. A research probe of a doubling copy cut a zero-run 7z from 14.95 G to 0.91 G instructions. The same loop inlined into the kernel made Silesia 4.4% slower, so the hypothesis was a large gain on zero runs from a cold helper that leaves the hot path as it is.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `base` is main at `da6eb8d`, binary SHA-256 prefix `5625aac77f2ba5d2`. `repeat` is `bb17cd7`, prefix `1fcdc258bf722e53`, one change on `da6eb8d`. `src/lzma.rs` did not change between them otherwise. Both are release builds. 7-Zip is 26.05.
- `zeros.7z` holds one file of 268,435,456 zero bytes as LZMA2, 39,329 bytes, written by `7zz a -t7z -m0=LZMA2 -mx6`. `silesia-lzma1.7z` is Silesia (211.9 MB) as solid LZMA, 49.6 MB, from the [sevenz-rust2 record](../sevenz-rust2/README.md).
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range, at one thread:

| Archive | `base` | `repeat` | 7-Zip |
| --- | --- | --- | --- |
| Zeros | 0.630 (0.583 to 0.673) | 0.086 (0.069 to 0.099) | 0.117 (0.105 to 0.151) |
| Silesia LZMA | 1.147 (1.133 to 1.160) | 1.149 (1.135 to 1.161) | not run |

CPU time follows wall time, 0.63 s against 0.08 s on zeros. Peak memory is unchanged, 69 MiB on zeros and 37 MiB on Silesia. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** A match whose distance is below 16 and whose run is longer than the distance goes to `Window::copy_repeating`, marked `#[cold]` and `#[inline(never)]`. It fills when the distance is 1 and otherwise doubles the copied span each step. Every other match takes the old single `copy_within`.
- **Tests.** An exhaustive test compares distances 1 to 15, runs up to 273, every window position and three history sizes against a byte-at-a-time copy, over a million cases. Two planted breaks in the helper were each caught.
- **Review.** An adversarial review ran 110 byte-exact extractions of xz, lzma, tar.xz, 7z LZMA and LZMA2 and ZIP LZMA archives built to force long short-distance matches, and found nothing.

## Limits

- One Mac and one synthetic zero archive. Real data with long zero runs, such as disk images and padded binaries, gains less than pure zeros.
- The helper serves every LZMA reader, including tar xz, ZIP LZMA and XZ, and SquashFS. Only 7z was timed.

## Reproduce

Build `da6eb8d` and `bb17cd7` with `cargo build --release`. Make the archive with `head -c 268435456 /dev/zero > zeros/zeros.bin` and `7zz a -t7z -m0=LZMA2 -mx6 zeros.7z zeros`. Then:

```sh
python3 scripts/compare_zip_readers.py zeros.7z zeros --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader repeat native <repeat binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads base 1 --reader-threads repeat 1 --reader-threads 7zip 1
```

Use `target/bench-work/7z-probe/silesia-lzma1.7z` and the Silesia folder for the second row.
