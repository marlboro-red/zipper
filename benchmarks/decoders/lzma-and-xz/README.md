# Zipper's LZMA and xz decoder against lzma-rs, lzma-rust2 and liblzma

Not ready yet, so M23 does not start until [M35](../../../docs/roadmap/m35-zstandard-and-lzma-at-c-speed-on-x86-64.md) closes the Linux gap. Zipper trails liblzma on x86-64 Linux by 1.09 to 1.20 times on both corpora and both streams, with ranges apart. On Windows it leads liblzma at 0.83 to 0.89 times its time. It decodes xz in 0.61 to 0.75 times `lzma-rust2`'s time and LZMA in 0.82 to 0.89 times in every run.

## Question

Does Zipper's LZMA and xz decoder lead `lzma-rs` and `lzma-rust2` in pure Rust and liblzma through the `liblzma` crate, on an Apple M2 Max and x86-64 Linux and on two corpora? The metric is the wall time of one decode of an xz and an LZMA-alone stream, and of a one-thread `.tar.xz` extraction.

## Setup

- Two corpora, each run on three machines. Every run built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. It extracts to 211,938,580 bytes of files.
  - `dependencies.tar` is 90,931,200 bytes with sha256 prefix `b8191b36e54aadd5`. It holds the 114 crates that `Cargo.lock` pins, 5,501 files, and extracts to 85,944,317 bytes of files.
  - Each tar's sha256 and every stream's sha256 match on all three machines. Only the WIMs differ, since wimlib writes each one differently.
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
  - The dependencies rows, decode and extraction, ran again with the same binaries and the driver at `c16c47c1`, starting at 07:18 UTC on 14 November 2026. Its `corpus` sorts the tar's entries by path parts, so the Windows `dependencies.tar` now has the same sha256 as the other machines. The first Windows tar listed mixed-case names in another order.
  - bsdtar there is the `tar.exe` that Windows ships, libarchive 3.8.9, and 7-Zip is 26.05 x64.
  - Its paths are written as `<scratch>` and `<home>` in the JSON.
- From `49f68e30` to `4518a8af` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke tests, the workflow and Markdown pages changed. Every run measured the same decoders and the same bench code.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against the tar. Each run took 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar and 7-Zip 26.06 at one thread. Every run checked the extracted bytes, sizes and tree members.
- On 14 November 2026 the user decided that the best alternative is the fastest of the pure-Rust crates and the crates that wrap a C library. A decoder becomes a crate only when it leads that one with ranges apart on every operating system measured and both corpora.
- The ratio column is Zipper's median time over the other row's. Its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference. A row is compared only with rows of its own run.
- The streams are xz in one block with CRC-64 and LZMA-alone with an end marker, both written by liblzma at preset 6. The Silesia streams are 49,218,368 and 49,212,908 bytes, and the dependencies streams 8,762,484 and 8,761,025 bytes.
- On the M2 Max the Silesia LZMA decode ran in its own run, `m31-lzma-decode-m2-max.json`.

## Results

- Against `lzma-rust2`, the best pure-Rust decoder, Zipper takes 0.690 and 0.615 times its time on xz on Silesia and the dependencies on the M2 Max, and 0.749 and 0.667 times on Linux. On LZMA it takes 0.817 and 0.855 times on the M2 Max and 0.863 and 0.894 times on Linux. All eight ranges are apart.
- Against `lzma-rs`, Zipper takes 0.345 to 0.555 times its time in every run. `lzma-rs` peaks at 706.4 MiB on Silesia xz on the M2 Max, against 308.2 MiB for Zipper.
- Against liblzma through the `liblzma` crate 0.4.9, Zipper takes 0.826 to 0.874 times its time on the M2 Max, with ranges apart. On Linux it trails, at 1.125 and 1.089 times on xz and 1.180 and 1.199 times on LZMA, with ranges apart.
- One-thread `.tar.xz` extraction of the dependencies leads bsdtar and 7-Zip on both machines. On Silesia it leads bsdtar on the M2 Max but trails 7-Zip there at 1.110 times, and trails both on Linux, at 1.205 times bsdtar and 1.112 times 7-Zip.
- On the Windows desktop Zipper takes 0.740 and 0.651 times `lzma-rust2`'s time on xz and 0.862 and 0.889 times on LZMA. Against liblzma, built there by MSVC, it takes 0.832 and 0.829 times on xz and 0.854 and 0.885 times on LZMA. All eight ranges are apart.
- One-thread `.tar.xz` extraction on Windows leads bsdtar on both corpora and 7-Zip on the dependencies. On Silesia it trails 7-Zip at 1.132 times (1.121 to 1.142).

