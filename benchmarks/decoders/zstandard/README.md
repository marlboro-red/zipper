# Zipper's Zstandard decoder against ruzstd and libzstd

Stays inside Zipper. Zipper trails libzstd on x86-64 Linux by 1.34 times on both corpora, with ranges apart, though it leads libzstd on an Apple M2 Max. It decodes in 0.29 to 0.45 times `ruzstd`'s time in every run.

## Question

Does Zipper's Zstandard decoder lead `ruzstd` in pure Rust and libzstd through the `zstd` crate, on an Apple M2 Max and x86-64 Linux and on two corpora? The metric is the wall time of one decode of a `.zst` stream, and of a one-thread `.tar.zst` extraction.

## Setup

- Two corpora, each run on two machines, five runs in all. Every run built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. It extracts to 211,938,580 bytes of files.
  - `dependencies.tar` is 90,931,200 bytes with sha256 prefix `b8191b36e54aadd5`. It holds the 114 crates that `Cargo.lock` pins, 5,501 files, and extracts to 85,944,317 bytes of files.
  - Each tar's sha256 and every stream's sha256 match on both machines.
- The second machine is an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0.
  - Silesia ran at `49f68e30`, starting at 22:58 UTC on 13 November 2026, and its LZMA decode ran alone at 23:13 UTC.
  - The dependencies ran at `4518a8af`, starting at 01:26 UTC on 14 November 2026.
  - The Silesia WIM extraction ran alone at `5e6dd97b`, at 02:33 UTC on 14 November 2026, with no busy process.
  - Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON, and its checkout is `<checkout>`.
- The Linux runs are hosted GitHub Actions runners with 4 cores and 16 GB on Linux 7.0 for Azure. Each run got a different processor.
  - Silesia ran at `4518a8af` as run 43539679044 of `m31-decoders.yml`, on an AMD EPYC 9V74. The tables use this run.
  - The dependencies ran at `4518a8af` as run 43539675724, on an AMD EPYC 7763.
  - A first Silesia run at `479b42fd`, run 43533788639 on an AMD EPYC 9V45, found no `7zz`. The comparison table lists its decode rows too.
- From `49f68e30` to `4518a8af` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke tests, the workflow and Markdown pages changed. Every run measured the same decoders and the same bench code.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against the tar. Each run took 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar and 7-Zip 26.06 at one thread. Every run checked the extracted bytes, sizes and tree members.
- On 14 November 2026 the user decided that the best alternative is the fastest of the pure-Rust crates and the crates that wrap a C library. A decoder becomes a crate only when it leads that one with ranges apart on both machines and both corpora.
- The ratio column is Zipper's median time over the other row's. Its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference. A row is compared only with rows of its own run.
- The streams are one frame each with its checksum, written by libzstd at level 3. `silesia.tar.zst` is 66,497,280 bytes and `dependencies.tar.zst` 12,598,289 bytes.

## Results

- Against `ruzstd`, the best pure-Rust decoder, Zipper takes 0.292 and 0.357 times its time on Silesia and the dependencies on the M2 Max, and 0.383 and 0.447 times on Linux. Every range is apart.
- Against libzstd 1.5.7, Zipper takes 0.913 and 0.926 times its time on the M2 Max, with ranges apart. On Linux it trails, at 1.342 and 1.343 times, with ranges apart.
- One-thread `.tar.zst` extraction leads bsdtar and 7-Zip in all four runs, with ranges apart. Against 7-Zip it takes 0.739 and 0.771 times its time on the M2 Max and 0.810 and 0.522 times on Linux.

Every decode comparison, Zipper's median time over the other decoder's with its range, in each run:

