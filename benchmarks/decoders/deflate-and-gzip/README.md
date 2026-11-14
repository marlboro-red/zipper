# Zipper's Deflate and gzip decoder against zlib-rs, miniz_oxide and libdeflate

Stays inside Zipper. Zipper decodes in 0.68 to 0.72 times `zlib-rs`'s time on an Apple M2 Max, but on x86-64 Linux it ties, leads or trails `zlib-rs` by a few percent, depending on the stream and corpus. libdeflate leads it on Linux on both corpora.

## Question

Does Zipper's Deflate decoder lead `zlib-rs` and `miniz_oxide` in pure Rust and libdeflate through `libdeflater`, on an Apple M2 Max and x86-64 Linux and on two corpora? The metric is the wall time of one decode of a raw Deflate and a gzip stream, and of a one-thread `.tar.gz` extraction.

## Setup

- Two corpora, each run on two machines, five runs in all. Every run built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. It extracts to 211,938,580 bytes of files.
  - `dependencies.tar` is 90,931,200 bytes with sha256 prefix `b8191b36e54aadd5`. It holds the 114 crates that `Cargo.lock` pins, 5,501 files, and extracts to 85,944,317 bytes of files.
  - Each tar's sha256 and every stream's sha256 match on both machines.
- The second machine is an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0.
  - Silesia ran at `49f68e30`, starting at 22:58 UTC on 13 November 2026, and its LZMA decode ran alone at 23:13 UTC.
  - The dependencies ran at `4518a8af`, starting at 01:26 UTC on 14 November 2026.
  - Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
- The Linux runs are hosted GitHub Actions runners with 4 cores and 16 GB on Linux 7.0 for Azure. Each run got a different processor.
  - Silesia ran at `4518a8af` as run 43539679044 of `m31-decoders.yml`, on an AMD EPYC 9V74. The tables use this run.
  - The dependencies ran at `4518a8af` as run 43539675724, on an AMD EPYC 7763.
  - A first Silesia run at `479b42fd`, run 43533788639 on an AMD EPYC 9V45, found no `7zz`. The comparison table lists its decode rows too.
- From `49f68e30` to `4518a8af` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke tests, the workflow and Markdown pages changed. Every run measured the same decoders and the same bench code.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against the tar. Each run took 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar and 7-Zip 26.06 at one thread. Every run checked the extracted bytes, sizes and tree members.
- The ratio column is Zipper's median time over the other row's. Its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference. A row is compared only with rows of its own run.
- The streams are raw Deflate and gzip, written by `flate2` with `zlib-rs` at level 6. The Silesia streams are 68,915,303 and 68,915,321 bytes, and the dependencies streams 16,319,060 and 16,319,078 bytes.
- The gzip extraction row covers Deflate, since a raw Deflate stream has no container to extract.

## Results

- Against `zlib-rs`, the best pure-Rust decoder, Zipper takes 0.678 to 0.721 times its time on the M2 Max, on both streams and both corpora, with ranges apart.
- On Linux the four ratios are 1.002 and 0.962 on Silesia raw Deflate and gzip, and 1.031 and 0.982 on the dependencies. Two ranges overlap, one has Zipper ahead and one behind, so the two decoders tie there.
- Against `miniz_oxide`, Zipper takes 0.361 to 0.446 times its time on the M2 Max and 0.629 to 0.666 times on Linux, all with ranges apart.
- Against libdeflate through `libdeflater` 1.26.1, the ranges overlap on Silesia on the M2 Max, and Zipper trails at 1.081 and 1.085 on the dependencies there. On Linux it trails at 1.229 to 1.328 times.
- One-thread `.tar.gz` extraction leads bsdtar and 7-Zip in all four runs, with ranges apart.

Every decode comparison, Zipper's median time over the other decoder's with its range, in each run:

