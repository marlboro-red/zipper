# Every Deflate decode on Zipper's own decoder, with no zlib

Kept. Moving 7z, gzip, CAB, DMG, SquashFS, CramFS, SWF, xar, HFS+, APFS and the disk images onto Zipper's own Deflate decoder takes flate2, zlib-rs and the macOS system zlib out of the build. On an M2 Max, 7z Deflate extracts 1.07 times faster at one thread and Silesia `.tar.gz` at level 1 1.05 times faster, with ranges apart, and no row got slower.

## Question

Can every remaining Deflate decode move to `src/inflate.rs`, so the release build links no zlib on any platform, without a slower workload? The [own Deflate decoder record](../../native-zip/own-inflate/README.md) moved ZIP alone and left flate2 for every other reader, with Apple's system zlib on macOS and zlib-rs elsewhere. The hypothesis was the same speed or better, since the decoder beat both in cycles.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `zlib` is main at `25dcb88`, binary SHA-256 prefix `b754ba8cd9129d26`. `own` is `25dcb88` with the commits main has from `c3cf377` to `04b5761`, prefix of the timed binary `c6f115f2599bc359`. The QCOW and SquashFS changes after `04b5761` touch neither timed path.
- The archives are Silesia (211.9 MB) as `.tar.gz` at `gzip` levels 1, 6 and 9 (77.3, 68.2 and 67.6 MB) and as a 7z of Deflate written by 7-Zip at `-mx6` (65.6 MB).
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall milliseconds with the range:

| Archive | Threads | `zlib` | `own` |
| --- | --- | --- | --- |
| 7z Deflate | 1 | 284.9 (278.8 to 289.2) | 265.6 (259.6 to 269.0) |
| 7z Deflate | default | 117.5 (116.2 to 119.6) | 112.9 (110.0 to 115.3) |
| `.tar.gz`, level 1 | 1 | 272.7 (271.4 to 338.1) | 259.8 (258.3 to 264.9) |
| `.tar.gz`, level 6 | 1 | 252.3 (247.3 to 253.3) | 246.2 (244.4 to 248.2) |
| `.tar.gz`, level 6 | default | 253.1 (250.4 to 253.3) | 246.8 (245.0 to 248.3) |
| `.tar.gz`, level 9 | 1 | 249.3 (247.4 to 253.4) | 246.5 (244.0 to 248.5) |

The 7z rows, the level 1 row and the level 6 row at default threads have ranges apart. Peak memory matched or fell, from 14 to 10 MiB on the level 1 row. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** `src/framing.rs` reads raw Deflate, zlib and multi-member gzip over `Inflate`, with every check flate2 made. Those are the zlib header and Adler-32, and the gzip header flags, header CRC, CRC-32 and size trailer. CAB MSZIP starts each block from the previous 32 KiB. QCOW follows QEMU's rule and accepts a cluster that fills before its stream ends. `cargo xtask gate` now fails if the normal dependency tree on any target names a zlib crate, or if the macOS binary links `libz`.
- **Tests.** Each caller is compared with the old flate2 code on generated streams with cuts and flips, the gzip corpus is compared whole and at every cut, and a Kani proof covers the gzip header reader. 20 test plants, 2 Kani plants and 3 plants of the zlib check were each caught.
- **Review.** An adversarial review of about 4,000 inputs confirmed nothing. Its one plausible finding, an interrupted read at a gzip trailer, failed a test first and is fixed.

## Limits

- One machine and one corpus, and only the gzip and 7z paths were timed. CAB, DMG, SquashFS, CramFS and the disk images rely on their corpus tests and counters.
- SquashFS keeps one 64 KiB window per image, about 33 to 60 KB of footprint above the flate2 build, which does not grow with the image.
- Some decode-error messages changed, and a reader now stays failed after an error where flate2 sometimes returned the end of the stream.

## Reproduce

Build `25dcb88`, and `25dcb88` after `git cherry-pick c3cf377^..04b5761`, with `cargo build --release`. Then, for each archive:

```sh
python3 scripts/compare_zip_readers.py silesia-1.tar.gz target/bench-corpora/silesia/silesia \
  --archive-format native --runs 7 --cooldown 2 --work-dir "$PWD/target/bench-work" \
  --reader zlib native <zlib binary> --reader own native <own binary> \
  --reader-threads zlib 1 --reader-threads own 1
```

Use `--archive-format 7z` for the 7z rows and `auto` for the default rows.