Every decode comparison, Zipper's median time over the other decoder's with its range, in each run:

| Stream | Against | Run | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- |
| `xz` | `lzma-rs` | Silesia on the Apple M2 Max | 0.555 (0.504 to 0.604) | Zipper ahead |
| `xz` | `lzma-rust2` | Silesia on the Apple M2 Max | 0.690 (0.648 to 0.770) | Zipper ahead |
| `xz` | `liblzma` | Silesia on the Apple M2 Max | 0.846 (0.788 to 0.947) | Zipper ahead |
| `xz` | `lzma-rs` | Silesia on hosted Linux | 0.518 (0.513 to 0.520) | Zipper ahead |
| `xz` | `lzma-rust2` | Silesia on hosted Linux | 0.749 (0.744 to 0.752) | Zipper ahead |
| `xz` | `liblzma` | Silesia on hosted Linux | 1.125 (1.120 to 1.131) | Zipper behind |
| `xz` | `lzma-rs` | Silesia on hosted Linux, first run | 0.471 (0.456 to 0.496) | Zipper ahead |
| `xz` | `lzma-rust2` | Silesia on hosted Linux, first run | 0.719 (0.670 to 0.754) | Zipper ahead |
| `xz` | `liblzma` | Silesia on hosted Linux, first run | 1.039 (0.978 to 1.088) | ranges overlap |
| `xz` | `lzma-rs` | Silesia on the Windows desktop | 0.508 (0.505 to 0.512) | Zipper ahead |
| `xz` | `lzma-rust2` | Silesia on the Windows desktop | 0.740 (0.737 to 0.745) | Zipper ahead |
| `xz` | `liblzma` | Silesia on the Windows desktop | 0.832 (0.826 to 0.837) | Zipper ahead |
| `xz` | `lzma-rs` | Dependencies on the Apple M2 Max | 0.449 (0.443 to 0.455) | Zipper ahead |
| `xz` | `lzma-rust2` | Dependencies on the Apple M2 Max | 0.615 (0.606 to 0.623) | Zipper ahead |
| `xz` | `liblzma` | Dependencies on the Apple M2 Max | 0.826 (0.812 to 0.837) | Zipper ahead |
| `xz` | `lzma-rs` | Dependencies on hosted Linux | 0.425 (0.420 to 0.430) | Zipper ahead |
| `xz` | `lzma-rust2` | Dependencies on hosted Linux | 0.667 (0.656 to 0.674) | Zipper ahead |
| `xz` | `liblzma` | Dependencies on hosted Linux | 1.089 (1.076 to 1.103) | Zipper behind |
| `xz` | `lzma-rs` | Dependencies on the Windows desktop | 0.426 (0.417 to 0.434) | Zipper ahead |
| `xz` | `lzma-rust2` | Dependencies on the Windows desktop | 0.651 (0.643 to 0.661) | Zipper ahead |
| `xz` | `liblzma` | Dependencies on the Windows desktop | 0.829 (0.820 to 0.841) | Zipper ahead |
| `lzma` | `lzma-rs` | Silesia on the Apple M2 Max | 0.471 (0.446 to 0.494) | Zipper ahead |
| `lzma` | `lzma-rust2` | Silesia on the Apple M2 Max | 0.817 (0.772 to 0.883) | Zipper ahead |
| `lzma` | `liblzma` | Silesia on the Apple M2 Max | 0.872 (0.824 to 0.938) | Zipper ahead |
| `lzma` | `lzma-rs` | Silesia on hosted Linux | 0.488 (0.483 to 0.493) | Zipper ahead |
| `lzma` | `lzma-rust2` | Silesia on hosted Linux | 0.863 (0.846 to 0.871) | Zipper ahead |
| `lzma` | `liblzma` | Silesia on hosted Linux | 1.180 (1.172 to 1.192) | Zipper behind |
| `lzma` | `lzma-rs` | Silesia on hosted Linux, first run | 0.437 (0.418 to 0.472) | Zipper ahead |
| `lzma` | `lzma-rust2` | Silesia on hosted Linux, first run | 0.846 (0.783 to 0.921) | Zipper ahead |
| `lzma` | `liblzma` | Silesia on hosted Linux, first run | 1.098 (1.034 to 1.180) | Zipper behind |
| `lzma` | `lzma-rs` | Silesia on the Windows desktop | 0.482 (0.477 to 0.491) | Zipper ahead |
| `lzma` | `lzma-rust2` | Silesia on the Windows desktop | 0.862 (0.847 to 0.878) | Zipper ahead |
| `lzma` | `liblzma` | Silesia on the Windows desktop | 0.854 (0.842 to 0.870) | Zipper ahead |
| `lzma` | `lzma-rs` | Dependencies on the Apple M2 Max | 0.345 (0.340 to 0.349) | Zipper ahead |
| `lzma` | `lzma-rust2` | Dependencies on the Apple M2 Max | 0.855 (0.841 to 0.866) | Zipper ahead |
| `lzma` | `liblzma` | Dependencies on the Apple M2 Max | 0.874 (0.856 to 0.887) | Zipper ahead |
| `lzma` | `lzma-rs` | Dependencies on hosted Linux | 0.386 (0.379 to 0.393) | Zipper ahead |
| `lzma` | `lzma-rust2` | Dependencies on hosted Linux | 0.894 (0.869 to 0.908) | Zipper ahead |
| `lzma` | `liblzma` | Dependencies on hosted Linux | 1.199 (1.176 to 1.219) | Zipper behind |
| `lzma` | `lzma-rs` | Dependencies on the Windows desktop | 0.388 (0.385 to 0.395) | Zipper ahead |
| `lzma` | `lzma-rust2` | Dependencies on the Windows desktop | 0.889 (0.881 to 0.908) | Zipper ahead |
| `lzma` | `liblzma` | Dependencies on the Windows desktop | 0.885 (0.877 to 0.900) | Zipper ahead |

