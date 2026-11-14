# Zipper's XPRESS decoder against xpress-huffman

Not ready yet. Zipper decodes XPRESS chunks of Silesia in 0.33 times `xpress-huffman`'s time on an Apple M2 Max and 0.31 times on x86-64 Linux, with ranges apart. The M2 Max has no WIM extraction row, and the decoded chunks come from this bench's own encoder.

## Question

Does Zipper's XPRESS Huffman decoder lead the Rust crates that read XPRESS, of which `xpress-huffman` is the only one under a compatible licence, on both an Apple M2 Max and x86-64 Linux? The metric is the wall time of one decode of Silesia in 32 KiB XPRESS chunks, and of a one-thread WIM extraction.

## Setup

- Two machines, one session each. Both built with rustc 1.99.1 and no extra `RUSTFLAGS`.
  - The second machine, an Apple M2 Max with 12 cores and 64 GiB on macOS 27.0, at `49f68e30`. The decode and extraction run started at 22:58 UTC on 13 November 2026. The LZMA decode ran alone at 23:13 UTC. Its binaries ran from `<target>`, which stands for that machine's build folder in the JSON.
  - A hosted GitHub Actions runner, an AMD EPYC 9V45 with 4 cores and 16 GB on Linux 7.0 for Azure, at `479b42fd`. It is run 43533788639 of `m31-decoders.yml`, started at 00:26 UTC on 14 November 2026.
- From `49f68e30` to `479b42fd` only the driver, `scripts/compare_zip_readers.py`, the bench's smoke test, the workflow and the M31 page changed. Both machines ran the same decoders and the same bench code.
- `silesia.tar` is 211,957,760 bytes with sha256 prefix `1953965768007e93`. Its sha256 and every stream's sha256 match on both machines.
- `decode` ran each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand, and then checks every byte against `silesia.tar`. It ran 11 rounds after one warmup, in an order seeded with 20261114. The peak RSS is the whole process's, so it includes the input and the 202 MiB output buffer.
- `extract` ran `scripts/compare_zip_readers.py`, 7 runs after one warmup, with Zipper at one thread and at all threads, and bsdtar at one thread. 7-Zip 26.06 ran at one thread on the M2 Max only. Every run checked the extracted bytes, sizes and tree members, 211,938,580 bytes of files.
- The tables name each run's JSON file, its revision, its one-minute load at the start and how many processes the busy guard saw at the start. The ratio column is Zipper's median time over the other row's, and its range runs from Zipper's fastest over the other's slowest to Zipper's slowest over the other's fastest. "Ranges overlap" means no established difference.
- The stream is `silesia.tar.xpress`, 76,787,818 bytes, written by this bench's greedy encoder from MS-XCA in 32 KiB chunks with one code each, as WIM stores them.
- `xpress-huffman` gets two more zero bytes after each coded chunk, since it stops once its input cursor reaches the end of a chunk.
- On Linux `wimlib-imagex` wrote `silesia.wim`, 74,591,925 bytes with XPRESS chunks, for the extraction row. No C library for XPRESS was measured.

## Results

- Against `xpress-huffman`, Zipper takes 0.329 times its time (0.307 to 0.353) on the M2 Max and 0.314 times (0.305 to 0.333) on Linux, with ranges apart on both.
- `xpress-huffman` peaks at 480.6 MiB on the M2 Max and 378.0 MiB on Linux, against 352.9 and 279.8 MiB for Zipper, with the same input and output buffers.
- On Linux the WIM extracts in 544.1 ms at one thread and 548.2 ms at all threads. No other reader ran on it, since the runner found no `7zz`.

### Apple M2 Max

- `m31-decoders-m2-max.json`, revision `49f68e30`, 12 cores, 11 decode rounds, one-minute load 4.70 at the start, busy processes at the start 1

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 467.2 (449.2 to 488.8) | 0.467 | 352.9 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 1419.6 (1385.5 to 1464.6) | 1.420 | 480.6 | 0.329 (0.307 to 0.353) | Zipper ahead |

No extraction row for `xpress` in this run.

### AMD EPYC 9V45, Linux

- `m31-decoders-linux-x86_64.json`, revision `479b42fd`, 4 cores, 11 decode rounds, one-minute load 1.38 at the start, busy processes at the start 0

Decode of `xpress`, median wall ms of the decode call with its range, CPU s and peak RSS:

| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | `zipper` 0.1.0-alpha.3 | 394.0 (389.9 to 406.6) | 0.394 | 279.8 |  |  |
| `xpress-huffman` | `xpress-huffman` 0.1.3 | 1253.8 (1221.1 to 1278.6) | 1.254 | 378.0 | 0.314 (0.305 to 0.333) | Zipper ahead |

Extraction of the `xpress` row, 74,591,925 bytes into 211,938,580, median wall ms with its range, CPU s and peak RSS:

| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |
| --- | --- | --- | --- | --- | --- | --- |
| `zipper` | 1 | 544.1 (540.8 to 552.3) | 0.530 | 5.9 |  |  |
| `zipper-all-threads` | all | 548.2 (539.4 to 550.9) | 0.540 | 5.9 |  |  |

## Decision

The verdict is that it is not ready yet.

- The decode lead over `xpress-huffman` holds on both machines.
- M31's exit check 1 asks for extraction on both machines, and the M2 Max has none.
- The decoded chunks come from the bench's greedy encoder. Chunks written by wimlib or by Windows were decoded only inside the Linux WIM extraction, with no reader beside Zipper.
- An M2 Max WIM row, with 7-Zip on both machines, and a decode of wimlib's chunks would settle the case.

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
python3 benchmarks/decoders/tables.py xpress
```
