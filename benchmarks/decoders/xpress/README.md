# Zipper's XPRESS decoder against xpress-huffman

Becomes a crate. Zipper decodes XPRESS chunks in 0.28 to 0.33 times `xpress-huffman`'s time on both corpora on an Apple M2 Max, on x86-64 Linux and on Windows, with ranges apart in every run. Windows has no WIM extraction row. No crate wraps a C library for XPRESS. Silesia WIM extraction still takes 1.35 times 7-Zip's time on the M2 Max and 1.30 times on Linux.

## Question

Does Zipper's XPRESS Huffman decoder lead `xpress-huffman`, the only Rust crate under a compatible licence that reads it, on an Apple M2 Max and x86-64 Linux and on two corpora? The metric is the wall time of one decode of a tar in 32 KiB XPRESS chunks, and of a one-thread WIM extraction.

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
- The streams come from this bench's greedy encoder, written from MS-XCA, in 32 KiB chunks with one code each, as WIM stores them. `silesia.tar.xpress` is 76,787,818 bytes and `dependencies.tar.xpress` 19,402,251 bytes.
- `xpress-huffman` gets two more zero bytes after each coded chunk, since it stops once its input cursor reaches the end of a chunk.
- `wimlib-imagex` wrote each WIM with XPRESS chunks for the extraction rows. On Linux they are 74,591,929 bytes for Silesia and 19,023,514 bytes for the dependencies. On the M2 Max wimlib 1.14.5 wrote the Silesia WIM, 74,591,891 bytes, in `m31-wim-m2-max-silesia.json`.
- No crate that wraps a C library reads XPRESS, so `xpress-huffman` is the best alternative.

## Results

- Against `xpress-huffman`, Zipper takes 0.329 and 0.330 times its time on Silesia and the dependencies on the M2 Max, and 0.296 and 0.279 times on Linux, with ranges apart in every run.
- `xpress-huffman` peaks at more memory than Zipper in every run, with the same input and output buffers.
- The Silesia WIM extracts at one thread in 1.348 times 7-Zip's time on the M2 Max (1.144 to 1.577) and 1.304 times on Linux (1.299 to 1.310). All threads change nothing, at 622.8 against 622.0 ms on the M2 Max.
- On Linux the dependencies WIM extracts in 0.971 times 7-Zip's time (0.941 to 0.980).
- On the Windows desktop Zipper takes 0.332 and 0.317 times `xpress-huffman`'s time, with ranges apart. Windows has no WIM extraction row, since `wimlib-imagex` is not installed there.

Every decode comparison, Zipper's median time over the other decoder's with its range, in each run:

| Stream | Against | Run | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- |
| `xpress` | `xpress-huffman` | Silesia on the Apple M2 Max | 0.329 (0.307 to 0.353) | Zipper ahead |
| `xpress` | `xpress-huffman` | Silesia on hosted Linux | 0.296 (0.292 to 0.300) | Zipper ahead |
| `xpress` | `xpress-huffman` | Silesia on hosted Linux, first run | 0.314 (0.305 to 0.333) | Zipper ahead |
| `xpress` | `xpress-huffman` | Silesia on the Windows desktop | 0.332 (0.328 to 0.341) | Zipper ahead |
| `xpress` | `xpress-huffman` | Dependencies on the Apple M2 Max | 0.330 (0.327 to 0.334) | Zipper ahead |
| `xpress` | `xpress-huffman` | Dependencies on hosted Linux | 0.279 (0.272 to 0.291) | Zipper ahead |
| `xpress` | `xpress-huffman` | Dependencies on the Windows desktop | 0.317 (0.314 to 0.327) | Zipper ahead |

### Silesia on the Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1
- `m31-wim-m2-max-silesia.json`, revision `5e6dd97b`, Apple M2 Max with 12 cores, extraction only, one-minute load 2.29 at the start, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 467.2 (449.2 to 488.8) | 0.467 | 352.9 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 1419.6 (1385.5 to 1464.6) | 1.420 | 480.6 | 0.329 (0.307 to 0.353) | Zipper ahead |