| Stream | Against | Run | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- |
| `zstd` | `ruzstd` | Silesia on the Apple M2 Max | 0.292 (0.251 to 0.305) | Zipper ahead |
| `zstd` | `libzstd` | Silesia on the Apple M2 Max | 0.913 (0.897 to 0.950) | Zipper ahead |
| `zstd` | `ruzstd` | Silesia on hosted Linux | 0.383 (0.380 to 0.388) | Zipper ahead |
| `zstd` | `libzstd` | Silesia on hosted Linux | 1.342 (1.289 to 1.361) | Zipper behind |
| `zstd` | `ruzstd` | Silesia on hosted Linux, first run | 0.332 (0.313 to 0.347) | Zipper ahead |
| `zstd` | `libzstd` | Silesia on hosted Linux, first run | 1.214 (0.831 to 1.265) | ranges overlap |
| `zstd` | `ruzstd` | Dependencies on the Apple M2 Max | 0.357 (0.353 to 0.362) | Zipper ahead |
| `zstd` | `libzstd` | Dependencies on the Apple M2 Max | 0.926 (0.914 to 0.937) | Zipper ahead |
| `zstd` | `ruzstd` | Dependencies on hosted Linux | 0.447 (0.440 to 0.460) | Zipper ahead |
| `zstd` | `libzstd` | Dependencies on hosted Linux | 1.343 (1.308 to 1.393) | Zipper behind |

### Silesia on the Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

Decode of `zstd`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 145.3 (144.1 to 150.4) | 0.145 | 335.4 |  |  |
| `ruzstd` | `ruzstd` 0.9.1 | 496.7 (493.9 to 573.0) | 0.497 | 342.8 | 0.292 (0.251 to 0.305) | Zipper ahead |
| `libzstd` | `zstd` 0.14.3, `zstd-safe` 8.2.0, `zstd-sys` 2.1.2+zstd.1.5.7 | 159.1 (158.3 to 160.7) | 0.159 | 333.0 | 0.913 (0.897 to 0.950) | Zipper ahead |

Extraction of the `zstd` row, 66,497,280 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 161.2 (159.8 to 161.7) | 0.181 | 15.5 |  |  |
| `zipper-all-threads` | all | 161.0 (160.0 to 162.1) | 0.181 | 14.8 |  |  |
| `bsdtar` | 1 | 197.9 (193.6 to 200.3) | 0.289 | 7.2 | 0.815 (0.798 to 0.835) | Zipper ahead |
| `7zip` | 1 | 218.0 (213.2 to 220.9) | 0.226 | 6.1 | 0.739 (0.723 to 0.758) | Zipper ahead |

### Silesia on hosted Linux

- `m31-decoders-linux-x86_64-silesia.json`, revision `4518a8af`, AMD EPYC 9V74 80-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.01 at the start, busy processes at the start 0

Decode of `zstd`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 219.7 (219.2 to 221.9) | 0.220 | 270.9 |  |  |
| `ruzstd` | `ruzstd` 0.9.1 | 572.9 (571.5 to 577.4) | 0.573 | 273.7 | 0.383 (0.380 to 0.388) | Zipper ahead |
| `libzstd` | `zstd` 0.14.3, `zstd-safe` 8.2.0, `zstd-sys` 2.1.2+zstd.1.5.7 | 163.7 (163.0 to 170.1) | 0.164 | 270.1 | 1.342 (1.289 to 1.361) | Zipper behind |

Extraction of the `zstd` row, 66,497,280 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 223.9 (223.2 to 226.6) | 0.250 | 14.2 |  |  |
| `zipper-all-threads` | all | 231.7 (229.2 to 233.0) | 0.260 | 14.1 |  |  |
| `bsdtar` | 1 | 245.0 (243.5 to 248.7) | 0.230 | 6.9 | 0.914 (0.898 to 0.931) | Zipper ahead |
| `7zip` | 1 | 276.2 (264.5 to 287.9) | 0.310 | 7.9 | 0.810 (0.775 to 0.857) | Zipper ahead |

### Dependencies on the Apple M2 Max

- `m31-decoders-m2-max-dependencies.json`, revision `4518a8af`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 3.61 at the start, busy processes at the start 2

Decode of `zstd`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 41.4 (40.9 to 41.8) | 0.041 | 117.7 |  |  |
| `ruzstd` | `ruzstd` 0.9.1 | 115.9 (115.6 to 116.1) | 0.116 | 123.0 | 0.357 (0.353 to 0.362) | Zipper ahead |
| `libzstd` | `zstd` 0.14.3, `zstd-safe` 8.2.0, `zstd-sys` 2.1.2+zstd.1.5.7 | 44.7 (44.6 to 44.8) | 0.045 | 115.3 | 0.926 (0.914 to 0.937) | Zipper ahead |

