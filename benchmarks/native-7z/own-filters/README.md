# 7z branch and Delta filters on Zipper's own code

**Kept. Decoding the 7z branch and Delta filters with `src/xz_filters.rs` instead of liblzma leaves extraction time unchanged on four filter archives at one and four threads, with every range overlapping.** It removes liblzma from the 7z reader outside tests. The archive meant to hold a chain longer than liblzma allows held only three filters, so that new native path was not timed.

## Question

The 7z reader ran its branch filters (BCJ, ARM64 and the rest) and Delta through liblzma's raw decoder, over a stream that wrapped Zipper's own LZMA2 output. The change decodes them with Zipper's filters, which nine Kani proofs cover. Does 7z extraction stay as fast? The metric is the median wall time of `zipper extract` with the default engine, with CPU time and peak memory beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off, no agents running.
- `before` is main at `fd585d0`, binary SHA-256 prefix `b9637555412c32fe`. `after` is the change at `f8013cc`, prefix `0066b9a432069045`. 7-Zip 26.05 for arm64 is the reference.
- Four solid archives written by 7-Zip 26.05 in `target/bench-work/7z-filters/`:
  - `bcj.7z`, BCJ then LZMA2, over Silesia `ooffice` and `mozilla`, 57,372,672 bytes of x86 code and data, 15,664,881 bytes packed;
  - `arm64.7z`, ARM64 then LZMA2, over `librustc_driver`, `7zz` and `zipper`, 209,958,008 bytes of arm64 code, 35,676,314 bytes packed;
  - `delta.7z`, `Delta:2` then LZMA2, over Silesia `x-ray`, 8,474,240 bytes, 4,251,435 bytes packed;
  - `chain4.7z`, `Delta:2`, BCJ and ARM64 then LZMA2, over the x86 set, 22,229,382 bytes packed.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 7 --cooldown 5` ran each archive at one thread and at four, after one warmup round, in interleaved order. It checks every extracted byte against the source tree.
- System Settings' storage pane kept two services busy, so the script refused the first four starts. No timed run lists a busy process.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | 7-Zip | `after` / `before` |
| --- | --- | --- | --- | --- | ---: |
| BCJ | 1 | 0.446 (0.436 to 0.451) | 0.445 (0.435 to 0.450) | 0.395 | 0.998 |
| BCJ | 4 | 0.442 (0.428 to 0.446) | 0.444 (0.436 to 0.449) | 0.388 | 1.005 |
| ARM64 | 1 | 0.982 (0.973 to 0.984) | 0.973 (0.963 to 0.980) | 0.852 | 0.991 |
| ARM64 | 4 | 0.977 (0.956 to 0.985) | 0.965 (0.960 to 0.975) | 0.691 | 0.988 |
| Delta | 1 | 0.133 (0.133 to 0.145) | 0.141 (0.123 to 0.146) | 0.123 | 1.063 |
| Delta | 4 | 0.143 (0.134 to 0.148) | 0.135 (0.132 to 0.146) | 0.130 | 0.941 |
| Three filters | 1 | 0.619 (0.615 to 0.627) | 0.619 (0.612 to 0.624) | 0.459 | 1.000 |
| Three filters | 4 | 0.619 (0.611 to 0.628) | 0.615 (0.612 to 0.622) | 0.547 | 0.994 |

CPU time matches wall time within 0.01 s in every cell. Peak memory fell by about 1 MiB on most archives, for example from 38 to 37 MiB on BCJ at one thread.

## Decision

Kept. Every range overlaps, so no difference in time is established. The filters cost little next to LZMA2 decoding, which sets the time. The change keeps the speed, removes liblzma from the 7z reader outside tests, and puts each filter under a Kani proof.

## Limits

- One Mac and one session. Linux and Windows were not measured.
- Four threads is the script's default, not Zipper's default thread count.
- `chain4.7z` holds three filters, a chain main already decoded natively. The five-coder chains that main sent to 7-Zip were not timed.
- 7-Zip is 11 to 28% faster on these archives, and the gap is LZMA2 decoding, which this change does not touch.

## Reproduce

Build release binaries at `fd585d0` and `f8013cc`. In `target/bench-work/7z-filters`, copy Silesia `ooffice` and `mozilla` into a folder `x86`, Silesia `x-ray` into `xray`, and three arm64 binaries into `arm64`, each under its own parent folder. Then write the archives:

```sh
7zz a -m0=BCJ -m1=LZMA2 bcj.7z x86-parent/x86
7zz a -m0=ARM64 -m1=LZMA2 arm64.7z arm64-parent/arm64
7zz a -m0=Delta:2 -m1=LZMA2 delta.7z xray-parent/xray
7zz a -m0=Delta:2 -m1=BCJ -m2=ARM64 -m3=LZMA2 chain4.7z x86-parent/x86
```

Run `scripts/compare_zip_readers.py` on each archive and its source folder with `--archive-format 7z --runs 7 --cooldown 5`. Add `--reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1` for one thread.