| Stream | Against | Run | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- |
| `deflate` | `zlib-rs` | Silesia on the Apple M2 Max | 0.720 (0.677 to 0.747) | Zipper ahead |
| `deflate` | `miniz_oxide` | Silesia on the Apple M2 Max | 0.438 (0.405 to 0.462) | Zipper ahead |
| `deflate` | `libdeflate` | Silesia on the Apple M2 Max | 1.053 (0.966 to 1.109) | ranges overlap |
| `deflate` | `zlib-rs` | Silesia on hosted Linux | 1.002 (0.994 to 1.021) | ranges overlap |
| `deflate` | `miniz_oxide` | Silesia on hosted Linux | 0.666 (0.663 to 0.679) | Zipper ahead |
| `deflate` | `libdeflate` | Silesia on hosted Linux | 1.246 (1.238 to 1.272) | Zipper behind |
| `deflate` | `zlib-rs` | Silesia on hosted Linux, first run | 1.015 (0.952 to 1.056) | ranges overlap |
| `deflate` | `miniz_oxide` | Silesia on hosted Linux, first run | 0.646 (0.610 to 0.674) | Zipper ahead |
| `deflate` | `libdeflate` | Silesia on hosted Linux, first run | 1.219 (1.119 to 1.260) | Zipper behind |
| `deflate` | `zlib-rs` | Dependencies on the Apple M2 Max | 0.690 (0.676 to 0.706) | Zipper ahead |
| `deflate` | `miniz_oxide` | Dependencies on the Apple M2 Max | 0.361 (0.351 to 0.369) | Zipper ahead |
| `deflate` | `libdeflate` | Dependencies on the Apple M2 Max | 1.081 (1.055 to 1.106) | Zipper behind |
| `deflate` | `zlib-rs` | Dependencies on hosted Linux | 1.031 (1.026 to 1.044) | Zipper behind |
| `deflate` | `miniz_oxide` | Dependencies on hosted Linux | 0.629 (0.615 to 0.637) | Zipper ahead |
| `deflate` | `libdeflate` | Dependencies on hosted Linux | 1.328 (1.312 to 1.345) | Zipper behind |
| `gzip` | `zlib-rs` | Silesia on the Apple M2 Max | 0.721 (0.667 to 0.738) | Zipper ahead |
| `gzip` | `miniz_oxide` | Silesia on the Apple M2 Max | 0.446 (0.413 to 0.461) | Zipper ahead |
| `gzip` | `libdeflate` | Silesia on the Apple M2 Max | 1.078 (0.817 to 1.123) | ranges overlap |
| `gzip` | `zlib-rs` | Silesia on hosted Linux | 0.962 (0.902 to 0.977) | Zipper ahead |
| `gzip` | `miniz_oxide` | Silesia on hosted Linux | 0.666 (0.655 to 0.677) | Zipper ahead |
| `gzip` | `libdeflate` | Silesia on hosted Linux | 1.229 (1.216 to 1.252) | Zipper behind |
| `gzip` | `zlib-rs` | Silesia on hosted Linux, first run | 0.950 (0.898 to 1.019) | ranges overlap |
| `gzip` | `miniz_oxide` | Silesia on hosted Linux, first run | 0.643 (0.614 to 0.678) | Zipper ahead |
| `gzip` | `libdeflate` | Silesia on hosted Linux, first run | 1.163 (1.113 to 1.241) | Zipper behind |
| `gzip` | `zlib-rs` | Dependencies on the Apple M2 Max | 0.678 (0.662 to 0.702) | Zipper ahead |
| `gzip` | `miniz_oxide` | Dependencies on the Apple M2 Max | 0.366 (0.356 to 0.377) | Zipper ahead |
| `gzip` | `libdeflate` | Dependencies on the Apple M2 Max | 1.085 (1.055 to 1.123) | Zipper behind |
| `gzip` | `zlib-rs` | Dependencies on hosted Linux | 0.982 (0.958 to 1.003) | ranges overlap |
| `gzip` | `miniz_oxide` | Dependencies on hosted Linux | 0.633 (0.619 to 0.646) | Zipper ahead |
| `gzip` | `libdeflate` | Dependencies on hosted Linux | 1.282 (1.162 to 1.311) | Zipper behind |

### Silesia on the Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

Decode of `deflate`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 187.0 (181.0 to 193.4) | 0.187 | 337.6 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 259.7 (258.8 to 267.3) | 0.260 | 337.5 | 0.720 (0.677 to 0.747) | Zipper ahead |
| `miniz_oxide` | `miniz_oxide` 0.9.1 | 426.8 (418.8 to 446.4) | 0.427 | 337.5 | 0.438 (0.405 to 0.462) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 177.5 (174.4 to 187.4) | 0.178 | 337.5 | 1.053 (0.966 to 1.109) | ranges overlap |

