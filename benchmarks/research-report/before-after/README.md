# Before and after the research report's changes

Every workload the research report's kept changes target is faster, from 1.10 times on small-file ZIPs to 8.33 times on long zero runs. No other workload moved. On the same machine Zipper now beats 7-Zip 26.06 on every ZIP row, on solid small-file 7z and on zero runs. It still trails on plain LZMA 7z by 1.08 to 1.10 times and on BZip2 by 1.51 times.

## Question

What did the changes kept under [M24](../../../docs/roadmap/m24-zip-and-7z-throughput-from-the-research-report.md) buy together, and did any workload get slower? Each change has its own record with its own baseline. This one compares main before the first of them with main after the last, on one machine, with 7-Zip in every row.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `before` is main at `b98b9ee`, binary SHA-256 prefix `a08cb2c5271e1228`, the last commit before the first change. `after` is main at `00928e1`, prefix `1bb002946cc58c82`. 7-Zip is 26.06, prefix `f45be05e8a8dbc25`.
- Between them, main gained these changes:
  - [buffered BCJ2 branch streams](../../native-7z/bcj2-buffered-branches/README.md) and [the BCJ2 slice scan](../../native-7z/bcj2-slice-scan/README.md);
  - [one Deflate64 decoder per ZIP entry](../../native-zip/deflate64-one-inflater/README.md);
  - [the doubling copy for short-distance LZMA matches](../../lzma/repeating-copy/README.md);
  - [parallel LZMA2 segments behind filters](../../native-7z/filtered-lzma2-segments/README.md);
  - [the macOS small-files cap](../../native-zip/small-files-cap/README.md);
  - [the solid-folder writer pool](../../native-7z/solid-writer-pool/README.md);
  - [each 7z folder created once](../../native-7z/create-folders-once/README.md).

  Main also dropped `native-fast`, which no row here uses, and gained per-format features, which leave the default build's code the same.
- The archives are those of the change records and the codec scoreboard. Most hold Silesia (211.9 MB) or the dependency sources (67.5 MB). The rest hold the mixed tree, 10,000 flat tiny files or 256 MiB of zeros.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall milliseconds. "Apart" means the before and after ranges do not overlap.

| Workload | Threads | `before` | `after` | Ratio | 7-Zip | `after` ÷ 7-Zip |
| --- | --- | ---: | ---: | --- | ---: | ---: |
| 7z of zeros | 1 | 1,108 | 133 | 8.33, apart | 153 | 0.87 |
| 7z BCJ2 | 1 | 4,595 | 1,615 | 2.85, apart | 1,312 | 1.23 |
| 7z BCJ2 | default | 4,602 | 1,614 | 2.85, apart | 1,314 | 1.23 |
| ZIP Deflate64 | 1 | 955 | 519 | 1.84, apart | 675 | 0.77 |
| 7z dependency sources, solid | default | 666 | 403 | 1.65, apart | 661 | 0.61 |
| 7z ARM64 filter | default | 1,257 | 973 | 1.29, apart | 906 | 1.07 |
| ZIP mixed tree | default | 238 | 216 | 1.11, apart | 450 | 0.48 |
| ZIP dependency sources | default | 359 | 326 | 1.10, apart | 677 | 0.48 |
| 7z dependency sources, solid | 1 | 647 | 639 | 1.01 | 657 | 0.97 |
| 7z dependency sources, non-solid | default | 395 | 392 | 1.01 | 871 | 0.45 |
| 7z BCJ filter | default | 573 | 572 | 1.00 | 512 | 1.12 |
| 7z Silesia | 1 | 1,497 | 1,493 | 1.00 | 1,356 | 1.10 |
| 7z Silesia | default | 976 | 972 | 1.00 | 899 | 1.08 |
| ZIP flat tiny files | default | 430 | 429 | 1.00 | 882 | 0.49 |
| ZIP Silesia | 1 | 275 | 277 | 0.99 | 708 | 0.39 |
| ZIP Silesia | default | 119 | 117 | 1.02 | 708 | 0.16 |

The codec rows, at one thread:

| Codec | `before` | `after` | 7-Zip | `after` ÷ 7-Zip |
| --- | ---: | ---: | ---: | ---: |
| 7z BZip2 | 3,488 | 3,486 | 2,313 | 1.51 |
| 7z PPMd | 10,879 | 10,880 | 10,585 | 1.03 |
| 7z Swap4 | 1,864 | 1,863 | 1,591 | 1.17 |
| ZIP BZip2 | 3,490 | 3,489 | 2,310 | 1.51 |
| ZIP LZMA | 1,514 | 1,511 | 1,349 | 1.12 |
| ZIP XZ | 1,521 | 1,520 | 1,380 | 1.10 |
| ZIP Zstandard | 230 | 228 | 242 | 0.94 |
| ZIP PPMd | 12,237 | 12,239 | 11,625 | 1.05 |

Every row without a ratio marked apart has overlapping ranges. Peak memory changed on two rows. It rose from 38 to 142 MiB on the ARM64 filter, where segments now decode in parallel, and from 42 to 48 MiB on the solid dependency sources, from the writer pool. The raw JSON is beside this page, one file per row.

## Decision

Kept as the milestone's summary record. Each change has its own decision. Together they make no measured workload slower, so nothing is reverted.

## Limits

- One machine. The change records measured some steps on an Apple M5, and those numbers are not compared with these.
- One archive per shape. The gains depend on the shape. The ARM64 row gains only because its stream has several LZMA2 blocks, and the solid row gains only at more than one thread.
- 7-Zip ran with its own default threads on the `default` rows and one thread on the others.

## Reproduce

Build `b98b9ee` and `00928e1` with `cargo build --release`. Then, for each row, run its archive and source folder with:

```sh
python3 scripts/compare_zip_readers.py ARCHIVE SOURCE --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native <before binary> --reader after native <after binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1
```

Use `--archive-format zip` for ZIPs and `auto` for the default rows.
