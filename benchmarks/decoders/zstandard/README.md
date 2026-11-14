# Zipper's Zstandard decoder against ruzstd and libzstd

Becomes a crate, once a second corpus repeats the lead. Zipper decodes Silesia in 0.29 times `ruzstd`'s time on an Apple M2 Max and 0.33 times on x86-64 Linux, with ranges apart. It leads libzstd on the M2 Max only, so no lead over libzstd is claimed.

## Question

Does Zipper's Zstandard decoder lead the Zstandard decoders a Rust program can use today, `ruzstd` in pure Rust and libzstd through the `zstd` crate, on both an Apple M2 Max and x86-64 Linux? The metric is the wall time of one decode of a Silesia `.zst` stream, and of a one-thread `.tar.zst` extraction.

## Setup

- Two machines, one session each. Both built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - The second machine, an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0, at `49f68e30`. The decode and extraction run started at 22:58 UTC on 13 November 2026. The LZMA decode ran alone at 23:13 UTC. Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
  - A hosted GitHub Actions runner, an AMD EPYC 9V45 with 4 cores and 16 GB on Linux 7.0 for Azure, at `479b42fd`. It is run 43533788639 of `m31-decoders.yml`, started at 00:26 UTC on 14 November 2026.
- From `49f68e30` to `479b42fd` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke test, the workflow and the M31 page changed. Both machines ran the same decoders and the same bench code.
- `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. Its sha256 and every stream's sha256 match on both machines.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against `silesia.tar`. It ran 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the 202 MiB output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar at one thread. 7-Zip 26.06 ran at one thread on the M2 Max only. Every run checked the extracted bytes, sizes and tree members, 211,938,580 bytes of files.
- The tables name each run's JSON file, its revision, its one-minute load at the start and how many processes the busy guard saw at the start. The ratio column is Zipper's median time over the other row's, and its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference.
- The stream is `silesia.tar.zst`, 66,497,280 bytes, one frame with its checksum, written by libzstd at level 3.

## Results

- Against `ruzstd`, the best pure-Rust decoder, Zipper takes 0.292 times its time (0.251 to 0.305) on the M2 Max and 0.332 times (0.313 to 0.347) on Linux. The ranges are apart on both.
- Against libzstd 1.5.7, Zipper takes 0.913 times its time (0.897 to 0.950) on the M2 Max, with ranges apart. On Linux the median is 1.214 times. libzstd's slowest sample, 174.8 ms, reaches into Zipper's range, so the ranges overlap.
- Decode CPU time matches wall time on both machines. Every decoder's peak RSS sits within 10 MiB of the others on each machine.
- One-thread `.tar.zst` extraction takes 0.815 times bsdtar's time on the M2 Max and 0.794 times on Linux, and 0.739 times 7-Zip's on the M2 Max, all with ranges apart. All threads do not change it.

### Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

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

### AMD EPYC 9V45, Linux

- `m31-decoders-linux-x86_64.json`, revision `479b42fd`, 4 cores, 11 decode rounds, one-minute load 1.38 at the start, busy processes at the start 0

Decode of `zstd`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 149.0 (145.2 to 152.9) | 0.149 | 271.0 |  |  |
| `ruzstd` | `ruzstd` 0.9.1 | 448.7 (440.0 to 464.4) | 0.449 | 273.7 | 0.332 (0.313 to 0.347) | Zipper ahead |
| `libzstd` | `zstd` 0.14.3, `zstd-safe` 8.2.0, `zstd-sys` 2.1.2+zstd.1.5.7 | 122.8 (120.8 to 174.8) | 0.123 | 270.3 | 1.214 (0.831 to 1.265) | ranges overlap |

Extraction of the `zstd` row, 66,497,280 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 151.2 (149.2 to 159.5) | 0.180 | 14.3 |  |  |
| `zipper-all-threads` | all | 156.7 (153.6 to 171.3) | 0.190 | 15.2 |  |  |
| `bsdtar` | 1 | 190.4 (189.5 to 198.6) | 0.180 | 6.9 | 0.794 (0.752 to 0.842) | Zipper ahead |

## Decision

The verdict is that it becomes a crate, held until a second corpus repeats the lead over `ruzstd`.

- The lead over `ruzstd` is three times on both machines, with ranges apart, so it rests on two machines.
- No lead over libzstd is claimed. Zipper leads on the M2 Max, and on Linux its median is behind with the ranges overlapping.
- A crate's claim is a pure-Rust Zstandard decoder about three times `ruzstd`'s speed on Silesia, on these two machines.

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
python3 benchmarks/decoders/tables.py zstandard
```
