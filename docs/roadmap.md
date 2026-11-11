# Roadmap

The first phase ended with M9. Zipper's in-process readers replaced 7-Zip as the default for ZIP, tar and 7z. They are correct on hostile input, restore the metadata users expect, and extract faster with bounded memory. Speed work starts on macOS. CI runs every correctness check on Linux for each push. The Windows workflow runs them on Windows when invoked, before a release.

The second phase, M10 to M21, ends when Zipper extracts everything 7-Zip extracts without 7-Zip installed, with LZMA decoded by its own code. Each new reader keeps the first phase's contract. It refuses unsafe paths and links, publishes nothing on failure, bounds its memory and passes the `zipper-review` skill. A faster reader never drops one of these checks.

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
| [M17. Disc and disk images](roadmap/m17-disc-and-disk-images.md) | In progress | M16 |
| [M18. RAR](roadmap/m18-rar.md) | Planned | M14, M15 |
| [M19. Legacy formats](roadmap/m19-legacy-formats.md) | In progress | M13 |
| [M20. Zipper without 7-Zip](roadmap/m20-zipper-without-7-zip.md) | Planned | M11 to M19, M21 |
| [M22. Zipper's own ZIP directory reader](roadmap/m22-zippers-own-zip-directory-reader.md) | Done | M21 |
| [M23. The LZMA decoder as its own crate](roadmap/m23-the-lzma-decoder-as-its-own-crate.md) | Planned | M21 |
| [M24. ZIP and 7z throughput from the research report](roadmap/m24-zip-and-7z-throughput-from-the-research-report.md) | In progress | M22 |
| [M25. A release build without C](roadmap/m25-a-release-build-without-c.md) | Done | M22 |
| [M26. Cargo features for each format](roadmap/m26-cargo-features-for-each-format.md) | In progress | M25 |
| [M27. LZ4 and `compress` at the reference speed](roadmap/m27-lz4-and-compress-at-the-reference-speed.md) | Planned | M25 |
| [M28. Clean failure everywhere](roadmap/m28-clean-failure-everywhere.md) | Planned | M8 |

M4, M5 and M6 closed on 2 November 2026. The [Linux and Windows scoreboard](../benchmarks/cross-platform/windows-folder-cap/README.md) shows the default beating default bsdtar on every workload on all three platforms. M5 deferred two items, parallel decoding of one Deflate entry and memory-mapped input, because the measured gain left for them is about 10 ms on one workload. M7 closed on 2 November 2026. The default engine reads plain, gzip, zstd and xz tar in process, with one decoding thread and four writers. Plain tar beats default bsdtar on every workload on macOS and Linux and on five of seven on Windows. The [Linux and Windows tar rows](../benchmarks/cross-platform/tar-scoreboard/README.md) leave one open item. On those platforms the zlib-rs decoder loses the large gzip entries to bsdtar, by up to 1.59× on Windows. A faster streaming inflate there, such as zlib-ng, is the next experiment. M8 closed on 3 November 2026. Applications can call `zipper::extract_archive` to extract into an existing folder, select ZIP paths, follow progress and cancel. Four adversarial reviews found five issues, each fixed with a test that failed first. The 7-Zip backend reports no progress yet. M9 closed the same day. The default engine reads 7z in process with all of these options, and sends BCJ2, PPMd, BZip2, encrypted and split 7z archives to 7-Zip. An early timing on one Mac shows the reader faster than 7-Zip on non-solid archives and 1.85× slower on solid Silesia, where 7-Zip decodes the two LZMA2 blocks at once. Splitting LZMA2 at its dictionary resets is the next experiment. M10 and M11 closed on 4 November 2026. `docs/reference/parity.md` lists 112 rows of 7-Zip extraction, and the gate fails when a row loses its proof. On Windows every reader now copies the archive's Mark of the Web, restores read-only, hidden and system attributes, and creates links that stay inside. 7-Zip stores NTFS alternate streams only in WIM, so they move to M17. Two adversarial reviews found no escape in the links. M12 closed the same day. `--existing` takes 7-Zip's skip and rename modes, `--include` and `--exclude` select in every native reader, tar included, and `--flat`, `-` and `--stdout` cover `7z e`, `-si` and `-so`. Two adversarial reviews found a race that replaced files written during extraction and two link escapes through links the destination keeps, all fixed with tests. Their shared formal checks were added afterwards, on 4 November 2026. An exhaustive check follows every published link on disk across 25,200 runs. The ZIP decoder budget has a TLA+ model, and the new header parsers have Kani proofs. Every stream end is tested at every truncation and padding.

M13 closed on 5 November 2026. Every ZIP method, 7z codec and compressed tar in the M10 matrix reads in process, except ZIP's Shrink and Implode, which moved to M19, and encryption, which is M14. The Codecs section of [Results](results.md) extracts one Silesia archive per codec at one thread. Zipper beats 7-Zip on 7z Deflate64 by 1.31× and bsdtar on lzip by 1.11×, and matches 7-Zip on PPMd. It is slower on the rest. 7z BCJ2 takes 4.44 times 7-Zip's time, LZ4 tar 3.26 times bsdtar's and `compress` tar 2.71 times 7-Zip's. BZip2 takes 1.39 to 1.40 times, ZIP Deflate64 1.46 times, because it runs two decoders to refuse trailing data, and LZMA, xz and Swap4 1.24 to 1.31 times. Those gaps are the next experiments, after M21 replaces liblzma. Building the rows found that a `.tar.lz4` whose first block was over 1 MiB extracted as one file, fixed in `39712da`.

## Close a milestone

1. Confirm every exit check with its gate step, test or record.
2. Run every owed slow sync that `cargo xtask verify --owed` lists.
3. Set the milestone's status to Done in this page.
4. Run the `zipper-reflect` skill and commit what it encodes.
