# Experiment log

Every performance experiment has one record in this directory, with its raw data beside it. This page lists them, newest first within each area. For the current headline numbers, read [Results](../docs/results.md). `cargo xtask bench` generates that page from the JSON in `scoreboard/`.

All records are local measurements. Each one names its machine, revision, run count and verification.

## Native format

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-01 | [Level and block-size sweep on dependency sources](rust-dependencies/level-sweep.md) | Larger blocks cut archive bytes by up to 48%. Extraction ranges mostly overlap. The default stays. |
| 2026-10-29 | [Silesia extraction time versus compression ratio](silesia/README.md) | The default stays at level 1 with 1024 KiB blocks. The frontier joins two sessions and two binaries. |
| 2026-10-29 | [Matched-sync comparison with tar+zstd](v2/zstd-fullsync.md) | Matched sync removed the old 3.5× creation gap. The remaining random-data difference is not established. |
| 2026-10-29 | [Indexed v2 measurements](v2/README.md) | V2 improves large-file extraction and small-file density. Compression speed is mixed. |
| 2026-10-29 | [First baseline against 7-Zip](7zip-baseline.md) | Faster than 7-Zip LZMA2 level 1 at unmatched ratios. It set the v2 priorities. |

## Tar reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-02 | [Decoding tar data straight into writer chunks](native-tar/direct-chunks/README.md) | Kept. The 256 MiB random gzip entry extracts 1.11× faster and the compressible one 1.06× faster, with one fewer copy. |
| 2026-11-02 | [Writer pool for tar extraction](native-tar/writer-pool/README.md) | Kept. Four writer threads make small-file tar extraction 3.0× to 5.6× faster and large gzip entries 1.11× to 1.24× faster. Measured under load from a game. |

## ZIP reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-02 | [Larger streaming steps](native-zip/stream-steps/README.md) | Kept. The 256 MiB random entry extracts 1.30× faster at one thread. Other large rows improved in median but overlap their ranges. |
| 2026-11-02 | [Single-thread extraction on the calling thread](native-zip/inline-single-thread/README.md) | Rejected. Skipping the rayon pool at one thread moved no workload beyond its range. |
| 2026-11-02 | [System zlib for streamed Deflate on macOS](native-zip/system-zlib/README.md) | Kept on macOS. Silesia extracts 1.23× faster at one thread and 1.18× at four, with 22% less CPU at four. Linux and Windows unchanged. |
| 2026-11-02 | [Write-behind for large entries](native-zip/write-behind/README.md) | Kept. With a spare thread, a 256 MiB entry extracts 1.62× faster when random and 1.34× when compressible, with CPU within 6% and 5 MiB more peak memory. |
| 2026-11-02 | [Small-file creation cost on macOS](native-zip/create-cost/README.md) | Native extraction runs 5% to 6.2% above the system-call floor. 3× over default bsdtar is below that floor on macOS. |
| 2026-11-01 | [Timestamp restore](native-zip/timestamps/README.md) | Kept. Times match `unzip` and bsdtar. One call per file costs about 3% to 11%. Measured on a busy machine. |
| 2026-11-01 | [Permission restore](native-zip/permissions/README.md) | Kept. Modes match `unzip` and bsdtar. Skipping directories already at the default mode removed a 10% cost on 996 directories. |
| 2026-11-01 | [Unicode path field check](native-zip/unicode-path/README.md) | Kept. Entries cannot be renamed by a Unicode path field. No listing cost. Windows-era archives that use the field go to the backend. |
| 2026-11-01 | [Exact Deflate decoding](native-zip/exact-decoding/README.md) | Kept. Closes two hostile-archive gaps. Large streamed entries extract 1.2× to 2.2× faster because input is read 64 KiB at a time. |
| 2026-11-01 | [Record layout check](native-zip/record-layout/README.md) | Kept. Closes five more hostile-archive gaps with no measured cost. Rejects self-extracting archives. |
| 2026-11-01 | [Local-header agreement check](native-zip/local-header-check/README.md) | Kept. Closes three hostile-archive gaps with no measured time cost and about 0.4 MiB at 10,000 entries. |
| 2026-10-30 | [Decoder selection for large compressible entries](native-zip/decoder-selection/README.md) | Kept. Streaming these entries is faster on most patterns and uses under 4 MiB instead of about 19 MiB. |
| 2026-10-30 | [Lazy streaming scratch](native-zip/lazy-buffer/README.md) | Rejected for speed. It saves about 0.7–1 MiB of peak memory on tiny files. |
| 2026-10-30 | [Direct output without BufWriter](native-zip/direct-output/README.md) | Rejected. No general extraction gain. |
| 2026-10-30 | [Buffered checksums with libdeflate CRC](native-zip/crc-choice/README.md) | Rejected. No established gain. |
| 2026-10-30 | [Adaptive read-ahead](native-zip/adaptive-input/README.md) | Kept on Windows on one separated result of ten, which only returns to alpha speed. Some Linux raw samples are missing. |
| 2026-10-30 | [Windows alpha release versus current source](windows-release-comparison/README.md) | Native-fast is 7.43× faster than the alpha's native-fast on one entry, but streaming in either build is faster. Large random streaming regressed. |
| 2026-10-30 | [Bounded output writes on Windows](native-zip/windows-write-size/README.md) | Kept on Windows. It fixes a native-fast write regression, 5.38× to 6.72× over the old path. Streaming stays faster on the compressible entry. |
| 2026-10-30 | [Preserved buffered window across seeks](native-zip/preserved-window/README.md) | Kept. Grouped Linux extraction is 8.7% faster. |
| 2026-10-30 | [Smaller input read-ahead](native-zip/input-buffer/README.md) | Kept. Linux streaming extraction is 2.06× to 4.63× faster with less memory. |
| 2026-10-30 | [File activity trace of tiny-file extraction](native-zip/file-activity/README.md) | Found 4.72 GB of logical reads for a 3.4 MB archive. Output creation is still the largest syscall cost. |
| 2026-10-30 | [Linux small-archive cache](native-zip/linux-cache/README.md) | Kept on Linux. 10,000-file extraction is 1.95× to 5.87× faster. |
| 2026-10-30 | [Shared entry-buffer budget](native-zip/shared-budget/README.md) | Kept. 1.33× on one 61 MiB entry at 31× the peak RSS. By default this applies only on ARM64 macOS. |
| 2026-10-30 | [Threefold small-file extraction target](native-zip/threefold/README.md) | Not achieved. Integrity verification improved instead. |
| 2026-10-29 | [Buffered central-directory preflight](native-zip/preflight.md) | Kept. Listing is about 18% faster. Extraction ranges overlap. |
| 2026-10-29 | [Native ZIP baseline](native-zip/README.md) | About twice as fast as direct 7-Zip with four native workers. No one-worker or CPU data. |

