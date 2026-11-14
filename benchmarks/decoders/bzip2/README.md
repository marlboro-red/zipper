# Zipper's BZip2 decoder against libbz2-rs-sys and libbzip2

Becomes a crate. At one thread Zipper decodes in 0.61 to 0.72 times the time of `libbz2-rs-sys` and of libbzip2 1.0.8. That holds on both corpora on an Apple M2 Max, on x86-64 Linux and on Windows, with ranges apart in every run. It is the only decoder here that leads its C library everywhere.

## Question

Does Zipper's BZip2 decoder lead `libbz2-rs-sys` in pure Rust and libbzip2 through `bzip2-sys`, on an Apple M2 Max and x86-64 Linux and on two corpora? The metric is the wall time of one decode of a `.bz2` stream at one thread, and of a one-thread `.tar.bz2` extraction.

## Setup

- Two corpora, each run on three machines. Every run built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. It extracts to 211,938,580 bytes of files.
  - `dependencies.tar` is 90,931,200 bytes with sha256 prefix `b8191b36e54aadd5`. It holds the 114 crates that `Cargo.lock` pins, 5,501 files, and extracts to 85,944,317 bytes of files.
  - Each tar's sha256 and every stream's sha256 match on the M2 Max and Linux. Silesia matches on Windows too. The Windows `dependencies.tar` has sha256 prefix `18492cd88f9b2021`, with the same 114 crates by sha256, the same 5,501 files and the same 85,944,317 bytes of files.
- The second machine is an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0.
  - Silesia ran at `49f68e30`, starting at 22:58 UTC on 13 November 2026, and its LZMA decode ran alone at 23:13 UTC.
  - The dependencies ran at `4518a8af`, starting at 01:26 UTC on 14 November 2026.
  - The Silesia WIM extraction ran alone at `5e6dd97b`, at 02:33 UTC on 14 November 2026, with no busy process.
  - Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON, and its checkout is `<checkout>`.
- The Linux runs are hosted GitHub Actions runners with 4 cores and 16 GB on Linux 7.0 for Azure. Each run got a different processor.
  - Silesia ran at `4518a8af` as run 43539679044 of `m31-decoders.yml`, on an AMD EPYC 9V74. The tables use this run.
  - The dependencies ran at `4518a8af` as run 43539675724, on an AMD EPYC 7763.
  - A first Silesia run at `479b42fd`, run 43533788639 on an AMD EPYC 9V45, found no `7zz`. The comparison table lists its decode rows too.
- The Windows machine is a Windows 11 desktop, AMD Ryzen 5 5600X, 6 cores and 12 threads, which the user chose over a hosted runner. Its C libraries were built by the MSVC Build Tools.
  - The decode rows ran at `5e6dd97b`, starting at 02:57 UTC on 14 November 2026.
  - The extraction rows ran the same binaries with the driver at `3b324db6`, which pipes 7-Zip's output correctly on Windows. They started at 03:45 UTC.
  - bsdtar there is the `tar.exe` that Windows ships, libarchive 3.8.9, and 7-Zip is 26.05 x64.
  - Its paths are written as `<scratch>` and `<home>` in the JSON.
- From `49f68e30` to `4518a8af` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke tests, the workflow and Markdown pages changed. Every run measured the same decoders and the same bench code.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against the tar. Each run took 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar and 7-Zip 26.06 at one thread. Every run checked the extracted bytes, sizes and tree members.
- On 14 November 2026 the user decided that the best alternative is the fastest of the pure-Rust crates and the crates that wrap a C library. A decoder becomes a crate only when it leads that one with ranges apart on every operating system measured and both corpora.
- The ratio column is Zipper's median time over the other row's. Its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference. A row is compared only with rows of its own run.
- The streams are written by `libbz2-rs-sys` at level 9. `silesia.tar.bz2` is 54,595,355 bytes and `dependencies.tar.bz2` 11,764,273 bytes.
- Zipper's decoder ran twice, on a one-thread Rayon pool as `zipper` and on the global pool as `zipper-all-threads`. The other decoders run on one thread.

## Results

