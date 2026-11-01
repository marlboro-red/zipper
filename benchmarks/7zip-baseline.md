# First baseline against 7-Zip

Zipper at Zstandard level 1 created and extracted these corpora faster than 7-Zip at LZMA2 level 1. That does not show a win at matched compression ratios, and the [matched-sync comparison against tar+zstd and tar+pzstd](v2/zstd-fullsync.md) is a closer baseline.

## Question

How do Zipper's creation speed, extraction speed, archive size and memory compare with 7-Zip on four synthetic and local corpora?

## Setup

- Engine revision `6acd322d2b11d52530b3606415e1f36981c68c74`, release build.
- Apple M5, 10 logical CPUs, 32 GiB RAM, macOS 26.7.1 arm64.
- Four requested threads for each engine and three runs per corpus.
- Zipper uses Zstandard level 1. 7-Zip 26.05 uses LZMA2 with `-mx=1`.
- `scripts/make_benchmark_corpus.py` generates deterministic logs, pseudorandom bytes, and 4,000 similar configuration files. It concatenates the regular files under `/usr/bin` for the binary sample. This machine provided 222.46 MiB. The exact binary source hashes are in `binary-sources.json`.
- `scripts/benchmark.py` runs Zipper first, then 7-Zip. Timing includes process startup and filesystem I/O and excludes hash verification. The script does not flush OS caches, control temperature or randomize engine order. The initial hash pass warms input caches.
- All extracted file bytes and directory trees passed verification.

## Results

Throughput comes from the median wall time.

| Corpus | Input MiB | Compress MiB/s: Zipper / 7-Zip | Extract MiB/s: Zipper / 7-Zip | Archive MiB: Zipper / 7-Zip |
| --- | ---: | ---: | ---: | ---: |
| logs | 256.00 | 2224.0 / 290.5 | 2444.0 / 876.5 | 46.701 / 48.856 |
| binaries | 222.46 | 2092.5 / 223.1 | 3715.1 / 560.8 | 73.692 / 55.529 |
| random | 256.00 | 1328.3 / 89.1 | 5293.7 / 5108.4 | 256.003 / 256.016 |
| small-files | 15.61 | 182.5 / 101.1 | 77.0 / 68.8 | 0.545 / 0.037 |

Peak RSS is measured per child with `wait4`. Each value is the median of the run peaks.

| Corpus | Compression peak RSS MiB: Zipper / 7-Zip | Extraction peak RSS MiB: Zipper / 7-Zip |
| --- | ---: | ---: |
| logs | 23.4 / 18.3 | 17.6 / 9.5 |
| binaries | 23.4 / 20.9 | 19.6 / 11.0 |
| random | 18.9 / 23.2 | 10.4 / 12.8 |
| small-files | 4.3 / 25.1 | 3.6 / 8.5 |

- **Logs.** Zipper compresses 7.7× faster and extracts 2.8× faster, with a 4.4% smaller archive.
- **Binaries.** Zipper compresses 9.4× faster and extracts 6.6× faster, with a 32.7% larger archive.
- **Random bytes.** Zipper compresses 14.9× faster. Neither engine reduces the size. Extraction is effectively tied at 1.04×.
- **Small files.** Zipper compresses 1.8× faster and extracts 1.12× faster, but its archive is 14.6× larger.

## Decision

The baseline set these priorities, in order:

1. Pack small files into shared independently decodable blocks, to improve the compression ratio and spread the per-file and per-task overhead.
2. Sweep compression levels and compare at matched archive sizes, including a Zstandard-based baseline.
3. Reuse codec contexts and overlap reading, codec work and writing. At this revision, batches have synchronization barriers and each file is processed separately.
4. Expand to public and real-workload corpora with larger runs, interleaved order and thread-scaling measurements.

## Limits

- The codecs differ and the levels are not equivalent. Zipper benefits from independently decodable blocks.
- There was no comparison against a tuned Zstandard archiver, LZ4, other 7-Zip presets or a public benchmark corpus.
- The log and small-file data are synthetic. The small files share redundant text on purpose, which exposes the cost of compressing each file independently instead of solidly.
- Zipper syncs the archive before publishing it. The benchmark does not force the same durability behavior on 7-Zip. Extraction timing does not guarantee durable writes.
- These are local application timings, not isolated codec or storage bandwidth.
- Some operations last only 50–200 ms, so treat small differences as noise, especially random-data extraction. Raw samples are in each corpus JSON.
- CPU utilization and energy are not measured.

## Reproduce

```sh
cargo build --release
python3 scripts/make_benchmark_corpus.py /tmp/zipper-corpus --mib 256
python3 scripts/benchmark.py /tmp/zipper-corpus/logs --runs 3 --threads 4 --sevenzip 7zz
# Repeat for binaries, random, and small-files.
```