Extraction of the `zstd` row, 12,598,289 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 423.2 (420.9 to 437.6) | 0.542 | 14.6 |  |  |
| `zipper-all-threads` | all | 272.7 (267.3 to 275.7) | 1.185 | 15.2 |  |  |
| `bsdtar` | 1 | 594.6 (590.7 to 609.4) | 0.650 | 7.1 | 0.712 (0.691 to 0.741) | Zipper ahead |
| `7zip` | 1 | 549.0 (541.1 to 555.0) | 0.597 | 6.0 | 0.771 (0.758 to 0.809) | Zipper ahead |

### Dependencies on hosted Linux

- `m31-decoders-linux-x86_64-dependencies.json`, revision `4518a8af`, AMD EPYC 7763 64-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.69 at the start, busy processes at the start 0

Decode of `zstd`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 79.5 (79.2 to 81.4) | 0.080 | 104.4 |  |  |
| `ruzstd` | `ruzstd` 0.9.1 | 177.8 (176.7 to 180.0) | 0.178 | 112.2 | 0.447 (0.440 to 0.460) | Zipper ahead |
| `libzstd` | `zstd` 0.14.3, `zstd-safe` 8.2.0, `zstd-sys` 2.1.2+zstd.1.5.7 | 59.2 (58.4 to 60.5) | 0.059 | 104.2 | 1.343 (1.308 to 1.393) | Zipper behind |

Extraction of the `zstd` row, 12,598,289 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 212.6 (204.5 to 223.2) | 0.330 | 14.8 |  |  |
| `zipper-all-threads` | all | 193.3 (189.3 to 212.8) | 0.480 | 14.7 |  |  |
| `bsdtar` | 1 | 432.0 (420.4 to 443.3) | 0.420 | 7.1 | 0.492 (0.461 to 0.531) | Zipper ahead |
| `7zip` | 1 | 407.5 (374.3 to 425.9) | 0.480 | 7.8 | 0.522 (0.480 to 0.596) | Zipper ahead |

## Decision

The verdict is that it stays inside Zipper.

- libzstd through the `zstd` crate is the best alternative, and Zipper trails it on x86-64 Linux on both corpora with ranges apart.
- The lead over `ruzstd`, the best pure-Rust decoder, holds in every run, but the rule counts the C-wrapping crate too.
- No profile on x86-64 Linux shows where the gap of 1.34 times comes from, so this record does not call it closable.

## Limits

- The M2 Max was loaded during both of its runs, and both passed `--allow-busy`.
  - A `rustc` process used 98 to 100% of one core at the start of the Silesia decode and of each of its four extraction rows. The Silesia LZMA decode ran in its own run with no busy process.
  - `corespotlightd` used 153% and `fseventsd` 43% at the start of the dependencies decode, and `fseventsd` 47 to 48% at the start of three of its extraction rows.
- Two corpora, Silesia and the dependencies. Neither holds many small compressed frames or streams.
- The M2 Max WIM row ran on Silesia only, in its own run at `5e6dd97b` with wimlib 1.14.5. The dependencies have no WIM row on the M2 Max.
- No run has a `.tar.lzma` extraction row. Zipper writes the `.tar.lzma` out as one file, the `.tar`, where bsdtar reads the tar inside, so the trees differ.
- The three Linux runs each got a different AMD EPYC, so Linux numbers compare only within one run. Linux CPU times for child processes come in steps of 10 ms.
- Each machine ran each corpus once, so session to session drift is not measured.
- No profile was taken. The limiter named is decode CPU, read from CPU time matching wall time at one thread.

## Reproduce

The [M31 page](../../../docs/roadmap/m31-a-release-case-for-each-decoder.md#measure) gives the commands for both corpora on both machines. The JSON is in [`data/`](../data) beside these records, with the second machine's build folder written as `<target>`, its checkout as `<checkout>` and its home folder as `<home>`. Every number on this page comes from

```sh
python3 benchmarks/decoders/tables.py zstandard
```