### Silesia on the Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1
- `m31-lzma-decode-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 5.44 at the start, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1647.3 (1611.3 to 1749.4) | 1.647 | 308.2 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 2966.5 (2897.5 to 3199.2) | 2.967 | 706.4 | 0.555 (0.504 to 0.604) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 2387.8 (2272.9 to 2485.1) | 2.388 | 309.7 | 0.690 (0.648 to 0.770) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1946.1 (1848.1 to 2045.1) | 1.946 | 308.1 | 0.846 (0.788 to 0.947) | Zipper ahead |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1554.1 (1478.8 to 1573.2) | 1.554 | 308.1 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 3299.4 (3182.1 to 3316.4) | 3.299 | 310.0 | 0.471 (0.446 to 0.494) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1903.2 (1781.1 to 1915.7) | 1.903 | 308.1 | 0.817 (0.772 to 0.883) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1781.6 (1677.7 to 1794.5) | 1.782 | 308.1 | 0.872 (0.824 to 0.938) | Zipper ahead |

Extraction of the `xz` row, 49,218,368 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 1752.9 (1744.5 to 1761.2) | 1.776 | 18.8 |  |  |
| `zipper-all-threads` | all | 1759.3 (1747.4 to 1821.8) | 1.784 | 21.2 |  |  |
| `bsdtar` | 1 | 2173.1 (2100.2 to 2182.3) | 2.157 | 10.5 | 0.807 (0.799 to 0.839) | Zipper ahead |
| `7zip` | 1 | 1579.0 (1529.6 to 1592.7) | 1.580 | 12.6 | 1.110 (1.095 to 1.151) | Zipper behind |

### Silesia on hosted Linux

- `m31-decoders-linux-x86_64-silesia.json`, revision `4518a8af`, AMD EPYC 9V74 80-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.01 at the start, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1796.7 (1794.2 to 1800.5) | 1.797 | 260.1 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 3470.4 (3461.4 to 3498.7) | 3.470 | 656.5 | 0.518 (0.513 to 0.520) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 2400.4 (2395.0 to 2410.6) | 2.400 | 260.3 | 0.749 (0.744 to 0.752) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1596.9 (1591.8 to 1602.0) | 1.597 | 260.3 | 1.125 (1.120 to 1.131) | Zipper behind |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1687.1 (1678.5 to 1698.9) | 1.687 | 260.0 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 3455.9 (3445.1 to 3474.2) | 3.452 | 260.1 | 0.488 (0.483 to 0.493) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1955.1 (1949.9 to 1983.6) | 1.955 | 260.2 | 0.863 (0.846 to 0.871) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1430.3 (1425.8 to 1431.9) | 1.430 | 260.2 | 1.180 (1.172 to 1.192) | Zipper behind |

Extraction of the `xz` row, 49,218,368 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 1807.3 (1801.0 to 1825.5) | 1.840 | 19.9 |  |  |
| `zipper-all-threads` | all | 1810.6 (1802.5 to 1830.1) | 1.850 | 19.9 |  |  |
| `bsdtar` | 1 | 1500.0 (1494.1 to 1504.2) | 1.490 | 12.3 | 1.205 (1.197 to 1.222) | Zipper behind |
| `7zip` | 1 | 1625.5 (1622.4 to 1632.9) | 1.620 | 14.5 | 1.112 (1.103 to 1.125) | Zipper behind |

### Silesia on the Windows desktop

- `m34-decode-windows-silesia.json`, revision `5e6dd97b`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0
- `m34-extract-windows-silesia.json`, revision `3b324db6`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, extraction only, no load average on this system, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1472.5 (1470.1 to 1477.7) | not measured | 261.2 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 2899.8 (2884.0 to 2913.4) | not measured | 658.4 | 0.508 (0.505 to 0.512) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1989.0 (1984.0 to 1995.5) | not measured | 266.4 | 0.740 (0.737 to 0.745) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1769.3 (1765.5 to 1778.8) | not measured | 261.1 | 0.832 (0.826 to 0.837) | Zipper ahead |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1389.2 (1384.6 to 1411.4) | not measured | 261.1 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 2881.5 (2875.9 to 2899.9) | not measured | 262.1 | 0.482 (0.477 to 0.491) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1612.3 (1607.2 to 1634.8) | not measured | 261.1 | 0.862 (0.847 to 0.878) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1627.6 (1622.1 to 1644.1) | not measured | 261.1 | 0.854 (0.842 to 0.870) | Zipper ahead |

Extraction of the `xz` row, 49,218,368 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 1486.0 (1482.5 to 1494.8) | 1.531 | 19.8 |  |  |
| `zipper-all-threads` | all | 1486.6 (1483.8 to 1489.2) | 1.531 | 19.8 |  |  |
| `bsdtar` | 1 | 1702.5 (1694.6 to 1731.6) | 1.688 | 13.7 | 0.873 (0.856 to 0.882) | Zipper ahead |
| `7zip` | 1 | 1312.7 (1308.6 to 1322.6) | 0.016 | 7.7 | 1.132 (1.121 to 1.142) | Zipper behind |

### Dependencies on the Apple M2 Max

- `m31-decoders-m2-max-dependencies.json`, revision `4518a8af`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 3.61 at the start, busy processes at the start 2

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 309.2 (306.5 to 312.6) | 0.309 | 108.0 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 689.1 (686.6 to 692.2) | 0.689 | 281.8 | 0.449 (0.443 to 0.455) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 502.7 (501.9 to 505.8) | 0.503 | 116.3 | 0.615 (0.606 to 0.623) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 374.1 (373.3 to 377.6) | 0.374 | 108.0 | 0.826 (0.812 to 0.837) | Zipper ahead |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 257.5 (254.5 to 259.3) | 0.257 | 108.0 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 745.6 (742.3 to 749.2) | 0.746 | 118.0 | 0.345 (0.340 to 0.349) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 301.1 (299.6 to 302.8) | 0.301 | 108.0 | 0.855 (0.841 to 0.866) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 294.5 (292.3 to 297.4) | 0.295 | 108.0 | 0.874 (0.856 to 0.887) | Zipper ahead |

Extraction of the `xz` row, 8,762,484 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 545.7 (543.9 to 572.0) | 0.826 | 19.5 |  |  |
| `zipper-all-threads` | all | 473.6 (470.0 to 476.9) | 1.201 | 19.8 |  |  |
| `bsdtar` | 1 | 952.8 (946.2 to 973.3) | 0.946 | 11.2 | 0.573 (0.559 to 0.605) | Zipper ahead |
| `7zip` | 1 | 771.2 (761.4 to 789.1) | 0.817 | 12.6 | 0.708 (0.689 to 0.751) | Zipper ahead |

### Dependencies on hosted Linux

- `m31-decoders-linux-x86_64-dependencies.json`, revision `4518a8af`, AMD EPYC 7763 64-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.69 at the start, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 419.5 (416.0 to 422.4) | 0.419 | 106.4 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 986.7 (982.1 to 990.2) | 0.987 | 279.7 | 0.425 (0.420 to 0.430) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 629.0 (626.3 to 634.1) | 0.629 | 106.8 | 0.667 (0.656 to 0.674) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 385.0 (382.9 to 386.6) | 0.385 | 106.5 | 1.089 (1.076 to 1.103) | Zipper behind |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 364.6 (362.1 to 368.7) | 0.365 | 106.5 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 943.6 (938.5 to 955.2) | 0.943 | 106.5 | 0.386 (0.379 to 0.393) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 407.9 (406.2 to 416.5) | 0.408 | 106.4 | 0.894 (0.869 to 0.908) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 304.1 (302.5 to 307.9) | 0.304 | 106.5 | 1.199 (1.176 to 1.219) | Zipper behind |

Extraction of the `xz` row, 8,762,484 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 509.1 (505.0 to 535.4) | 0.680 | 20.6 |  |  |
| `zipper-all-threads` | all | 568.9 (562.7 to 610.8) | 0.910 | 20.7 |  |  |
| `bsdtar` | 1 | 663.1 (658.3 to 675.6) | 0.650 | 12.5 | 0.768 (0.747 to 0.813) | Zipper ahead |
| `7zip` | 1 | 710.1 (699.3 to 737.8) | 0.740 | 14.6 | 0.717 (0.684 to 0.766) | Zipper ahead |

### Dependencies on the Windows desktop

- `m34-decode-windows-dependencies.json`, revision `c16c47c1`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0
- `m34-extract-windows-dependencies.json`, revision `c16c47c1`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, extraction only, no load average on this system, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 289.6 (288.4 to 293.2) | not measured | 107.2 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 679.7 (675.6 to 691.5) | not measured | 273.6 | 0.426 (0.417 to 0.434) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 445.0 (443.3 to 448.8) | not measured | 109.4 | 0.651 (0.643 to 0.661) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 349.2 (348.7 to 351.6) | not measured | 107.1 | 0.829 (0.820 to 0.841) | Zipper ahead |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 253.6 (252.5 to 257.3) | not measured | 107.1 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 654.0 (651.2 to 655.2) | not measured | 108.1 | 0.388 (0.385 to 0.395) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 285.1 (283.4 to 286.5) | not measured | 107.2 | 0.889 (0.881 to 0.908) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 286.7 (285.8 to 287.9) | not measured | 107.1 | 0.885 (0.877 to 0.900) | Zipper ahead |

Extraction of the `xz` row, 8,762,484 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 4229.6 (4196.9 to 4244.9) | 3.625 | 19.3 |  |  |
| `zipper-all-threads` | all | 4198.3 (4126.9 to 4241.6) | 3.922 | 19.5 |  |  |
| `bsdtar` | 1 | 4605.5 (4549.1 to 4960.5) | 3.938 | 14.1 | 0.918 (0.846 to 0.933) | Zipper ahead |
| `7zip` | 1 | 6802.2 (6755.2 to 6862.1) | 0.016 | 7.7 | 0.622 (0.612 to 0.628) | Zipper ahead |

## Decision

The verdict is that it is not ready yet. On 14 November 2026 the user chose this over keeping it inside Zipper, since it is the fastest pure-Rust LZMA decoder in every run and leads liblzma on the M2 Max. [M35](../../../docs/roadmap/m35-zstandard-and-lzma-at-c-speed-on-x86-64.md) tries to close the Linux gap.

- liblzma through the `liblzma` crate is the best alternative, and Zipper trails it on x86-64 Linux on both corpora and both streams with ranges apart.
- The lead over both pure-Rust crates holds in every run, but the rule counts the C-wrapping crate too.
- M23 publishes this decoder as a crate and starts only if this verdict is a crate, so M23 does not start before M35 closes.
- No profile on x86-64 Linux shows where the gap comes from, so this record does not call it closable.
- Windows reverses the Linux gap. Zipper leads liblzma there on both corpora and both streams, with ranges apart. The C library was built by MSVC on Windows and by GCC on Linux, so the Linux gap may belong to the compiler or the machine. M35 checks how the C side was compiled. Linux still trails, so the verdict stays not ready yet.

## Limits

- The M2 Max was loaded during both of its runs, and both passed `--allow-busy`.
  - A `rustc` process used 98 to 100% of one core at the start of the Silesia decode and of each of its four extraction rows. The Silesia LZMA decode ran in its own run with no busy process.
  - `corespotlightd` used 153% and `fseventsd` 43% at the start of the dependencies decode, and `fseventsd` 47 to 48% at the start of three of its extraction rows.
- Two corpora, Silesia and the dependencies. Neither holds many small compressed frames or streams.
- The M2 Max WIM row ran on Silesia only, in its own run at `5e6dd97b` with wimlib 1.14.5. The dependencies have no WIM row on the M2 Max.
- No run has a `.tar.lzma` extraction row. Zipper writes the `.tar.lzma` out as one file, the `.tar`, where bsdtar reads the tar inside, so the trees differ.
- The three Linux runs each got a different AMD EPYC, so Linux numbers compare only within one run. Linux CPU times for child processes come in steps of 10 ms.
- Windows has no WIM extraction row, since `wimlib-imagex` is not installed there.
- On Windows the decode bench reports no CPU time for the decode call, and the driver has no load average. Its 7-Zip rows report the CPU time and peak RSS of the process that pipes 7-Zip's output, not of 7-Zip.
- On Windows the dependencies take 4.1 to 7.4 s to extract with every reader, so file creation on NTFS dominates those rows, not the decoder.
- The C libraries were built by MSVC on Windows and by the system C compiler on the M2 Max and Linux, so a gap to a C library can belong to its compiler.
- Each machine ran each corpus once, so session to session drift is not measured.
- No profile was taken. The limiter named is decode CPU, read from CPU time matching wall time at one thread.

## Reproduce

The [M31 page](../../../docs/roadmap/m31-a-release-case-for-each-decoder.md#measure) gives the commands for both corpora on both machines. The JSON is in [`data/`](../data) beside these records, with the second machine's build folder written as `<target>`, its checkout as `<checkout>` and its home folder as `<home>`. Every number on this page comes from

```sh
python3 benchmarks/decoders/tables.py lzma-and-xz
```
