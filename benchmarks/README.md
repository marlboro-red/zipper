# Local performance baseline — 2026-10-29

Measured engine revision: `6acd322d2b11d52530b3606415e1f36981c68c74`.

Apple M5, 10 logical CPUs, 32 GiB RAM, macOS 26.7.1 arm64. Release build; four requested threads for each engine; three runs per corpus. Zipper uses Zstandard level 1; 7-Zip 26.05 uses LZMA2 with `-mx=1`. Values below are throughput derived from median wall time. All extracted file bytes and directory trees passed verification.

| Corpus | Input MiB | Compress MiB/s: Zipper / 7-Zip | Extract MiB/s: Zipper / 7-Zip | Archive MiB: Zipper / 7-Zip |
| --- | ---: | ---: | ---: | ---: |
| logs | 256.00 | 2224.0 / 290.5 | 2444.0 / 876.5 | 46.701 / 48.856 |
| binaries | 222.46 | 2092.5 / 223.1 | 3715.1 / 560.8 | 73.692 / 55.529 |
| random | 256.00 | 1328.3 / 89.1 | 5293.7 / 5108.4 | 256.003 / 256.016 |
| small-files | 15.61 | 182.5 / 101.1 | 77.0 / 68.8 | 0.545 / 0.037 |

| Corpus | Compression peak RSS MiB: Zipper / 7-Zip | Extraction peak RSS MiB: Zipper / 7-Zip |
| --- | ---: | ---: |
| logs | 23.4 / 18.3 | 17.6 / 9.5 |
| binaries | 23.4 / 20.9 | 19.6 / 11.0 |
| random | 18.9 / 23.2 | 10.4 / 12.8 |
| small-files | 4.3 / 25.1 | 3.6 / 8.5 |

## Interpretation

Zipper is faster on these inputs at these presets. Logs compress 7.7× faster and extract 2.8× faster with a 4.4% smaller archive. Binaries compress 9.4× faster and extract 6.6× faster, at a 32.7% archive-size penalty. Random bytes compress 14.9× faster but neither engine meaningfully reduces size; extraction is effectively tied (1.04×). Small files compress 1.8× faster and extract 1.12× faster, but produce a 14.6× larger archive.

This does not establish leadership or a win at matched compression ratios. The codecs differ, levels are not equivalent, and Zipper benefits from independently decodable blocks. We have not compared against a tuned Zstandard archiver, LZ4, other 7-Zip presets, or a public benchmark corpus. The log and small-file data are synthetic. Small files deliberately share redundant text, exposing the cost of independent per-file compression versus solid compression.

## Method and limits

- `scripts/make_benchmark_corpus.py` generates deterministic logs, pseudorandom bytes, and 4,000 similar configuration files. It concatenates available regular files under `/usr/bin` for the binary sample; this machine provided 222.46 MiB. Exact binary source hashes are in `binary-sources.json`.
- `scripts/benchmark.py` runs Zipper first, then 7-Zip. It includes process startup and filesystem I/O but excludes hash verification from timing. No OS cache flush, thermal control, or randomized engine ordering; the initial hash pass warms input caches.
- Zipper syncs the archive before publishing; the benchmark does not force equivalent durability behavior on 7-Zip. Extraction timing does not guarantee durable writes. These are local application timings, not isolated codec or storage bandwidth measurements.
- Some operations last only 50–200 ms, so minor differences, especially random-data extraction, should be treated as noise. Raw samples are retained in each corpus JSON.
- Peak RSS is measured per child with `wait4`; table values are medians of run peaks. CPU utilization and energy are not measured.

## Reproduce

```sh
cargo build --release
python3 scripts/make_benchmark_corpus.py /tmp/zipper-corpus --mib 256
python3 scripts/benchmark.py /tmp/zipper-corpus/logs --runs 3 --threads 4 --sevenzip 7zz
# Repeat for binaries, random, and small-files.
```

## Priorities

1. Pack small files into shared independently decodable blocks to improve compression ratio and amortize file/task overhead.
2. Sweep compression levels and compare at matched archive sizes, including a Zstandard-based baseline.
3. Reuse codec contexts and overlap reading, codec work, and writing; current batches have synchronization barriers and each file is processed separately.
4. Expand to public and real-workload corpora with larger runs, interleaved order, and thread-scaling measurements.
