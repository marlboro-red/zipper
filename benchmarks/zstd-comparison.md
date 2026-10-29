# Same-codec application comparison — 2026-10-29

Zipper engine revision `7efc10b` (engine unchanged from `6acd322`); Apple M5, 32 GiB RAM, macOS 26.7.1. Zstandard and PZstandard from local Zstandard 1.5.7 installation. Five measured runs after one warmup round, seeded randomized engine ordering within each corpus/round, four requested codec workers, level 1. Every restored file and directory manifest was verified. Raw results: [zstd-comparison.json](zstd-comparison.json).

| Corpus | Engine | Compression MiB/s | Extraction MiB/s | Archive MiB |
| --- | --- | ---: | ---: | ---: |
| logs | zipper | 2394.7 | 4520.2 | 46.701 |
| logs | tar+zstd | 2897.9 | 1794.1 | 46.787 |
| logs | tar+pzstd | 2972.6 | 2007.6 | 46.769 |
| binaries | zipper | 2119.5 | 3695.3 | 73.692 |
| binaries | tar+zstd | 2647.9 | 1600.9 | 72.809 |
| binaries | tar+pzstd | 2681.0 | 1989.4 | 73.109 |
| random | zipper | 1027.6 | 5174.1 | 256.003 |
| random | tar+zstd | 4109.9 | 2102.2 | 256.006 |
| random | tar+pzstd | 4201.0 | 2084.1 | 256.009 |
| small-files | zipper | 187.5 | 81.7 | 0.545 |
| small-files | tar+zstd | 14.1 | 15.1 | 0.107 |
| small-files | tar+pzstd | 14.0 | 15.2 | 0.113 |

## Interpretation

Unlike the earlier comparison with an LZMA2 preset, this is a comparison of containers and application pipelines using the same codec family. Zipper does not have a universal creation-speed lead. Its current per-file encoding also makes the small-file archive much larger than the tar alternatives. Zipper does extract faster in these local application tests. That is not proof that its Zstandard decoder is faster: filesystem behavior, tar processing, pipes, buffering, frame boundaries, and metadata work differ.

## Method and limitations

- Complete tar creation is piped into zstd or pzstd; extraction pipes the decoder into tar. No prebuilt tar input is excluded from the creation cost.
- Creation includes archive-file `fsync` for all engines. This aligns an important durability step but is not a guarantee of identical crash durability: publication/rename/directory persistence still differ. Extraction is not explicitly synced by this harness.
- Tar preserves metadata that the current Zipper format does not. Manifest verification checks file content and directory membership, not metadata equality. Metadata-heavy workloads are not feature-equivalent comparisons.
- Four requested codec workers are not an equal total-thread budget: the pipelines have I/O and tar work in other processes. Ordinary zstd and pzstd also differ in decoding parallelism. No CPU-time or process-tree RSS accounting was added here.
- Zipper uses 1 MiB independent frames; the other tools retain their default chunk/window choices. Streaming into zstd does not provide the original input size. This compares actual application defaults, not identical codec parameters.
- Input hashes warm the cache. Cache flushing, storage isolation, and thermal controls were not performed. Corpora and generation are described in the original baseline. Several operations are short; raw results should be used to assess variation.
- Small-file tar extraction may be dominated by metadata/system-call costs. Measure an equal-metadata mode and in-memory decoding before attributing that gap to the container design.

## Reproduce

```sh
cargo build --release
python3 scripts/make_benchmark_corpus.py /tmp/zipper-corpus --mib 256
python3 scripts/compare_zstd.py /tmp/zipper-corpus --runs 5 --threads 4
```

Requires `tar`, `zstd`, and `pzstd` on PATH. The generator writes local executable source hashes because the binary corpus varies with the operating-system installation.
