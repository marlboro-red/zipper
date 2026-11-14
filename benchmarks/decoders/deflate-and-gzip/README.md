# Zipper's Deflate and gzip decoder against zlib-rs, miniz_oxide and libdeflate

Stays inside Zipper. Zipper decodes Silesia in 0.72 times `zlib-rs`'s time on an Apple M2 Max, but ties it on x86-64 Linux with ranges overlapping, and trails libdeflate there by 1.16 to 1.22 times.

## Question

Does Zipper's Deflate decoder lead the Deflate decoders a Rust program can use today, `zlib-rs` and `miniz_oxide` in pure Rust and libdeflate through `libdeflater`, on both an Apple M2 Max and x86-64 Linux? The metric is the wall time of one decode of a raw Deflate and a gzip stream of Silesia, and of a one-thread `.tar.gz` extraction.

## Setup

- Two machines, one session each. Both built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - The second machine, an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0, at `49f68e30`. The decode and extraction run started at 22:58 UTC on 13 November 2026. The LZMA decode ran alone at 23:13 UTC. Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
  - A hosted GitHub Actions runner, an AMD EPYC 9V45 with 4 cores and 16 GB on Linux 7.0 for Azure, at `479b42fd`. It is run 43533788639 of `m31-decoders.yml`, started at 00:26 UTC on 14 November 2026.
- From `49f68e30` to `479b42fd` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke test, the workflow and the M31 page changed. Both machines ran the same decoders and the same bench code.
- `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. Its sha256 and every stream's sha256 match on both machines.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against `silesia.tar`. It ran 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the 202 MiB output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar at one thread. 7-Zip 26.06 ran at one thread on the M2 Max only. Every run checked the extracted bytes, sizes and tree members, 211,938,580 bytes of files.
- The tables name each run's JSON file, its revision, its one-minute load at the start and how many processes the busy guard saw at the start. The ratio column is Zipper's median time over the other row's, and its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference.
- The streams are `silesia.tar.deflate`, 68,915,303 bytes of raw Deflate, and `silesia.tar.gz`, 68,915,321 bytes, both written by `flate2` with `zlib-rs` at level 6.
- The gzip extraction row covers Deflate, since a raw Deflate stream has no container to extract.

## Results

- Against `zlib-rs`, the best pure-Rust decoder, Zipper takes 0.720 times its time on raw Deflate (0.677 to 0.747) and 0.721 times on gzip (0.667 to 0.738) on the M2 Max, with ranges apart.
- On Linux the same ratios are 1.015 (0.952 to 1.056) and 0.950 (0.898 to 1.019). Both ranges overlap, so Zipper and `zlib-rs` tie there.
- Against `miniz_oxide`, Zipper takes 0.438 and 0.446 times its time on the M2 Max and 0.646 and 0.643 times on Linux, all with ranges apart.
- Against libdeflate through `libdeflater` 1.26.1, the ranges overlap on the M2 Max, at medians of 1.053 and 1.078. On Linux Zipper trails, at 1.219 (1.119 to 1.260) on raw Deflate and 1.163 (1.113 to 1.241) on gzip.
- One-thread `.tar.gz` extraction takes 0.680 times bsdtar's time on the M2 Max and 0.435 times on Linux, and 0.297 times 7-Zip's on the M2 Max, all with ranges apart.

### Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

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

### AMD EPYC 9V45, Linux

- `m31-decoders-linux-x86_64.json`, revision `479b42fd`, 4 cores, 11 decode rounds, one-minute load 1.38 at the start, busy processes at the start 0

Decode of `deflate`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 211.5 (206.3 to 215.5) | 0.212 | 272.1 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 208.4 (204.1 to 216.7) | 0.208 | 272.1 | 1.015 (0.952 to 1.056) | ranges overlap |
| `miniz_oxide` | `miniz_oxide` 0.9.1 | 327.4 (319.9 to 338.0) | 0.327 | 272.0 | 0.646 (0.610 to 0.674) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 173.6 (171.1 to 184.3) | 0.174 | 272.1 | 1.219 (1.119 to 1.260) | Zipper behind |

Decode of `gzip`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 212.9 (207.7 to 220.8) | 0.213 | 272.1 |  |  |
| `zlib-rs` | `zlib-rs` 0.6.8 | 224.1 (216.8 to 231.3) | 0.224 | 272.2 | 0.950 (0.898 to 1.019) | ranges overlap |
| `miniz_oxide` | `miniz_oxide` 0.9.1, `crc32fast` 1.5.3 | 331.1 (325.5 to 338.1) | 0.331 | 271.9 | 0.643 (0.614 to 0.678) | Zipper ahead |
| `libdeflate` | `libdeflater` 1.26.1, `libdeflate-sys` 1.26.1 | 183.0 (178.0 to 186.6) | 0.183 | 272.1 | 1.163 (1.113 to 1.241) | Zipper behind |

Extraction of the `gzip` row, 68,915,321 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 219.1 (216.2 to 223.2) | 0.250 | 11.1 |  |  |
| `zipper-all-threads` | all | 220.6 (217.4 to 235.5) | 0.250 | 11.0 |  |  |
| `bsdtar` | 1 | 504.2 (496.1 to 514.4) | 0.500 | 4.6 | 0.435 (0.420 to 0.450) | Zipper ahead |

## Decision

The verdict is that it stays inside Zipper.

- The lead over `zlib-rs` holds on the M2 Max only. On x86-64 Linux the two tie, so no lead over the best pure-Rust decoder rests on two machines.
- Zipper trails libdeflate on Linux and ties it on the M2 Max.
- The decoder keeps its place in Zipper, where `.tar.gz` extraction leads bsdtar on both machines.

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
python3 benchmarks/decoders/tables.py deflate-and-gzip
```