- Against `libbz2-rs-sys`, the pure-Rust decoder, Zipper at one thread takes 0.609 and 0.649 times its time on Silesia and the dependencies on the M2 Max, and 0.683 and 0.701 times on Linux.
- Against libbzip2 1.0.8, it takes 0.663 and 0.723 times on the M2 Max and 0.670 and 0.634 times on Linux. All eight ranges are apart.
- One-thread `.tar.bz2` extraction leads bsdtar in all four runs. Against 7-Zip it leads on the dependencies on both machines. On Silesia the ranges overlap on the M2 Max, at 0.903, and Zipper trails on Linux at 1.019 (1.009 to 1.034).
- At all threads the extraction takes 451.6 and 341.2 ms on the M2 Max, and 1,591.8 and 665.9 ms on the 4-core runners, at the cost of more CPU time and peak memory.
- On the Windows desktop Zipper at one thread takes 0.644 and 0.706 times `libbz2-rs-sys`'s time and 0.669 and 0.717 times libbzip2's, with ranges apart.
- One-thread `.tar.bz2` extraction on Windows leads bsdtar and 7-Zip on both corpora, with ranges apart. Against 7-Zip it takes 0.913 times on Silesia and 0.620 times on the dependencies.

Every decode comparison, Zipper's median time over the other decoder's with its range, in each run:

| Stream | Against | Run | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- |
| `bzip2` | `libbz2-rs-sys` | Silesia on the Apple M2 Max | 0.609 (0.570 to 0.682) | Zipper ahead |
| `bzip2` | `libbzip2` | Silesia on the Apple M2 Max | 0.663 (0.625 to 0.743) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Silesia on hosted Linux | 0.683 (0.679 to 0.689) | Zipper ahead |
| `bzip2` | `libbzip2` | Silesia on hosted Linux | 0.670 (0.667 to 0.674) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Silesia on hosted Linux, first run | 0.651 (0.617 to 0.711) | Zipper ahead |
| `bzip2` | `libbzip2` | Silesia on hosted Linux, first run | 0.644 (0.605 to 0.687) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Silesia on the Windows desktop | 0.644 (0.625 to 0.649) | Zipper ahead |
| `bzip2` | `libbzip2` | Silesia on the Windows desktop | 0.669 (0.666 to 0.671) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Dependencies on the Apple M2 Max | 0.649 (0.644 to 0.657) | Zipper ahead |
| `bzip2` | `libbzip2` | Dependencies on the Apple M2 Max | 0.723 (0.718 to 0.734) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Dependencies on hosted Linux | 0.701 (0.692 to 0.742) | Zipper ahead |
| `bzip2` | `libbzip2` | Dependencies on hosted Linux | 0.634 (0.626 to 0.653) | Zipper ahead |
| `bzip2` | `libbz2-rs-sys` | Dependencies on the Windows desktop | 0.706 (0.660 to 0.752) | Zipper ahead |
| `bzip2` | `libbzip2` | Dependencies on the Windows desktop | 0.717 (0.698 to 0.762) | Zipper ahead |

### Silesia on the Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 2212.1 (2132.5 to 2456.1) | 2.212 | 320.5 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 361.2 (336.3 to 392.8) | 3.228 | 386.0 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 3629.9 (3599.3 to 3744.3) | 3.630 | 313.7 | 0.609 (0.570 to 0.682) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 3337.9 (3304.4 to 3409.9) | 3.338 | 313.7 | 0.663 (0.625 to 0.743) | Zipper ahead |

Extraction of the `bzip2` row, 54,595,355 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 2358.1 (2342.0 to 2630.1) | 2.385 | 21.3 |  |  |
| `zipper-all-threads` | all | 451.6 (446.3 to 463.8) | 3.679 | 91.5 |  |  |
| `bsdtar` | 1 | 3548.1 (3537.6 to 3576.5) | 3.535 | 6.0 | 0.665 (0.655 to 0.743) | Zipper ahead |
| `7zip` | 1 | 2612.8 (2604.8 to 2619.6) | 2.609 | 8.7 | 0.903 (0.894 to 1.010) | ranges overlap |

### Silesia on hosted Linux

- `m31-decoders-linux-x86_64-silesia.json`, revision `4518a8af`, AMD EPYC 9V74 80-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.01 at the start, busy processes at the start 0

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 3237.6 (3232.2 to 3247.5) | 3.237 | 265.8 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 1473.2 (1461.3 to 1493.0) | 4.465 | 279.3 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 4741.8 (4714.9 to 4759.3) | 4.741 | 260.7 | 0.683 (0.679 to 0.689) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 4829.1 (4817.1 to 4849.2) | 4.829 | 260.7 | 0.670 (0.667 to 0.674) | Zipper ahead |