Decode of `gzip`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 194.9 (186.3 to 198.3) | 0.195 | 337.6 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 270.1 (268.6 to 279.3) | 0.270 | 337.5 | 0.721 (0.667 to 0.738) | Zipper ahead |
| `miniz_oxide` | `miniz_oxide` 0.9.1, `crc32fast` 1.5.3 | 436.6 (430.3 to 450.8) | 0.437 | 337.5 | 0.446 (0.413 to 0.461) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 180.8 (176.5 to 228.1) | 0.181 | 337.5 | 1.078 (0.817 to 1.123) | ranges overlap |

Extraction of the `gzip` row, 68,915,321 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 206.4 (206.2 to 209.9) | 0.228 | 13.7 |  |  |
| `zipper-all-threads` | all | 208.6 (206.3 to 210.4) | 0.230 | 15.0 |  |  |
| `bsdtar` | 1 | 303.6 (298.9 to 309.7) | 0.290 | 2.5 | 0.680 (0.666 to 0.702) | Zipper ahead |
| `7zip` | 1 | 695.3 (691.7 to 697.4) | 0.733 | 4.2 | 0.297 (0.296 to 0.304) | Zipper ahead |

### Silesia on hosted Linux

- `m31-decoders-linux-x86_64-silesia.json`, revision `4518a8af`, AMD EPYC 9V74 80-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.01 at the start, busy processes at the start 0

Decode of `deflate`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 254.7 (253.9 to 259.1) | 0.255 | 272.0 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 254.3 (253.8 to 255.5) | 0.254 | 272.0 | 1.002 (0.994 to 1.021) | ranges overlap |
| `miniz_oxide` | `miniz_oxide` 0.9.1 | 382.6 (381.5 to 383.0) | 0.383 | 272.0 | 0.666 (0.663 to 0.679) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 204.4 (203.7 to 205.1) | 0.204 | 272.0 | 1.246 (1.238 to 1.272) | Zipper behind |

Decode of `gzip`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 258.5 (257.9 to 262.2) | 0.259 | 272.0 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 268.7 (268.3 to 285.9) | 0.269 | 272.0 | 0.962 (0.902 to 0.977) | Zipper ahead |
| `miniz_oxide` | `miniz_oxide` 0.9.1, `crc32fast` 1.5.3 | 387.8 (387.2 to 393.6) | 0.388 | 272.0 | 0.666 (0.655 to 0.677) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 210.3 (209.4 to 212.0) | 0.210 | 272.0 | 1.229 (1.216 to 1.252) | Zipper behind |

Extraction of the `gzip` row, 68,915,321 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 267.3 (262.9 to 271.3) | 0.290 | 11.1 |  |  |
| `zipper-all-threads` | all | 273.5 (270.2 to 274.6) | 0.310 | 11.2 |  |  |
| `bsdtar` | 1 | 557.2 (556.8 to 558.0) | 0.550 | 4.4 | 0.480 (0.471 to 0.487) | Zipper ahead |
| `7zip` | 1 | 795.9 (778.3 to 813.5) | 0.870 | 6.2 | 0.336 (0.323 to 0.349) | Zipper ahead |

### Dependencies on the Apple M2 Max

- `m31-decoders-m2-max-dependencies.json`, revision `4518a8af`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 3.61 at the start, busy processes at the start 2

Decode of `deflate`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 48.2 (47.6 to 48.8) | 0.048 | 122.4 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 69.9 (69.2 to 70.4) | 0.070 | 122.3 | 0.690 (0.676 to 0.706) | Zipper ahead |
| `miniz_oxide` | `miniz_oxide` 0.9.1 | 133.4 (132.5 to 135.8) | 0.133 | 122.3 | 0.361 (0.351 to 0.369) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 44.6 (44.2 to 45.1) | 0.045 | 122.4 | 1.081 (1.055 to 1.106) | Zipper behind |

Decode of `gzip`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 50.1 (49.2 to 51.4) | 0.050 | 122.4 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 73.9 (73.2 to 74.4) | 0.074 | 122.3 | 0.678 (0.662 to 0.702) | Zipper ahead |
| `miniz_oxide` | `miniz_oxide` 0.9.1, `crc32fast` 1.5.3 | 136.8 (136.3 to 138.3) | 0.137 | 122.3 | 0.366 (0.356 to 0.377) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 46.2 (45.8 to 46.6) | 0.046 | 122.4 | 1.085 (1.055 to 1.123) | Zipper behind |