Extraction of the `xpress` row, 74,591,891 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 622.0 (603.2 to 696.0) | 0.612 | 3.9 |  |  |
| `zipper-all-threads` | all | 622.8 (614.6 to 750.4) | 0.614 | 4.2 |  |  |
| `7zip` | 1 | 461.4 (441.3 to 527.2) | 0.453 | 3.5 | 1.348 (1.144 to 1.577) | Zipper behind |

### Silesia on hosted Linux

- `m31-decoders-linux-x86_64-silesia.json`, revision `4518a8af`, AMD EPYC 9V74 80-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.01 at the start, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 449.8 (449.1 to 453.7) | 0.450 | 279.7 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 1520.5 (1514.0 to 1537.7) | 1.520 | 378.0 | 0.296 (0.292 to 0.300) | Zipper ahead |

Extraction of the `xpress` row, 74,591,929 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 631.8 (630.7 to 634.2) | 0.620 | 5.9 |  |  |
| `zipper-all-threads` | all | 630.1 (629.4 to 632.1) | 0.620 | 5.8 |  |  |
| `7zip` | 1 | 484.6 (484.0 to 485.7) | 0.480 | 5.6 | 1.304 (1.299 to 1.310) | Zipper behind |

### Silesia on the Windows desktop

- `m34-decode-windows-silesia.json`, revision `5e6dd97b`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 412.1 (410.9 to 419.6) | not measured | 281.7 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 1240.7 (1230.9 to 1254.0) | not measured | 388.1 | 0.332 (0.328 to 0.341) | Zipper ahead |

No extraction row for `xpress` in this run.

### Dependencies on the Apple M2 Max

- `m31-decoders-m2-max-dependencies.json`, revision `4518a8af`, Apple M2 Max with 12 cores, 11 decode rounds, one-minute load 3.61 at the start, busy processes at the start 2

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 131.4 (131.2 to 131.8) | 0.131 | 128.3 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 398.1 (394.7 to 401.7) | 0.398 | 168.7 | 0.330 (0.327 to 0.334) | Zipper ahead |

No extraction row for `xpress` in this run.

### Dependencies on hosted Linux

- `m31-decoders-linux-x86_64-dependencies.json`, revision `4518a8af`, AMD EPYC 7763 64-Core Processor with 4 cores, 11 decode rounds, one-minute load 1.69 at the start, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 150.0 (149.3 to 153.1) | 0.150 | 110.3 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 538.4 (525.5 to 547.9) | 0.538 | 139.6 | 0.279 (0.272 to 0.291) | Zipper ahead |

Extraction of the `xpress` row, 19,023,514 bytes into 85,944,317, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 450.8 (448.9 to 454.0) | 0.440 | 8.3 |  |  |
| `zipper-all-threads` | all | 443.9 (442.3 to 457.6) | 0.440 | 8.2 |  |  |
| `7zip` | 1 | 464.5 (463.3 to 477.1) | 0.450 | 7.1 | 0.971 (0.941 to 0.980) | Zipper ahead |

### Dependencies on the Windows desktop

- `m34-decode-windows-dependencies.json`, revision `5e6dd97b`, AMD64 Family 25 Model 33 Stepping 0, AuthenticAMD with 12 cores, 11 decode rounds, no load average on this system, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 116.9 (116.5 to 119.9) | not measured | 111.3 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 368.2 (366.7 to 370.5) | not measured | 143.5 | 0.317 (0.314 to 0.327) | Zipper ahead |

No extraction row for `xpress` in this run.

## Decision

The verdict is that it becomes a crate.

- The decode lead over `xpress-huffman`, the best alternative, holds on both machines and both corpora, with ranges apart in every run.
- As a finding, Silesia WIM extraction trails 7-Zip on both machines, at 1.348 and 1.304 times its time. The decode bench does not show where that time goes, so the WIM reader around the decoder needs a profile.
- The extraction rows decode chunks that wimlib wrote, so the case does not rest on the bench's own encoder alone.
- Windows confirms the decode lead on both corpora. It has no WIM extraction row, which M34's exit check 1 asks for.

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
python3 benchmarks/decoders/tables.py xpress
```