Extraction of the `bzip2` row, 54,595,355 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 3312.6 (3294.9 to 3332.0) | 3.350 | 26.4 |  |  |
| `zipper-all-threads` | all | 1591.8 (1579.6 to 1610.6) | 4.750 | 51.0 |  |  |
| `bsdtar` | 1 | 5044.7 (5019.3 to 5097.3) | 5.030 | 7.5 | 0.657 (0.646 to 0.664) | Zipper ahead |
| `7zip` | 1 | 3251.4 (3222.6 to 3264.8) | 3.240 | 10.6 | 1.019 (1.009 to 1.034) | Zipper behind |

### Silesia on the Windows desktop

- `m34-decode-windows-silesia.json`, revision `5e6dd97b`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0
- `m34-extract-windows-silesia.json`, revision `3b324db6`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, extraction only, no load average on this system, busy processes at the start 0

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 2731.6 (2722.1 to 2737.9) | not measured | 270.7 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 523.3 (517.7 to 550.2) | not measured | 320.1 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 4239.4 (4221.0 to 4355.3) | not measured | 261.7 | 0.644 (0.625 to 0.649) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 4082.9 (4077.7 to 4088.5) | not measured | 261.6 | 0.669 (0.666 to 0.671) | Zipper ahead |

Extraction of the `bzip2` row, 54,595,355 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 2782.4 (2777.7 to 2820.7) | 2.828 | 21.3 |  |  |
| `zipper-all-threads` | all | 721.0 (702.6 to 727.0) | 5.953 | 70.3 |  |  |
| `bsdtar` | 1 | 4229.7 (4217.7 to 4286.8) | 4.219 | 9.1 | 0.658 (0.648 to 0.669) | Zipper ahead |
| `7zip` | 1 | 3046.0 (3043.1 to 3049.7) | 0.016 | 7.7 | 0.913 (0.911 to 0.927) | Zipper ahead |

### Dependencies on the Apple M2 Max

- `m31-decoders-m2-max-dependencies.json`, revision `4518a8af`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 3.61 at the start, busy processes at the start 2

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 667.7 (665.3 to 673.0) | 0.668 | 122.9 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 105.4 (104.1 to 107.4) | 0.952 | 179.4 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 1028.8 (1024.6 to 1032.3) | 1.029 | 117.1 | 0.649 (0.644 to 0.657) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 923.6 (917.4 to 927.1) | 0.924 | 117.1 | 0.723 (0.718 to 0.734) | Zipper ahead |

Extraction of the `bzip2` row, 11,764,273 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 992.6 (988.7 to 1005.3) | 1.175 | 20.0 |  |  |
| `zipper-all-threads` | all | 341.2 (338.8 to 346.7) | 2.321 | 80.5 |  |  |
| `bsdtar` | 1 | 1485.9 (1478.6 to 1495.0) | 1.476 | 6.6 | 0.668 (0.661 to 0.680) | Zipper ahead |
| `7zip` | 1 | 1197.4 (1192.3 to 1206.9) | 1.241 | 8.7 | 0.829 (0.819 to 0.843) | Zipper ahead |

### Dependencies on hosted Linux

- `m31-decoders-linux-x86_64-dependencies.json`, revision `4518a8af`, AMD EPYC 7763 64-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.69 at the start, busy processes at the start 0

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 1174.1 (1166.4 to 1204.8) | 1.174 | 118.6 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 520.7 (514.2 to 574.6) | 1.585 | 119.7 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 1674.3 (1624.6 to 1685.1) | 1.674 | 104.7 | 0.701 (0.692 to 0.742) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 1850.5 (1845.5 to 1862.8) | 1.850 | 104.6 | 0.634 (0.626 to 0.653) | Zipper ahead |

Extraction of the `bzip2` row, 11,764,273 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 1349.0 (1339.3 to 1358.9) | 1.480 | 27.1 |  |  |
| `zipper-all-threads` | all | 665.9 (650.0 to 689.3) | 2.130 | 45.6 |  |  |
| `bsdtar` | 1 | 2286.2 (2275.3 to 2318.0) | 2.280 | 7.9 | 0.590 (0.578 to 0.597) | Zipper ahead |
| `7zip` | 1 | 1487.3 (1444.6 to 1498.1) | 1.530 | 10.6 | 0.907 (0.894 to 0.941) | Zipper ahead |

