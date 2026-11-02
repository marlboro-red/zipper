# Writer pool for tar extraction

**Kept. With one decoding thread and four writer threads, tar extraction is 3.0× to 5.6× faster on small-file workloads and 1.11× to 1.24× faster on large gzip entries.** Every row beats its range. The machine was running a game under Wine throughout, which made file creation about five times slower than in quiet sessions. So the absolute times are not comparable with other records, though the interleaved ordering is.

## Question

The first tar reader decoded, parsed and wrote on one thread. A three-round trial showed it losing to default bsdtar on three `.tar.gz` workloads, Silesia, the large compressible entry and the large random entry. Each lost by 1.07× to 1.34×. Plain tar was level with bsdtar there. The hypothesis was that writing on separate threads would hide the write cost behind decoding, and that parallel creation would speed up small files as it does for ZIP. The metric is the wall median against the single-threaded reader on the same archive, with both ranges.

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. Release builds.
- `before` is the reader at `8dbf02d`, binary SHA-256 prefix `87f6bfc75adf`. `w1` and `w4` are the change with one and four writers, prefix `8f5a995e8f3f`.
- One thread decodes and parses. Each file goes to a writer as one job, and a file larger than 1 MiB streams its chunks to one writer in order. At most 16 MiB of file data waits between them.
- The fixtures come from `write_tar` and `write_gzip` in `scripts/extraction_matrix.py`. They are ustar archives with fixed metadata, and their gzip level 6 forms.
- `scripts/compare_zip_readers.py --archive-format tar` ran each archive with the default engine, 7 interleaved rounds after one warmup. Every round passed the byte and tree check.
- The load average was 3.5 to 4.2 throughout, from a game running under Wine.

## Results

Wall medians in milliseconds with the range, then median CPU milliseconds and peak MiB. Raw samples are in this folder.

| Archive | Bytes | `before` | `w1` | `w4` |
| --- | ---: | ---: | ---: | ---: |
| 10,000 flat tiny files, gzip | 1,600,877 | 2,682.4 [2,629.5–2,719.9], CPU 2,569, 5.6 | 2,496.4 [2,383.9–2,811.8], CPU 2,636, 5.7 | 476.9 [464.0–623.2], CPU 1,587, 5.9 |
| 10,000 flat tiny files, plain | 16,394,240 | 2,537.5 [2,498.9–2,684.5], CPU 2,468, 5.5 | 2,511.7 [2,463.3–2,621.3], CPU 2,531, 5.6 | 515.2 [469.4–582.3], CPU 1,627, 5.8 |
| 4,000 grouped files, gzip | 109,846 | 1,123.9 [1,007.5–1,153.7], CPU 1,097, 4.0 | 1,053.1 [977.9–1,253.0], CPU 1,139, 4.1 | 377.5 [183.4–503.9], CPU 1,210, 4.2 |
| Rust dependency sources, gzip | 12,452,225 | 1,568.2 [1,451.6–1,697.5], CPU 1,595, 6.7 | 1,268.3 [1,097.9–1,332.0], CPU 1,633, 10.4 | 462.4 [364.4–641.3], CPU 1,330, 9.5 |
| Rust dependency sources, plain | 71,127,040 | 1,383.5 [1,301.0–1,460.4], CPU 1,420, 6.3 | 1,251.1 [1,175.3–1,395.3], CPU 1,472, 10.8 | 338.5 [277.5–481.4], CPU 1,104, 9.5 |
| Silesia, gzip | 68,228,613 | 912.0 [893.2–933.5], CPU 899, 4.6 | 829.8 [795.9–839.2], CPU 916, 10.6 | 824.8 [814.9–842.4], CPU 920, 10.8 |
| 256 MiB random entry, gzip | 268,517,566 | 411.7 [398.5–436.6], CPU 402, 4.6 | 339.7 [289.1–359.3], CPU 469, 9.7 | 344.8 [334.0–363.9], CPU 467, 9.8 |
| 256 MiB compressible entry, gzip | 652,142 | 432.1 [374.2–445.9], CPU 421, 4.2 | 339.5 [319.8–349.2], CPU 448, 9.3 | 347.1 [303.9–351.6], CPU 452, 13.5 |

A first pass of the same comparison, before the 16 MiB budget, also ran two and eight writers. Eight writers were within the four-writer ranges on every archive, so four stays the default.

## Decision

Kept, with four writers by default. On small files the four-writer medians are 3.0× to 5.6× below the single-threaded reader, far outside both ranges. On large entries one writer already overlaps writing with decoding. With four writers, Silesia drops by 1.11× and the large entries by 1.19× to 1.24×. Their ranges do not overlap the baseline. Peak memory rises by at most 9.3 MiB.

An adversarial review then found that queued chunks could reach 389 MB at 64 writers on a slow disk. The change caps file data in flight at 16 MiB. On the review's 2 GB archive of 5 MiB files, peak resident memory is 20 to 25 MB at 1, 4 and 64 writers.

## Limits

One Mac, one session, under constant load from a game running under Wine. File creation was about five times slower than in the scoreboard trial an hour earlier, so the small-file ratios may differ on a quiet machine. Grouped files ran with 10% more CPU at four writers. Linux and Windows were not measured here; the scoreboard on their runners will show the tar rows. bsdtar was not in this comparison.

## Reproduce

```sh
python3 - <<'EOF'
import sys; sys.path.insert(0, 'scripts')
from pathlib import Path
from extraction_matrix import write_tar, write_gzip
source = Path('target/bench-corpora/silesia/silesia').resolve()
write_tar(source, Path('silesia.tar'))
write_gzip(Path('silesia.tar'), Path('silesia.tar.gz'))
EOF
python3 scripts/compare_zip_readers.py silesia.tar.gz target/bench-corpora/silesia/silesia \
  --archive-format tar --runs 7 --threads 4 \
  --reader before native zipper-before --reader w1 native zipper-after \
  --reader w4 native zipper-after --reader-threads w1 1 > silesia.tar.gz.json
```

Build `zipper-before` at `8dbf02d` and `zipper-after` at this commit.
