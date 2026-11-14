# Zipper's BZip2 decoder against libbz2-rs-sys and libbzip2

Becomes a crate, once a second corpus repeats the lead. At one thread Zipper decodes Silesia in 0.61 to 0.66 times the time of `libbz2-rs-sys` and of libbzip2 1.0.8 on both an Apple M2 Max and x86-64 Linux, with ranges apart. It is the only decoder here that leads its C library on both machines.

## Question

Does Zipper's BZip2 decoder lead the BZip2 decoders a Rust program can use today, `libbz2-rs-sys` in pure Rust and libbzip2 through `bzip2-sys`, on both an Apple M2 Max and x86-64 Linux? The metric is the wall time of one decode of a Silesia `.bz2` stream at one thread, and of a one-thread `.tar.bz2` extraction.

## Setup

- Two machines, one session each. Both built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - The second machine, an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0, at `49f68e30`. The decode and extraction run started at 22:58 UTC on 13 November 2026. The LZMA decode ran alone at 23:13 UTC. Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
  - A hosted GitHub Actions runner, an AMD EPYC 9V45 with 4 cores and 16 GB on Linux 7.0 for Azure, at `479b42fd`. It is run 43533788639 of `m31-decoders.yml`, started at 00:26 UTC on 14 November 2026.
- From `49f68e30` to `479b42fd` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke test, the workflow and the M31 page changed. Both machines ran the same decoders and the same bench code.
- `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. Its sha256 and every stream's sha256 match on both machines.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against `silesia.tar`. It ran 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the 202 MiB output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar at one thread. 7-Zip 26.06 ran at one thread on the M2 Max only. Every run checked the extracted bytes, sizes and tree members, 211,938,580 bytes of files.
- The tables name each run's JSON file, its revision, its one-minute load at the start and how many processes the busy guard saw at the start. The ratio column is Zipper's median time over the other row's, and its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference.
- The stream is `silesia.tar.bz2`, 54,595,355 bytes, written by `libbz2-rs-sys` at level 9.
- Zipper's decoder ran twice, on a one-thread Rayon pool as `zipper` and on the global pool as `zipper-all-threads`. The other decoders run on one thread.

## Results

- Against `libbz2-rs-sys`, the pure-Rust decoder, Zipper at one thread takes 0.609 times its time (0.570 to 0.682) on the M2 Max and 0.651 times (0.617 to 0.711) on Linux.
- Against libbzip2 1.0.8, it takes 0.663 times (0.625 to 0.743) on the M2 Max and 0.644 times (0.605 to 0.687) on Linux. All four ranges are apart.
- On the global pool Zipper decodes in 361.2 ms on the M2 Max's 12 cores and 1,121.5 ms on the runner's 4. Its CPU time rises from 2.2 to 3.2 s and from 2.5 to 3.4 s.
- One-thread `.tar.bz2` extraction takes 0.665 times bsdtar's time on the M2 Max and 0.612 times on Linux, with ranges apart. Against 7-Zip on the M2 Max the ranges overlap, at a median of 0.903, because Zipper's slowest run took 2,630.1 ms.
- At all threads the extraction takes 451.6 ms on the M2 Max and 1,200.7 ms on Linux, with peak RSS of 91.5 and 52.0 MiB.

### Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

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

### AMD EPYC 9V45, Linux

- `m31-decoders-linux-x86_64.json`, revision `479b42fd`, 4 cores, 11 decode rounds, one-minute load 1.38 at the start, busy processes at the start 0

Decode of `bzip2`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 2469.9 (2407.3 to 2593.5) | 2.469 | 266.0 |  |  |
| `zipper-all-threads` | `zipper` 0.1.0-alpha.3, `rayon` 1.12.0 | 1121.5 (1108.0 to 1167.8) | 3.391 | 279.4 |  |  |
| `libbz2-rs-sys` | `bzip2` 0.6.1, `libbz2-rs-sys` 0.2.6 | 3795.4 (3650.1 to 3899.9) | 3.795 | 260.8 | 0.651 (0.617 to 0.711) | Zipper ahead |
| `libbzip2` | `bzip2-sys` 0.1.13+1.0.8 | 3833.7 (3776.7 to 3980.1) | 3.833 | 260.8 | 0.644 (0.605 to 0.687) | Zipper ahead |

Extraction of the `bzip2` row, 54,595,355 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 2501.8 (2459.8 to 2523.8) | 2.540 | 26.4 |  |  |
| `zipper-all-threads` | all | 1200.7 (1185.3 to 1227.7) | 3.580 | 52.0 |  |  |
| `bsdtar` | 1 | 4088.4 (4044.2 to 4142.4) | 4.080 | 7.6 | 0.612 (0.594 to 0.624) | Zipper ahead |

## Decision

The verdict is that it becomes a crate, held until a second corpus repeats the lead.

- At one thread Zipper leads both the pure-Rust decoder and libbzip2 on both machines, with ranges apart.
- The parallel block decoder adds a lead that no alternative here offers, at the cost of more CPU time and memory.

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
python3 benchmarks/decoders/tables.py bzip2
```
