# Roadmap

The first phase ended with M9. Zipper's in-process readers replaced 7-Zip as the default for ZIP, tar and 7z. They are correct on hostile input, restore the metadata users expect, and extract faster with bounded memory. Speed work starts on macOS. CI runs every correctness check on Linux for each push. The Windows workflow runs them on Windows when invoked, before a release.

The second phase, from M10, set out to extract everything 7-Zip extracts without 7-Zip installed, with LZMA decoded by its own code. On 12 November 2026 the user decided that parity with 7-Zip is no longer a goal, and Zipper now refuses the formats whose readers it removed for licence reasons, as [M32](roadmap/m32-every-reader-under-zippers-licence.md) records. Each new reader keeps the first phase's contract. It refuses unsafe paths and links, publishes nothing on failure, bounds its memory and passes the `zipper-review` skill. A faster reader never drops one of these checks.

Every milestone from M10 to M21 has one more exit check, beside its own. New parsing code has a Kani proof, or an exhaustive test where Kani does not finish. New concurrency has a TLA+ model with its own `zipper-verify-*` skill. Each proof, test and model is listed in `verification/coverage.txt`, so the `verify` gate step catches drift. A planted break, made with `scripts/plant.py`, shows each one can fail.

Each milestone closes only when its exit checks pass. A check is a gate step, a test or an experiment record, never an opinion. [The design page](design.md#implementation-proceeds-through-five-ordered-gates) describes how extraction experiments proceed.

## Milestones

| Milestone | Status | Depends on |
| --- | --- | --- |
| [M0. Verified workflow](roadmap/m0-verified-workflow.md) | Done | None |
| [M1. Correctness contract for ZIP](roadmap/m1-correctness-contract-for-zip.md) | Done | M0 |
| [M2. Metadata fidelity](roadmap/m2-metadata-fidelity.md) | Done | M1 |
| [M3. Fixed scoreboard](roadmap/m3-fixed-scoreboard.md) | Done | M0 |
| [M4. Small-file output path](roadmap/m4-small-file-output-path.md) | Done | M3 |
| [M5. Remaining extraction headroom](roadmap/m5-remaining-extraction-headroom.md) | Done | M3 |
| [M6. Native reader as the default](roadmap/m6-native-reader-as-the-default.md) | Done | M1, M2 |
| [M7. Native tar family](roadmap/m7-native-tar-family.md) | Done | M5 |
| [M8. Extraction API for applications](roadmap/m8-extraction-api-for-applications.md) | Done | M7 |
| [M9. Native 7z reader](roadmap/m9-native-7z-reader.md) | Done | M8 |
| [M10. Parity matrix](roadmap/m10-parity-matrix.md) | Done | M9 |
| [M11. Windows metadata parity](roadmap/m11-windows-metadata-parity.md) | Done | M10 |
| [M12. Extraction controls](roadmap/m12-extraction-controls.md) | Done | M10 |
| [M13. Remaining codecs](roadmap/m13-remaining-codecs.md) | Done | M10 |
| [M21. Zipper's own LZMA decoder](roadmap/m21-zippers-own-lzma-decoder.md) | Done | M10 |
| [M14. Encrypted archives](roadmap/m14-encrypted-archives.md) | Done | M13 |
| [M15. Split, spanned and self-extracting archives](roadmap/m15-split-spanned-and-self-extracting-archives.md) | Done | M13 |
| [M16. Package formats](roadmap/m16-package-formats.md) | Done | M13 |
| [M17. Disc and disk images](roadmap/m17-disc-and-disk-images.md) | Done | M16 |
| [M18. RAR](roadmap/m18-rar.md) | In progress | M14, M15 |
| [M19. Legacy formats](roadmap/m19-legacy-formats.md) | Done | M13 |
| [M20. Zipper without 7-Zip](roadmap/m20-zipper-without-7-zip.md) | Done | M11 to M19, M21 |
| [M22. Zipper's own ZIP directory reader](roadmap/m22-zippers-own-zip-directory-reader.md) | Done | M21 |
| [M23. The LZMA decoder as its own crate](roadmap/m23-the-lzma-decoder-as-its-own-crate.md) | Planned | M21, M31 |
| [M24. ZIP and 7z throughput from the research report](roadmap/m24-zip-and-7z-throughput-from-the-research-report.md) | Done | M22 |
| [M25. A release build without C](roadmap/m25-a-release-build-without-c.md) | Done | M22 |
| [M26. Cargo features for each format](roadmap/m26-cargo-features-for-each-format.md) | Done | M25 |
| [M27. LZ4 and `compress` at the reference speed](roadmap/m27-lz4-and-compress-at-the-reference-speed.md) | Planned | M25 |
| [M28. Clean failure everywhere](roadmap/m28-clean-failure-everywhere.md) | Done | M8 |
| [M29. Overwrite at the speed of a fresh extraction](roadmap/m29-overwrite-at-the-speed-of-a-fresh-extraction.md) | Done | M8 |
| [M30. gzip end to end at libdeflate's speed](roadmap/m30-gzip-end-to-end-at-libdeflates-speed.md) | Done | M25 |
| [M31. A release case for each decoder](roadmap/m31-a-release-case-for-each-decoder.md) | Done | M25 |
| [M32. Every reader under Zipper's licence](roadmap/m32-every-reader-under-zippers-licence.md) | Done | M25 |
| [M33. Clean rewrites of the copied checks](roadmap/m33-clean-rewrites-of-the-copied-checks.md) | Planned | M32 |

M0 was done when the roadmap began on 1 November 2026. M1 closed the same day, with the reference corpus, the hostile tests and mutation testing of the ZIP reader. A run of 300,000 mutation cases found no failure, as [mutation testing](reference/zip-reader.md#mutation-testing) records. M2 closed on 2 November 2026, with Unix permissions, modification times and links that stay inside restored as the [ZIP reader reference](reference/zip-reader.md) lists. M3 closed the same day. `cargo xtask bench` regenerates every number in [Results](results.md), and the gate fails when the page differs from the committed JSON.

M4, M5 and M6 closed on 2 November 2026. The [Linux and Windows scoreboard](../benchmarks/cross-platform/windows-folder-cap/README.md) shows the default beating default bsdtar on every workload on both platforms. M5 deferred two items, parallel decoding of one Deflate entry and memory-mapped input, because the measured gain left for them is about 10 ms on one workload. M7 closed on 2 November 2026. The default engine reads plain, gzip, zstd and xz tar in process, with one decoding thread and four writers. Plain tar beats default bsdtar on every workload on macOS and Linux and on five of seven on Windows. The [Linux and Windows tar rows](../benchmarks/cross-platform/tar-scoreboard/README.md) leave one open item. On those platforms the zlib-rs decoder lost the large gzip entries to bsdtar, by up to 1.59× on Windows. M25 replaced zlib-rs with Zipper's own Deflate decoder, as the [no-zlib record](../benchmarks/deflate/no-zlib/README.md) shows on one Mac. No record since compares the gzip rows with bsdtar on Linux or Windows. M8 closed on 3 November 2026. Applications can call `zipper::extract_archive` to extract into an existing folder, select ZIP paths, follow progress and cancel. Four adversarial reviews found five issues, each fixed with a test that failed first. The 7-Zip backend reported no progress, until M20 removed it. M9 closed the same day. The default engine reads 7z in process with all of these options. It then sent BCJ2, PPMd, BZip2, encrypted and split 7z archives to 7-Zip, and since M13, M14 and M15 it reads them in process too. An early timing on one Mac shows the reader faster than 7-Zip on non-solid archives and 1.85× slower on solid Silesia, where 7-Zip decodes the two LZMA2 blocks at once. Splitting LZMA2 at its dictionary resets was the next experiment. The [LZMA2 segment record](../benchmarks/native-7z/lzma2-segments/README.md) made solid Silesia 1.59× faster at default threads, still 1.24× 7-Zip's time, and the [filtered segment record](../benchmarks/native-7z/filtered-lzma2-segments/README.md) did the same behind filters. M10 and M11 closed on 4 November 2026. `docs/reference/parity.md` lists 112 rows of 7-Zip extraction, and the gate fails when a row loses its proof. On Windows every reader now copies the archive's Mark of the Web, restores read-only, hidden and system attributes, and creates links that stay inside. 7-Zip stores NTFS alternate streams only in WIM, so they move to M17. Two adversarial reviews found no escape in the links. M12 closed the same day. `--existing` takes 7-Zip's skip and rename modes, `--include` and `--exclude` select in every native reader, tar included, and `--flat`, `-` and `--stdout` cover `7z e`, `-si` and `-so`. Two adversarial reviews found a race that replaced files written during extraction and two link escapes through links the destination keeps, all fixed with tests. Their shared formal checks were added afterwards, on 4 November 2026. An exhaustive check follows every published link on disk across 25,200 runs. The ZIP decoder budget has a TLA+ model, and the new header parsers have Kani proofs. Every stream end is tested at every truncation and padding.

M13 closed on 5 November 2026. Every ZIP method, 7z codec and compressed tar in the M10 matrix reads in process, except ZIP's Shrink and Implode, which moved to M19, and encryption, which is M14. The Codecs section of [Results](results.md) extracts one Silesia archive per codec at one thread. At `742ddef`, where M13 closed, Zipper beat 7-Zip on 7z Deflate64 by 1.31× and bsdtar on lzip by 1.11×, and matched 7-Zip on PPMd. It was slower on the rest. 7z BCJ2 took 4.44 times 7-Zip's time, LZ4 tar 3.26 times bsdtar's and `compress` tar 2.71 times 7-Zip's. BZip2 takes 1.39 to 1.40 times, ZIP Deflate64 1.46 times, because it runs two decoders to refuse trailing data, and LZMA, xz and Swap4 1.24 to 1.31 times. Records since then changed most of these rows. Two BCJ2 records, [buffered branches](../benchmarks/native-7z/bcj2-buffered-branches/README.md) and [the slice scan](../benchmarks/native-7z/bcj2-slice-scan/README.md), made BCJ2 faster, and 7-Zip is still faster at one thread. ZIP Deflate64 [decodes each entry once](../benchmarks/native-zip/deflate64-one-inflater/README.md) and beats 7-Zip. [Zipper's own BZip2 decoder](../benchmarks/bzip2/own-decoder-one-thread/README.md) beats 7-Zip 26.06 at one thread. M21 replaced liblzma, and [M27](roadmap/m27-lz4-and-compress-at-the-reference-speed.md) holds the LZ4 and `compress` gaps. Building the rows found that a `.tar.lz4` whose first block was over 1 MiB extracted as one file, fixed in `39712da`.

M21, M14, M15, M16 and M22 closed on 8 November 2026. M21 closed by the user's decision, with the speed half of its fourth exit check unmet. At one thread on solid Silesia, 7-Zip takes 1.335 s and Zipper's safe decoder 1.484 s, as the [unchecked tables record](../benchmarks/lzma/unchecked-tables/README.md) shows, and the gap left is 7-Zip's hand-written arm64 loop. M14 decrypts ZipCrypto, WinZip AES and 7z AES-256 in process. M15 reads raw split volumes, ZIP split sets and self-extracting ZIP and 7z in process, with Info-ZIP sets and stubs the tests build standing in for WinZip. M16 read every package format of the M10 matrix in process. M22 lists ZIP entries with Zipper's own directory reader, so the `zip` crate is a dev-dependency only. M25 closed on 11 November 2026. The release build compiles no C, and the gate's `pure rust` step fails when a crate in the build compiles or links native code.

M30 closed on 13 November 2026. On one Apple M2 Max, `zipper extract` of a 212 MB gzip file takes 209.5 ms against 218.7 ms for `libdeflate-gunzip`, with the ranges apart, as the [gzip file record](../benchmarks/deflate/gzip-file/README.md) shows. It was measured on one machine and one corpus.

M20 closed on 14 November 2026. No code path in the library or the CLI runs 7-Zip. A format with a reader goes to it, and one without fails with `zipper::Refused`. `50cde4b` removed the backend, and `tests/no_7zip.rs` checks that a 7-Zip on `PATH` never starts for any file in `tests/data`.

M31 closed on 14 November 2026. Each of Zipper's decoders was measured against the best pure-Rust crate and the best crate that wraps a C library. The runs covered an Apple M2 Max and hosted x86-64 Linux, on Silesia and the locked dependency sources. BZip2 and XPRESS lead in all four runs and become crates. Zstandard and LZMA trail libzstd and liblzma on Linux, and Deflate ties `zlib-rs` there, so all three stay inside Zipper and M23 does not start. The [experiment log](../benchmarks/README.md#release-cases-for-each-decoder) lists the five records.

M24 closed on 14 November 2026. Every idea of the research report on ZIP and 7z extraction has a record or a reason on its page. The kept changes include [buffered BCJ2 branch streams](../benchmarks/native-7z/bcj2-buffered-branches/README.md), [the doubling copy for short LZMA matches](../benchmarks/lzma/repeating-copy/README.md), [the 7z solid-folder writer pool](../benchmarks/native-7z/solid-writer-pool/README.md) and [parallel BZip2 blocks](../benchmarks/bzip2/block-parallel/README.md). Two ideas were declined. Detecting folder merges from `mkdir` results could save at most 1.8 traced ms of `stat` calls on the dependency sources. The LZMA literal path is left with a gap to 7-Zip that only its hand-written arm64 loop is shown to close, and Zipper stays pure Rust.

M29 closed on 13 November 2026. `--overwrite` into a folder that holds every file of the 28,000-file archive takes 1.80 times as long as into an empty one, down from 5.19 times, as the [full-folder record](../benchmarks/overwrite/full-folder/README.md) shows. The removal of the old files on APFS keeps it above 1.5 times. `e2dc81e` synced the exhaustive overwrite and cancel checks on the final code, after the Windows changes to `src/staging.rs`.
M17, M19, M26 and M32 closed on 14 November 2026. M17 reads ISO 9660, UDF and WIM in process and refuses the other images for licence reasons. The hosted Windows release candidate run on `0824bf1` passed the tests that write a WIM's named streams as NTFS streams. M19 closed when the parity matrix marked `Nsis` dropped, since its unshipped reader was a port of LGPL code. M26 closed when `954d0f7` ran the RAR tests in a build with only `rar` and made `cargo xtask format-tests` refuse a format feature with no tests. M32 closed on the same `Nsis` row and on `v0.1.0-alpha.3`, cut at `5d70a20` after the second audit.

M28 closed on 14 November 2026. A failed or cancelled extraction leaves the destination as it was on Linux, macOS and Windows, in `--flat` mode too. On Windows, Ctrl-C and Ctrl-Break cancel and roll back as SIGINT does on Unix, and `tests/console_cancel.rs` sends each during a run on the Windows check. A cancel that meets a failing rename returns `Cancelled`. Closing the Windows console window still ends a run partway.

## Close a milestone

1. Confirm every exit check with its gate step, test or record.
2. Run every owed slow sync that `cargo xtask verify --owed` lists.
3. Set the milestone's status to Done in this page.
4. Run the `zipper-reflect` skill and commit what it encodes.
