# Experiment log

Every performance experiment has one record in this directory, with its raw data beside it. This page lists them, newest first within each area. For the current headline numbers, read [Results](../docs/results.md).

All records are local measurements. Each one names its machine, revision, run count and verification.

## Native format

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-01 | [Level and block-size sweep on dependency sources](rust-dependencies/level-sweep.md) | Larger blocks cut archive bytes by up to 48%. Extraction ranges overlap. The default stays. |
| 2026-10-29 | [Silesia extraction time versus compression ratio](silesia/README.md) | Larger blocks can improve the ratio but cost memory and often extraction time. The default stays at level 1 with 1024 KiB blocks. |
| 2026-10-29 | [Matched-sync comparison with tar+zstd](v2/zstd-fullsync.md) | With equal file sync, Zipper creates random data at 1479 MiB/s versus 1560–1565 MiB/s for tar. The earlier 3.5-fold gap came from unequal sync. |
| 2026-10-29 | [Indexed v2 measurements](v2/README.md) | V2 improves large-file extraction and small-file density. Compression speed is mixed. |
| 2026-10-29 | [First baseline against 7-Zip](7zip-baseline.md) | Faster than 7-Zip LZMA2 level 1 at unmatched ratios. It set the v2 priorities. |

## ZIP reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-10-30 | [Decoder selection for large compressible entries](native-zip/decoder-selection/README.md) | Kept. Streaming these entries is faster on most patterns and uses under 4 MiB instead of about 19 MiB. |
| 2026-10-30 | [Lazy streaming scratch](native-zip/lazy-buffer/README.md) | Rejected for speed. It saves about 0.7–1 MiB of peak memory on tiny files. |
| 2026-10-30 | [Direct output without BufWriter](native-zip/direct-output/README.md) | Rejected. No general extraction gain. |
| 2026-10-30 | [Buffered checksums with libdeflate CRC](native-zip/crc-choice/README.md) | Rejected. No established gain. |
| 2026-10-30 | [Adaptive read-ahead](native-zip/adaptive-input/README.md) | Kept on Windows only, for large random streaming. |
| 2026-10-30 | [Windows alpha release versus current source](windows-release-comparison/README.md) | 7.43× faster on a compressible entry, slower on large streamed entries. Not a universal gain. |
| 2026-10-30 | [Bounded output writes on Windows](native-zip/windows-write-size/README.md) | Kept on Windows. Large entries extract 5.38× to 6.72× faster. |
| 2026-10-30 | [Preserved buffered window across seeks](native-zip/preserved-window/README.md) | Kept. Grouped Linux extraction is 8.7% faster. |
| 2026-10-30 | [Smaller input read-ahead](native-zip/input-buffer/README.md) | Kept. Linux streaming extraction is 2.06× to 4.63× faster with less memory. |
| 2026-10-30 | [File activity trace of tiny-file extraction](native-zip/file-activity/README.md) | Found 4.72 GB of logical reads for a 3.4 MB archive. Output creation is still the largest syscall cost. |
| 2026-10-30 | [Linux small-archive cache](native-zip/linux-cache/README.md) | Kept on Linux. 10,000-file extraction is 1.95× to 5.87× faster. |
| 2026-10-30 | [Shared entry-buffer budget](native-zip/shared-budget/README.md) | Kept. One large entry extracts 1.33× faster at a memory cost. |
| 2026-10-30 | [Threefold small-file extraction target](native-zip/threefold/README.md) | Not achieved. Integrity verification improved instead. |
| 2026-10-29 | [Buffered central-directory preflight](native-zip/preflight.md) | Kept. Listing is about 18% faster. Extraction ranges overlap. |
| 2026-10-29 | [Native ZIP baseline](native-zip/README.md) | About twice as fast as direct 7-Zip on three of four workloads. |

## Comparisons with other tools

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-10-30 | [Portable extraction matrix](extraction-matrix/README.md) | Found a Linux input bottleneck and a Windows large-entry regression. The ZIP reader records above address both. |
| 2026-10-30 | [Real dependency-source extraction](rust-dependencies/README.md) | Native-fast extracts the same ZIP 2.22× faster than bsdtar. The native archive is 14.5% smaller than the ZIP. |
| 2026-10-29 | [ZIP extraction versus bsdtar](native-zip/bsdtar-comparison.md) | About twice as fast as bsdtar on small files and 1.99× on Silesia. |
| 2026-10-29 | [Cross-platform Silesia checks](cross-platform/README.md) | Native ZIP beat direct 7-Zip on hosted macOS and Linux runners. |

Decisions made before these records existed are rows in [v2/decisions.tsv](v2/decisions.tsv). New decisions go in this log, not in that file.

## Add an experiment

1. Create `benchmarks/<area>/<experiment>/README.md` and put the raw data beside it.
2. Use exactly six level-2 headings, in this order. They are `## Question`, `## Setup`, `## Results`, `## Decision`, `## Limits` and `## Reproduce`. Put the verdict in one or two sentences under the title.
3. For a comparison with another tool, report the 1-thread Zipper row and CPU time next to the wall time. `scripts/extraction_matrix.py`, `scripts/compare_formats.py` and `scripts/compare_zstd.py` record both, pass reduced-metadata flags to tar, and write a `parity` section. State in Limits any difference that remains, such as 7-Zip restoring timestamps.
4. Add a row to the matching table on this page.
5. Run `cargo xtask lint-docs`. It checks the headings, the links, and that every page is reachable from `README.md`.
