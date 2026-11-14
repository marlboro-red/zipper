# Zipper's LZMA and xz decoder against lzma-rs, lzma-rust2 and liblzma

Becomes a crate, once a second corpus repeats the lead. Zipper decodes Silesia xz in 0.69 times `lzma-rust2`'s time on an Apple M2 Max and 0.72 times on x86-64 Linux, with ranges apart. It trails or ties liblzma on Linux, so no lead over liblzma is claimed.

## Question

Does Zipper's LZMA and xz decoder lead `lzma-rs` and `lzma-rust2` in pure Rust and liblzma through the `liblzma` crate, on both an Apple M2 Max and x86-64 Linux? The metric is the wall time of one decode of an xz and an LZMA-alone stream of Silesia, and of a one-thread `.tar.xz` extraction.

## Setup

- Two machines, one session each. Both built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - The second machine, an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0, at `49f68e30`. The decode and extraction run started at 22:58 UTC on 13 November 2026. The LZMA decode ran alone at 23:13 UTC. Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
  - A hosted GitHub Actions runner, an AMD EPYC 9V45 with 4 cores and 16 GB on Linux 7.0 for Azure, at `479b42fd`. It is run 43533788639 of `m31-decoders.yml`, started at 00:26 UTC on 14 November 2026.
- From `49f68e30` to `479b42fd` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke test, the workflow and the M31 page changed. Both machines ran the same decoders and the same bench code.
- `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. Its sha256 and every stream's sha256 match on both machines.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against `silesia.tar`. It ran 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the 202 MiB output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar at one thread. 7-Zip 26.06 ran at one thread on the M2 Max only. Every run checked the extracted bytes, sizes and tree members, 211,938,580 bytes of files.
- The tables name each run's JSON file, its revision, its one-minute load at the start and how many processes the busy guard saw at the start. The ratio column is Zipper's median time over the other row's, and its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference.
- The streams are `silesia.tar.xz`, 49,218,368 bytes in one block with CRC-64, and `silesia.tar.lzma`, 49,212,908 bytes of LZMA-alone with an end marker, both written by liblzma at preset 6.
- On the M2 Max the xz decode ran in the main run and the LZMA decode in its own run, `m31-lzma-decode-m2-max.json`.

## Results

- Against `lzma-rust2`, the best pure-Rust decoder, Zipper takes 0.690 times its time on xz (0.648 to 0.770) and 0.817 times on LZMA (0.772 to 0.883) on the M2 Max. On Linux it takes 0.719 times (0.670 to 0.754) and 0.846 times (0.783 to 0.921). All four ranges are apart.
- Against `lzma-rs`, Zipper takes 0.555 and 0.471 times its time on the M2 Max and 0.471 and 0.437 times on Linux. `lzma-rs` peaks at 706.4 MiB on xz on the M2 Max, against 308.2 MiB for Zipper.
- Against liblzma through the `liblzma` crate 0.4.9, Zipper takes 0.846 times its time on xz (0.788 to 0.947) and 0.872 times on LZMA (0.824 to 0.938) on the M2 Max, with ranges apart. On Linux the xz ranges overlap at a median of 1.039, and Zipper trails on LZMA at 1.098 (1.034 to 1.180).
- One-thread `.tar.xz` extraction takes 0.807 times bsdtar's time on the M2 Max and 1.110 times 7-Zip's, both with ranges apart. On Linux it takes 1.131 times bsdtar's time (1.083 to 1.161).

### Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1
- `m31-lzma-decode-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 5.44 at the start, busy processes at the start 0

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

### AMD EPYC 9V45, Linux

- `m31-decoders-linux-x86_64.json`, revision `479b42fd`, 4 cores, 11 decode rounds, one-minute load 1.38 at the start, busy processes at the start 0

Decode of `xz`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1403.1 (1380.1 to 1445.1) | 1.403 | 260.2 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 2981.9 (2911.7 to 3028.8) | 2.982 | 656.5 | 0.471 (0.456 to 0.496) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1950.7 (1916.4 to 2061.0) | 1.951 | 260.3 | 0.719 (0.670 to 0.754) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1350.7 (1328.1 to 1410.5) | 1.351 | 260.3 | 1.039 (0.978 to 1.088) | ranges overlap |

Decode of `lzma`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 1327.4 (1299.0 to 1415.4) | 1.327 | 260.2 |  |  |
| `lzma-rs` | `lzma-rs` 0.3.0 | 3038.3 (2998.5 to 3105.9) | 3.038 | 260.3 | 0.437 (0.418 to 0.472) | Zipper ahead |
| `lzma-rust2` | `lzma-rust2` 0.23.0 | 1569.8 (1537.6 to 1659.9) | 1.570 | 260.3 | 0.846 (0.783 to 0.921) | Zipper ahead |
| `liblzma` | `liblzma` 0.4.9, `liblzma-sys` 0.4.9 | 1208.4 (1199.5 to 1256.3) | 1.208 | 260.3 | 1.098 (1.034 to 1.180) | Zipper behind |

Extraction of the `xz` row, 49,218,368 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 1401.5 (1374.7 to 1428.0) | 1.430 | 20.0 |  |  |
| `zipper-all-threads` | all | 1393.2 (1376.9 to 1409.9) | 1.430 | 19.9 |  |  |
| `bsdtar` | 1 | 1239.3 (1229.9 to 1268.9) | 1.230 | 12.3 | 1.131 (1.083 to 1.161) | Zipper behind |

## Decision

The verdict is that it becomes a crate, held until a second corpus repeats the lead over `lzma-rust2`.

- The lead over both pure-Rust crates holds on both machines, with ranges apart, on xz and on LZMA.
- No lead over liblzma is claimed. Zipper leads it on the M2 Max, ties it on Linux xz and trails it on Linux LZMA. bsdtar, which uses liblzma, extracts `.tar.xz` faster on Linux too.
- M23 publishes this decoder as a crate. It should start once a second corpus repeats the lead over `lzma-rust2`. Its exit check 2 asks for liblzma on Linux with ranges apart or a stated tie, and these rows show a tie on xz and a loss on LZMA.

## Limits

- A `rustc` process used 98 to 100% of one core on the M2 Max at the start of its decode run and at the start of each of its four extraction rows. The run passed `--allow-busy`, and its one-minute load was 4.70 at the start. The LZMA decode ran in its own run with no busy process.
- One corpus, Silesia. Exit check 3 of M31 lets no lead rest on one corpus.
- The M2 Max has no WIM extraction row, because `wimlib-imagex` is not installed there to write `silesia.wim`.
- No run has a `.tar.lzma` extraction row. Zipper writes `silesia.tar.lzma` out as one file, `silesia.tar`, where bsdtar reads the tar inside, so the trees differ.
- The Linux run found no `7zz`, as its `references_missing` lists, so its extraction rows compare Zipper with bsdtar alone. Its CPU times for child processes come in steps of 10 ms.
- Each machine ran one session, and the Linux runner is a shared hosted machine. Session to session drift on either machine is not measured.
- No profile was taken. The limiter named is decode CPU, read from CPU time matching wall time at one thread.

## Reproduce

The [M31 page](../../../docs/roadmap/m31-a-release-case-for-each-decoder.md#measure) gives the commands for both machines. The JSON is in [`data/`](../data) beside these records, with the second machine's build folder written as `<target>` and its home folder as `<home>`. Every number on this page comes from

```sh
python3 benchmarks/decoders/tables.py lzma-and-xz
```
