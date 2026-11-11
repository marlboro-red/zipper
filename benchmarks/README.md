# Experiment log

Every performance experiment has one record in this directory, with its raw data beside it. This page lists them, newest first within each area. For the current headline numbers, read [Results](../docs/results.md). `cargo xtask bench` generates that page from the JSON in `scoreboard/`.

All records are local measurements. Each one names its machine, revision, run count and verification.

## Real-world archives

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-03 | [Real-world archives against bsdtar](realworld/README.md) | All 248 popular ZIP, wheel, jar and tar archives extract identically with Zipper and default bsdtar, across 189,576 paths. Zipper was faster on all 248, 10.51 s against 18.11 s in total. macOS only. |

## Zstandard decoder

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [Zipper's own Zstandard decoder against libzstd](zstd/own-decoder/README.md) | Kept. One-thread extraction of Silesia takes 0.97 to 0.98 times the cycles of libzstd 1.5.7 on an Apple M2 Max and 0.89 times on an M5, in safe Rust. The M2 Max took 12 to 14% more until the sequence loop stopped keeping its FSE states on the stack. A file of 3,983 small frames still takes 1.05 times libzstd's cycles there. |

## LZMA decoder

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [Doubling copy for long LZMA matches at a short distance](lzma/repeating-copy/README.md) | Kept. A 7z of 256 MiB of zeros extracts 7.3 times faster at one thread, 0.086 s against 0.630 s, and faster than 7-Zip. Silesia is unchanged. |
| 2026-11-10 | [Zipper's LZMA decoder against sevenz-rust2](lzma/sevenz-rust2/README.md) | Kept as a record. Zipper's decoder takes 0.79 times lzma-rust2's time on the probe stream, and Silesia 7z extracts 1.25 to 1.27 times faster than with sevenz-rust2 at one thread. lzma-rust2's assembly saves it 0.8%. |
| 2026-11-08 | [LZMA probability tables read without bounds checks](lzma/unchecked-tables/README.md) | Reverted. 3.1% fewer instructions, no commit faster than its parent, and 0.4% faster together, which does not justify `unsafe`. |
| 2026-11-08 | [LZMA output limit stopped at the window end](lzma/window-end-limit/README.md) | Reverted. 1.8% fewer instructions and no change in probe time, ranges overlapping. |
| 2026-11-08 | [LZMA kernel changes from a study of 7-Zip's decoder](lzma/7zip-study-kernel/README.md) | Kept two changes of three. 7z Silesia extracts about 3.5% faster. Holding the loop's state in locals was 19.5% slower and was reverted. 7-Zip is still about 11% faster at one thread. |
| 2026-11-07 | [LZMA bit trees with both children loaded early](lzma/tree-preload/README.md) | Kept four changes of five. Zipper's decoder takes 350.0 ms on the probe stream against 398.4 ms for liblzma, a ratio of 0.878. It builds only for tests, so no extraction is faster yet. |
| 2026-11-07 | [Parallel staging cleanup after an overwrite](overwrite/parallel-cleanup/README.md) | Reverted both. Emptying staging on Rayon left `--overwrite` wall time unchanged and raised CPU time by about half. One `lstat` fewer showed no difference. |
| 2026-11-07 | [Cost of undoing a failed overwrite](overwrite/rollback-cost/README.md) | Kept. The undo journal slows `--overwrite` on dependency sources from 0.710 to 0.851 s, ranges apart. Extraction into a new folder is unchanged. |
| 2026-11-06 | [7z LZMA and LZMA2 streams ended where 7-Zip ends them](native-7z/stricter-lzma-ends/README.md) | Kept. Refusing five hostile LZMA and LZMA2 ends costs no measurable time on five archives at one thread or default threads. Every range overlaps. |
| 2026-11-06 | [LZMA plain literals with both children loaded early](lzma/literal-preload/README.md) | Kept one change of three. The probe stream fell from 434.8 to 408.7 ms, and the decoder takes 1.03 times liblzma's time. Eight direct bits per normalisation was slower, and one `is_match` branch moved nothing. |
| 2026-11-06 | [LZMA decoder state kept in registers](lzma/kernel-state/README.md) | Kept one change. Zipper's decoder takes 1.09 times liblzma's time on the probe stream, against 1.29 times before, and 434.3 ms on an idle machine. An idle re-time rejected three more changes. No reader uses it yet. |
| 2026-11-05 | [LZMA decoder hot loop](lzma/hot-loop/README.md) | Kept nine changes. Zipper's own decoder is 1.56× faster than its first version and still 1.09× slower than liblzma 5.8.4. 7-Zip took about 310 ms against 437 ms. No reader uses it yet. |

## 7z reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [A writer pool for solid 7z folders of small files](native-7z/solid-writer-pool/README.md) | Kept. The solid dependency sources extract 1.60 times faster at default threads, 404 ms against 648 ms, on an M2 Max. One thread and Silesia are unchanged, and peak memory rises from 42 to 48 MiB. |
| 2026-11-11 | [BCJ2 main stream copied a slice at a time](native-7z/bcj2-slice-scan/README.md) | Kept. A BCJ2 7z of Silesia extracts 1.38 times faster at one thread, 1.614 s against 2.225 s, on an M2 Max. 7-Zip takes 1.311 s. Plain LZMA2 is unchanged. |
| 2026-11-11 | [Parallel LZMA2 segments behind 7z filters](native-7z/filtered-lzma2-segments/README.md) | Kept. An ARM64-filtered 7z extracts 1.29 times faster at four threads, 0.746 s against 0.963 s, with peak memory 37 to 141 MiB. Archives with one LZMA2 block are unchanged. |
| 2026-11-10 | [Buffered BCJ2 branch streams](native-7z/bcj2-buffered-branches/README.md) | Kept. A BCJ2 7z of Silesia extracts 2.25 times faster at one thread, 1.650 s against 3.715 s, with ranges apart. 7-Zip takes 1.022 s. Plain LZMA2 is unchanged. |
| 2026-11-10 | [Create each 7z output folder once](native-7z/create-folders-once/README.md) | Kept as a cleanup. The reader makes each folder once, not once per item, but every timing range on the dependency sources overlaps the old code's. |
| 2026-11-07 | [7z branch and Delta filters on Zipper's own code](native-7z/own-filters/README.md) | Kept. Extraction time is unchanged on BCJ, ARM64, Delta and three-filter archives at one and four threads, with every range overlapping. The 7z reader no longer uses liblzma outside tests. |
| 2026-11-08 | [7z write path with one CRC pass and no extra copy](native-7z/write-path/README.md) | Kept two changes of three. Solid Silesia extracts 1.25% faster at one thread. A window zeroed once raised memory 12 times on many small xz entries and was rejected. |
| 2026-11-08 | [Parallel LZMA2 segments in 146 MiB](native-7z/segment-memory/README.md) | Kept. Peak memory on solid Silesia at default threads fell from 320 to 146 MiB, below 7-Zip's 257 MiB, and extraction got 0.7% faster. |
| 2026-11-07 | [7z extraction switched to Zipper's own LZMA decoder](native-7z/own-decoder-shipped/README.md) | Kept. Silesia extracts 7 to 9% faster than with liblzma, and non-solid dependency sources 9 to 20% faster. 7-Zip still extracts one long LZMA stream about 8% faster at one thread. |
| 2026-11-06 | [7z extraction with Zipper's own LZMA decoder](native-7z/own-lzma-decoder/README.md) | Held. Silesia extracts 5 to 8% slower than with liblzma, and non-solid dependency sources 11 to 13% faster. The swap waits for a faster decoder. |
| 2026-11-03 | [Parallel LZMA2 segments in solid 7z folders](native-7z/lzma2-segments/README.md) | Kept. Solid Silesia extracts 1.59× faster, still 1.24× slower than 7-Zip, with peak memory up from 38 to 338 MiB. Other archives are unchanged. |

## Tar reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [Stored Deflate blocks copied straight to the caller](deflate/stored-direct-copy/README.md) | Kept. A `.tar.gz` of random bytes extracts level with the zlib build on an M2 Max, 1.01 to 1.03 times with ranges overlapping, where it took 1.15 times. Silesia `.tar.gz` is 0.98 times. |
| 2026-11-11 | [Zipper's own BZip2 decoder at one thread and in ZIP](bzip2/own-decoder-one-thread/README.md) | Kept. Silesia BZip2 extracts 1.66 times faster at one thread, faster than 7-Zip 26.06, and ZIP BZip2 1.47 times faster at default threads. Peak memory rises, and ZIP charges it to the decoder budget. |
| 2026-11-11 | [Every Deflate decode on Zipper's own decoder, with no zlib](deflate/no-zlib/README.md) | Kept. flate2, zlib-rs and the macOS system zlib leave the build. 7z Deflate extracts 1.07 times and `.tar.gz` at level 1 1.05 times faster on an M2 Max, and no row got slower. |
| 2026-11-11 | [BZip2 blocks decoded in parallel](bzip2/block-parallel/README.md) | Kept. Silesia `.tar.bz2` extracts 7.9 times faster and the BZip2 7z 5.0 times faster at default threads, 4.2 times faster than 7-Zip 26.06. One thread is unchanged, and peak memory rises to about 100 MiB. |
| 2026-11-08 | [lzip, `.lzma` and ZIP LZMA on Zipper's own decoder](xz/lzip-lzma-zip/README.md) | Kept. Silesia `.tar.lz` extracts 15 to 16% faster than with liblzma, `.lzma` 12% and ZIP LZMA 8 to 11%. The release build no longer links liblzma. |
| 2026-11-07 | [xz on Zipper's own decoder](xz/own-decoder/README.md) | Kept. Silesia `.tar.xz` extracts 11 to 15% faster than with liblzma, and dependency sources 5 to 11% faster. 7z LZMA2 extraction, which shares the changed reader, is unchanged. |
| 2026-11-02 | [Decoding tar data straight into writer chunks](native-tar/direct-chunks/README.md) | Kept. The 256 MiB random gzip entry extracts 1.11× faster and the compressible one 1.06× faster, with one fewer copy. |
| 2026-11-02 | [Writer pool for tar extraction](native-tar/writer-pool/README.md) | Kept. Four writer threads make small-file tar extraction 3.0× to 5.6× faster and large gzip entries 1.11× to 1.24× faster. Measured under load from a game. |

## ZIP reader

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [ZIP folders made in parallel before the workers start](native-zip/parallel-folders/README.md) | Reverted. On APFS making folders does not get faster with more threads, and the setup moved from 53.8 to 46.4 ms at four threads with ranges overlapping. Three folder-spelling tests are kept. |
| 2026-11-11 | [ZIP workers read the archive without a lock](native-zip/lock-free-reads/README.md) | Kept. 512 MiB of stored entries extract 1.18 times faster at 64 threads, and other rows are unchanged. The serial folder setup is about 20% of four-thread extraction on the dependency sources. |
| 2026-11-11 | [Zipper's own Deflate decoder](native-zip/own-inflate/README.md) | Kept as the ZIP Deflate decoder, in safe Rust. It takes 629.7 M cycles on Silesia against 681.3 for Apple's zlib, and a large compressible entry extracts 1.24 times faster in wall time on an M2 Max. Small-file ZIPs are unchanged. |
| 2026-11-11 | [A macOS worker cap for ZIPs of mostly small files](native-zip/small-files-cap/README.md) | Kept. Four workers when files average at most 64 KiB make the dependency sources 11% and the mixed tree 10% faster at default threads, with CPU about halved. Silesia is unchanged. |
| 2026-11-10 | [One Deflate64 decoder per ZIP entry](native-zip/deflate64-one-inflater/README.md) | Kept. A Deflate64 ZIP of Silesia extracts 1.83 times faster at one thread, 0.412 s against 0.755 s, and faster than 7-Zip at 0.530 s. Every damaged entry gets the same verdict. |
| 2026-11-08 | [ZIP listing with Zipper's own directory reader](native-zip/own-directory-listing/README.md) | Kept. Listing takes the same time as with the `zip` crate on six archives, every range overlapping, and lists a 100,000-entry archive the old path refused. |
| 2026-11-05 | [Patience for ZIP decoder memory](native-zip/decoder-memory-patience/README.md) | Kept a 1 s wait before an entry is recorded for another pass. A PPMd archive ran 9.50 s against 9.36 s at `80af2b5`, overlapping. Without the wait it took 1.3 times as long. |
| 2026-11-05 | [One entry per Rayon task](native-zip/one-entry-per-task/README.md) | Kept. Two Rust build folders extract 1.40× to 1.76× faster at four threads on Windows. Tiny-file workloads are unchanged. One machine only. |
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
| 2026-11-11 | [Zipper against the Go ZIP extractors pzip and fastzip](native-zip/go-zip-tools/README.md) | Zipper extracts Silesia ZIP about 2.5 times faster at four threads. On small files `--content-only` beats both by 10 to 12%, and default Zipper ties pzip while checking far more. |
| 2026-11-03 | [One tar writer per folder on Windows](cross-platform/tar-folder-lanes/README.md) | Kept on Windows. 10,000 flat tiny files extract 1.44× to 1.56× faster from tar, with ranges apart. Linux, where the change is off, stayed within 3%. |
| 2026-11-03 | [Run-time CPU detection in zlib-rs](cross-platform/zlib-rs-detection/README.md) | Kept. Silesia and the random entry extract 1.38× to 1.84× faster as `.tar.gz` on Linux and Windows runners, and Silesia ZIP 1.23× to 1.26×. A synthetic ZIP entry with 25-byte matches is 1.33× to 1.36× slower but still beats bsdtar. |
| 2026-11-02 | [Tar rows on Linux and Windows runners](cross-platform/tar-scoreboard/README.md) | Plain tar beats default bsdtar on every Linux workload and five of seven Windows ones. The gzip rows lose on the large entries, where the decoder is zlib-rs. |
| 2026-11-02 | [Windows worker cap for crowded folders](cross-platform/windows-folder-cap/README.md) | Kept. The Windows default uses one worker only when one folder holds 1,024 tiny files, and now beats bsdtar on every workload. |
| 2026-11-02 | [Scoreboard on Linux and Windows runners](cross-platform/scoreboard-2026-11-02/README.md) | Native at one thread beats bsdtar on every workload on both. At four threads, Windows loses on flat tiny files because parallel creation is slower there. |
| 2026-10-30 | [Portable extraction matrix](extraction-matrix/README.md) | Found a Linux input bottleneck and a Windows large-entry regression. The ZIP reader records above address both. |
| 2026-10-30 | [Real dependency-source extraction](rust-dependencies/README.md) | At one thread, native-fast is 1.33× faster than bsdtar with 25% less CPU. The native archive is 14.5% smaller than the ZIP. |
| 2026-10-29 | [ZIP extraction versus bsdtar](native-zip/bsdtar-comparison.md) | Four Zipper workers against one bsdtar process. One worker narrows small files to about 1.13×. Silesia is 1.99× with native-fast or 1.59× streaming. |
| 2026-10-29 | [Cross-platform Silesia checks](cross-platform/README.md) | Native ZIP with two workers beat direct 7-Zip on hosted runners. Wall time only. |


## Build settings

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-10 | [Rust 1.99.1 in place of 1.96.0](toolchain/rust-1.99.1/README.md) | Kept. Twelve Silesia workloads extract as fast as with 1.96.0, with ratios of 0.996 to 1.052 and every range overlapping. |
| 2026-11-10 | [Linux x86-64 rerun of the sevenz-rust2, PGO and Rust 1.99.1 timings](cross-platform/linux-x86-oneoff/README.md) | No change. On one hosted runner, Zipper beat sevenz-rust2 on every 7z workload, PGO helped only BZip2, and Rust 1.99.1 was 2.5% to 3.2% slower on gzip tar and one-thread ZIP Deflate. |
| 2026-11-10 | [Profile-guided optimization of the release binary](pgo/lzma-and-readers/README.md) | Reverted. No LZMA workload got faster, even with a profile of the measured archives. Only BZip2 tar gained, by 2.1%, and ten other workloads overlapped. |

## Milestone summaries

| Date | Experiment | Verdict |
| --- | --- | --- |
| 2026-11-11 | [Before and after the research report's changes](research-report/before-after/README.md) | Every targeted workload is faster, from 1.10 times on small-file ZIPs to 8.33 times on zero runs, and nothing else moved. Zipper now beats 7-Zip 26.06 on every ZIP row, solid small-file 7z and zero runs, and trails on plain LZMA 7z by 1.08 to 1.10 times. |

## Add an experiment

1. Create `benchmarks/<area>/<experiment>/README.md` and put the raw data beside it.
2. Use exactly six level-2 headings, in this order. They are `## Question`, `## Setup`, `## Results`, `## Decision`, `## Limits` and `## Reproduce`. Put the verdict in one or two sentences under the title.
3. For a comparison with another tool, report the 1-thread Zipper row and CPU time next to the wall time. `scripts/extraction_matrix.py` records both and writes a `parity` section. ZIP rows compare the native engines with default bsdtar, because both restore modes, times and links. State in Limits any difference that remains, such as 7-Zip restoring timestamps.
4. Add a row to the matching table on this page.
5. Run `cargo xtask lint-docs`. It checks the headings, the links, and that every page is reachable from `README.md`.