### Dependencies on the Windows desktop

- `m34-decode-windows-dependencies.json`, revision `5e6dd97b`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0
- `m34-extract-windows-dependencies.json`, revision `3b324db6`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, extraction only, no load average on this system, busy processes at the start 0

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 836.3 (821.5 to 886.7) | not measured | 114.0 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 174.5 (168.5 to 192.2) | not measured | 155.0 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 1184.7 (1178.7 to 1245.3) | not measured | 105.4 | 0.706 (0.660 to 0.752) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 1166.7 (1163.8 to 1177.3) | not measured | 105.4 | 0.717 (0.698 to 0.762) | Zipper ahead |

Extraction of the `bzip2` row, 11,766,429 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 4643.1 (4593.9 to 4777.6) | 4.594 | 21.7 |  |  |
| `zipper-all-threads` | all | 4349.9 (4289.6 to 4436.7) | 5.844 | 57.8 |  |  |
| `bsdtar` | 1 | 5713.4 (5665.6 to 5750.8) | 5.328 | 9.5 | 0.813 (0.799 to 0.843) | Zipper ahead |
| `7zip` | 1 | 7486.0 (7424.4 to 7608.6) | 0.000 | 7.7 | 0.620 (0.604 to 0.644) | Zipper ahead |

## Decision

The verdict is that it becomes a crate.

- At one thread Zipper leads both the pure-Rust decoder and libbzip2, the C library, on both machines and both corpora, with ranges apart in every run. It is the only decoder here that meets the rule against a C-wrapping crate.
- The parallel block decoder adds a lead that no alternative here offers, at the cost of more CPU time and memory.
- Windows confirms the verdict. Zipper leads both decoders on both corpora there, so the lead holds on all three operating systems.

## Limits

- The M2 Max was loaded during both of its runs, and both passed `--allow-busy`.
  - A `rustc` process used 98 to 100% of one core at the start of the Silesia decode and of each of its four extraction rows. The Silesia LZMA decode ran in its own run with no busy process.
  - `corespotlightd` used 153% and `fseventsd` 43% at the start of the dependencies decode, and `fseventsd` 47 to 48% at the start of three of its extraction rows.
- Two corpora, Silesia and the dependencies. Neither holds many small compressed frames or streams.
- The M2 Max WIM row ran on Silesia only, in its own run at `5e6dd97b` with wimlib 1.14.5. The dependencies have no WIM row on the M2 Max.
- No run has a `.tar.lzma` extraction row. Zipper writes the `.tar.lzma` out as one file, the `.tar`, where bsdtar reads the tar inside, so the trees differ.
- The three Linux runs each got a different AMD EPYC, so Linux numbers compare only within one run. Linux CPU times for child processes come in steps of 10 ms.
- Windows has no WIM extraction row, since `wimlib-imagex` is not installed there.
- The Windows `dependencies.tar` differs in sha256 from the other machines, though it holds the same crates, files and file bytes. Its streams differ by a few kilobytes.
- On Windows the decode bench reports no CPU time for the decode call, and the driver has no load average. Its 7-Zip rows report the CPU time and peak RSS of the process that pipes 7-Zip's output, not of 7-Zip.
- On Windows the dependencies take 4.2 to 7.5 s to extract with every reader, so file creation on NTFS dominates those rows, not the decoder.
- The C libraries were built by MSVC on Windows and by the system C compiler on the M2 Max and Linux, so a gap to a C library can belong to its compiler.
- Each machine ran each corpus once, so session to session drift is not measured.
- No profile was taken. The limiter named is decode CPU, read from CPU time matching wall time at one thread.

## Reproduce

The [M31 page](../../../docs/roadmap/m31-a-release-case-for-each-decoder.md#measure) gives the commands for both corpora on both machines. The JSON is in [`data/`](../data) beside these records, with the second machine's build folder written as `<target>`, its checkout as `<checkout>` and its home folder as `<home>`. Every number on this page comes from

```sh
python3 benchmarks/decoders/tables.py bzip2
```
