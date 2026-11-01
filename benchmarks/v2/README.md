# Indexed v2 measurements on 2026-10-29

V2 improves large-file extraction and small-file compression density on this local corpus. Compression speed remains mixed, and these results do not establish state-of-the-art leadership.

## Question

Does the indexed v2 format improve on v1, and do two follow-on experiments help v2? The first experiment deduplicates directory preparation. The second avoids buffer initialization.

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1.
- Release builds, Zstandard level 1, four requested codec workers.
- One warmup and seven measured rounds per corpus. Engine order is seeded and shuffled within each round.
- Every extraction is checked directly against the source bytes, file sizes, and directory membership. Verification is outside the timed interval. Zero verification failures occurred.
- Timings include startup and filesystem I/O. Extraction is not explicitly synced. Requested codec workers are not the total thread count.

The fixtures are 256 MiB JSON logs, 222.46 MiB of local executables, 256 MiB of deterministic pseudorandom bytes, and 4,000 similar configuration files totaling 15.61 MiB. Local executable contents depend on the OS installation. The generator records their source hashes.

The [raw samples](revisions.json) compare v1 (`a28964d` engine, unchanged through `eae9406`) with initial v2 (`d483303`). The third engine, `v2-reuse`, is a rejected buffer-initialization experiment. It is not the shipped implementation.

The directory experiment tests this hypothesis. Repeated `create_dir_all` calls for files sharing a parent waste filesystem work. The change collects distinct directory paths before creating them, then creates empty files. Payload decoding and writing remain unchanged.

The [corrected application comparison](zstd-fullsync.md) matches archive-file sync operations across engines. The earlier tar comparisons used a weaker sync operation on macOS, overstating the incompressible-data creation gap.

## Results

### V1 versus initial v2

| Corpus | Version | Compression MiB/s | Extraction MiB/s | Archive bytes |
| --- | --- | ---: | ---: | ---: |
| logs | v1 | 2264 | 4551 | 48,969,037 |
| logs | v2 | 2064 | 6798 | 48,967,453 |
| binaries | v1 | 1853 | 3783 | 77,271,177 |
| binaries | v2 | 1830 | 5633 | 77,270,195 |
| random | v1 | 1484 | 5213 | 268,438,833 |
| random | v2 | 1099 | 5188 | 268,436,635 |
| small-files | v1 | 189 | 83 | 571,915 |
| small-files | v2 | 202 | 84 | 35,013 |

Throughput uses median elapsed time. Logs and binaries extract about 49% faster, and their extraction ranges do not overlap:

- Logs went from 55.4–56.9 ms to 35.6–38.2 ms.
- Binaries went from 58.2–60.2 ms to 37.3–41.6 ms.

Small-file packing reduces archive size by 93.9% (16.3 times smaller). Initial v2 barely changes small-file extraction speed.

The random-data compression median regresses, with substantial timing variation. The v1 range is 161.5–250.4 ms, and the v2 range is 161.9–268.5 ms. Do not dismiss this as a demonstrated win or claim a confirmed cause.

Peak process RSS also increases on some workloads. Binary compression rises from 22.6 to 28.5 MiB. Small-file extraction rises from 3.6 to 16.0 MiB. The indexed manifest and overlapping pipeline trade memory for other capabilities.

### Directory preparation

The [seven-run comparison](directories.json) compares initial v2 with deduplicated directory preparation.

| Small-file extraction | Initial v2 | Deduplicated directories |
| --- | ---: | ---: |
| Median | 181.85 ms | 173.22 ms |
| Range | 181.38–193.92 ms | 170.01–177.63 ms |
| Throughput | 85.83 MiB/s | 90.11 MiB/s |
| Peak RSS | 16.03 MiB | 16.06 MiB |
| Archive bytes | 35,013 | 35,013 |

### Buffer initialization and profiling

Avoiding buffer initialization did not clear the timing-noise gate. Its raw measurements remain in `revisions.json` for the decision trail.

[Before](before-sample.txt) and [after](after-sample.txt) macOS sampling profiles were collected separately from benchmark runs. Zstandard dominates active compression stacks, and zeroing is a small component. A separate [small-file extraction profile](small-files-sample.txt) shows file open, write, and close work dominating active stacks. That profile supports filesystem-focused experiments.

## Decision

- Keep directory deduplication. It gave a 4.75% lower median extraction time, with disjoint observed ranges. Selected extraction, empty files, nested directories, and full byte recovery remain covered by tests.
- Revert the buffer-initialization change, because it did not clear the timing-noise gate.

[Decisions](decisions.tsv) records kept and rejected experiments. The next priorities are longer incompressible-data measurements, public real-world corpora, sweeps of the level and size trade, and profiling before further pipeline changes.

## Limits

- The random-data regression needs larger, longer incompressible workloads.
- The directory fixture has many shared parents. The result does not predict a win for a tree with a unique parent per file. The temporary set grows with distinct directories and adds metadata memory on such trees.
- Sampling counts are not precise CPU-time attribution, and idle threads are included.
- These are development fixtures, not a diverse public compression corpus.
- No cache flushing, thermal control, or isolated storage was used.
- Native metadata support is limited. Byte equality does not establish permission or timestamp parity.
- The smaller per-corpus JSON files are preliminary three-run observations. The interleaved comparisons supersede them.

## Reproduce

```sh
python3 scripts/make_benchmark_corpus.py /tmp/zipper-corpus --mib 256
python3 scripts/compare_revisions.py /tmp/zipper-corpus \
  --binary before=/path/to/before/zipper \
  --binary after=/path/to/after/zipper --runs 7 --threads 4
```