Extraction of the `gzip` row, 16,319,078 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 433.2 (421.8 to 449.5) | 0.562 | 13.8 |  |  |
| `zipper-all-threads` | all | 269.8 (268.4 to 274.6) | 1.187 | 12.7 |  |  |
| `bsdtar` | 1 | 657.3 (642.7 to 686.4) | 0.653 | 3.0 | 0.659 (0.614 to 0.699) | Zipper ahead |
| `7zip` | 1 | 590.5 (584.9 to 596.0) | 0.741 | 4.3 | 0.734 (0.708 to 0.768) | Zipper ahead |

### Dependencies on hosted Linux

- `m31-decoders-linux-x86_64-dependencies.json`, revision `4518a8af`, AMD EPYC 7763 64-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.69 at the start, busy processes at the start 0

Decode of `deflate`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 82.0 (81.9 to 82.9) | 0.082 | 107.4 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 79.6 (79.4 to 79.8) | 0.080 | 107.2 | 1.031 (1.026 to 1.044) | Zipper behind |
| `miniz_oxide` | `miniz_oxide` 0.9.1 | 130.3 (130.0 to 133.2) | 0.130 | 107.3 | 0.629 (0.615 to 0.637) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 61.8 (61.6 to 62.4) | 0.062 | 107.4 | 1.328 (1.312 to 1.345) | Zipper behind |

Decode of `gzip`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 85.6 (85.0 to 87.2) | 0.086 | 107.4 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 87.1 (86.9 to 88.8) | 0.087 | 107.1 | 0.982 (0.958 to 1.003) | ranges overlap |
| `miniz_oxide` | `miniz_oxide` 0.9.1, `crc32fast` 1.5.3 | 135.2 (135.0 to 137.4) | 0.135 | 107.1 | 0.633 (0.619 to 0.646) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 66.7 (66.5 to 73.2) | 0.067 | 107.3 | 1.282 (1.162 to 1.311) | Zipper behind |

Extraction of the `gzip` row, 16,319,078 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 207.1 (203.4 to 228.6) | 0.330 | 11.2 |  |  |
| `zipper-all-threads` | all | 204.8 (199.6 to 231.9) | 0.500 | 11.3 |  |  |
| `bsdtar` | 1 | 530.2 (527.0 to 538.0) | 0.520 | 4.7 | 0.391 (0.378 to 0.434) | Zipper ahead |
| `7zip` | 1 | 447.3 (435.5 to 454.6) | 0.630 | 6.3 | 0.463 (0.447 to 0.525) | Zipper ahead |

## Decision

The verdict is that it stays inside Zipper.

- The lead over `zlib-rs` holds on the M2 Max only. On x86-64 Linux the two tie, so no lead over the best pure-Rust decoder rests on two machines.
- Zipper trails libdeflate on Linux, and on the M2 Max it ties on Silesia and trails on the dependencies.
- The decoder keeps its place in Zipper, where `.tar.gz` extraction leads bsdtar and 7-Zip on both machines and both corpora.

## Limits

- The M2 Max was loaded during both of its runs, and both passed `--allow-busy`.
  - A `rustc` process used 98 to 100% of one core at the start of the Silesia decode and of each of its four extraction rows. The Silesia LZMA decode ran in its own run with no busy process.
  - `corespotlightd` used 153% and `fseventsd` 43% at the start of the dependencies decode, and `fseventsd` 47 to 48% at the start of three of its extraction rows.
- Two corpora, Silesia and the dependencies. Neither holds many small compressed frames or streams.
- The M2 Max has no WIM extraction row, because `wimlib-imagex` is not installed there to write the WIM.
- No run has a `.tar.lzma` extraction row. Zipper writes the `.tar.lzma` out as one file, the `.tar`, where bsdtar reads the tar inside, so the trees differ.
- The three Linux runs each got a different AMD EPYC, so Linux numbers compare only within one run. Linux CPU times for child processes come in steps of 10 ms.
- Each machine ran each corpus once, so session to session drift is not measured.
- No profile was taken. The limiter named is decode CPU, read from CPU time matching wall time at one thread.

## Reproduce

The [M31 page](../../../docs/roadmap/m31-a-release-case-for-each-decoder.md#measure) gives the commands for both corpora on both machines. The JSON is in [`data/`](../data) beside these records, with the second machine's build folder written as `<target>` and its home folder as `<home>`. Every number on this page comes from

```sh
python3 benchmarks/decoders/tables.py deflate-and-gzip
```
