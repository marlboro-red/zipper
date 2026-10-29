# Indexed v2 measurements — 2026-10-29

V2 improves large-file extraction and small-file compression density on this
local corpus. Compression speed remains mixed; these results do not establish
state-of-the-art leadership.

The [corrected application comparison](zstd-fullsync.md) matches archive-file
sync operations across engines. The earlier tar comparisons used a weaker sync
operation on macOS, overstating the incompressible-data creation gap.

Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds, Zstandard level 1,
four requested codec workers, one warmup and seven measured rounds per corpus.
Engine order is seeded and shuffled within each round. Every extraction is
checked directly against the source bytes, file sizes, and directory membership.
Verification is outside the timed interval. Zero verification failures occurred.

## V1 versus initial v2

[Raw samples](revisions.json) compare v1 (`a28964d` engine, unchanged through
`eae9406`) with initial v2 (`d483303`). The third engine, `v2-reuse`, is a rejected
buffer-initialization experiment; it is not the shipped implementation.

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

Throughput uses median elapsed time. Logs and binaries extract about 49% faster.
Their extraction ranges do not overlap: logs 55.4–56.9 ms versus 35.6–38.2 ms;
binaries 58.2–60.2 ms versus 37.3–41.6 ms. Small-file packing reduces archive size
by 93.9% (16.3 times smaller), while initial v2 barely changes extraction speed.

The random-data compression median regresses, with substantial timing variation:
v1 161.5–250.4 ms, v2 161.9–268.5 ms. Do not dismiss this as a demonstrated win
or claim a confirmed cause. Larger, longer incompressible workloads are needed.
Peak process RSS also increases on some workloads: binary compression rises
from 22.6 to 28.5 MiB; small-file extraction rises from 3.6 to 16.0 MiB. The
indexed manifest and overlapping pipeline trade memory for other capabilities.

## Directory preparation experiment

Hypothesis: repeated `create_dir_all` calls for files sharing a parent waste
filesystem work. Collect distinct directory paths before creating them, then
create empty files. Payload decoding and writing remain unchanged.

[Seven-run comparison](directories.json), initial v2 versus this change:

| Small-file extraction | Initial v2 | Deduplicated directories |
| --- | ---: | ---: |
| Median | 181.85 ms | 173.22 ms |
| Range | 181.38–193.92 ms | 170.01–177.63 ms |
| Throughput | 85.83 MiB/s | 90.11 MiB/s |
| Peak RSS | 16.03 MiB | 16.06 MiB |
| Archive bytes | 35,013 | 35,013 |

Keep: 4.75% lower median extraction time, with disjoint observed ranges. This
fixture has many shared parents; the result does not predict a win for a tree
with a unique parent per file. The temporary set grows with distinct directories
and adds metadata memory on such trees. Selected extraction, empty files, nested
directories, and full byte recovery remain covered by tests.

## Rejected experiment and profiling

Avoiding buffer initialization did not clear the timing-noise gate; it was
reverted. Its raw measurements remain in `revisions.json` for the decision trail.
[Before](before-sample.txt) and [after](after-sample.txt) macOS sampling profiles
were collected separately from benchmark runs. Active compression stacks are
dominated by Zstandard; zeroing is a small component. A separate
[small-file extraction profile](small-files-sample.txt) shows file open/write/
close work dominating active stacks, supporting filesystem-focused experiments.
Sampling counts are not precise CPU-time attribution; idle threads are included.

## Reproduce and limits

```sh
python3 scripts/make_benchmark_corpus.py /tmp/zipper-corpus --mib 256
python3 scripts/compare_revisions.py /tmp/zipper-corpus \
  --binary before=/path/to/before/zipper \
  --binary after=/path/to/after/zipper --runs 7 --threads 4
```

The fixtures are 256 MiB JSON logs, 222.46 MiB of local executables, 256 MiB
deterministic pseudorandom bytes, and 4,000 similar configuration files totaling
15.61 MiB. Local executable contents depend on the OS installation. The generator
records their source hashes. These are development fixtures, not a diverse public
compression corpus. No cache flushing, thermal control, or isolated storage was
used. Timings include startup and filesystem I/O; extraction is not explicitly
synced. Requested codec workers are not the total thread count. Native metadata
support is limited; byte equality does not establish permission/timestamp parity.

The smaller per-corpus JSON files are preliminary three-run observations and
are superseded by the interleaved comparisons. [Decisions](decisions.tsv) record
kept and rejected experiments. Next priorities are longer incompressible-data
measurements, public real-world corpora, level/size tradeoff sweeps, and profiling
before further pipeline changes.