## Comparisons with other tools

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-02 | [Tar rows on Linux and Windows runners](cross-platform/tar-scoreboard/README.md) | Plain tar beats default bsdtar on every Linux workload and five of seven Windows ones. The gzip rows lose on the large entries, where the decoder is zlib-rs. |
| 2026-11-02 | [Windows worker cap for crowded folders](cross-platform/windows-folder-cap/README.md) | Kept. The Windows default uses one worker only when one folder holds 1,024 tiny files, and now beats bsdtar on every workload. |
| 2026-11-02 | [Scoreboard on Linux and Windows runners](cross-platform/scoreboard-2026-11-02/README.md) | Native at one thread beats bsdtar on every workload on both. At four threads, Windows loses on flat tiny files because parallel creation is slower there. |
| 2026-10-30 | [Portable extraction matrix](extraction-matrix/README.md) | Found a Linux input bottleneck and a Windows large-entry regression. The ZIP reader records above address both. |
| 2026-10-30 | [Real dependency-source extraction](rust-dependencies/README.md) | At one thread, native-fast is 1.33× faster than bsdtar with 25% less CPU. The native archive is 14.5% smaller than the ZIP. |
| 2026-10-29 | [ZIP extraction versus bsdtar](native-zip/bsdtar-comparison.md) | Four Zipper workers against one bsdtar process. One worker narrows small files to about 1.13×. Silesia is 1.99× with native-fast or 1.59× streaming. |
| 2026-10-29 | [Cross-platform Silesia checks](cross-platform/README.md) | Native ZIP with two workers beat direct 7-Zip on hosted runners. Wall time only. |

Decisions made before these records existed are rows in [v2/decisions.tsv](v2/decisions.tsv). New decisions go in this log, not in that file.

## Add an experiment

1. Create `benchmarks/<area>/<experiment>/README.md` and put the raw data beside it.
2. Use exactly six level-2 headings, in this order. They are `## Question`, `## Setup`, `## Results`, `## Decision`, `## Limits` and `## Reproduce`. Put the verdict in one or two sentences under the title.
3. For a comparison with another tool, report the 1-thread Zipper row and CPU time next to the wall time. `scripts/extraction_matrix.py`, `scripts/compare_formats.py` and `scripts/compare_zstd.py` record both and write a `parity` section. ZIP rows compare the native engines with default bsdtar, because both restore modes, times and links. Native-format rows pass reduced-metadata flags to tar. State in Limits any difference that remains, such as 7-Zip restoring timestamps.
4. Add a row to the matching table on this page.
5. Run `cargo xtask lint-docs`. It checks the headings, the links, and that every page is reachable from `README.md`.
