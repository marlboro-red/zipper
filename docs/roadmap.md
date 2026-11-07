# Roadmap

The first phase ended with M9. Zipper's in-process readers replaced 7-Zip as the default for ZIP, tar and 7z. They are correct on hostile input, restore the metadata users expect, and extract faster with bounded memory. Speed work starts on macOS. CI runs every correctness check on Linux for each push. The Windows workflow runs them on Windows when invoked, before a release.

The second phase, M10 to M21, ends when Zipper extracts everything 7-Zip extracts without 7-Zip installed, with LZMA decoded by its own code. Each new reader keeps the first phase's contract. It refuses unsafe paths and links, publishes nothing on failure, bounds its memory and passes the `zipper-review` skill. A faster reader never drops one of these checks.

Every milestone from M10 to M21 has one more exit check, beside its own. New parsing code has a Kani proof, or an exhaustive test where Kani does not finish. New concurrency has a TLA+ model with its own `zipper-verify-*` skill. Each proof, test and model is listed in `verification/coverage.txt`, so the `verify` gate step catches drift. A planted break, made with `scripts/plant.py`, shows each one can fail.

Each milestone closes only when its exit checks pass. A check is a gate step, a test or an experiment record, never an opinion. [The design page](design.md#implementation-proceeds-through-five-ordered-gates) describes how extraction experiments proceed.

## Milestones

| Milestone | Status | Depends on |
| --- | --- | --- |
| [M0. Verified workflow](#m0-verified-workflow) | Done | None |
| [M1. Correctness contract for ZIP](#m1-correctness-contract-for-zip) | Done | M0 |
| [M2. Metadata fidelity](#m2-metadata-fidelity) | Done | M1 |
| [M3. Fixed scoreboard](#m3-fixed-scoreboard) | Done | M0 |
| [M4. Small-file output path](#m4-small-file-output-path) | Done | M3 |
| [M5. Remaining extraction headroom](#m5-remaining-extraction-headroom) | Done | M3 |
| [M6. Native reader as the default](#m6-native-reader-as-the-default) | Done | M1, M2 |
| [M7. Native tar family](#m7-native-tar-family) | Done | M5 |
| [M8. Extraction API for applications](#m8-extraction-api-for-applications) | Done | M7 |
| [M9. Native 7z reader](#m9-native-7z-reader) | Done | M8 |
| [M10. Parity matrix](#m10-parity-matrix) | Done | M9 |
| [M11. Windows metadata parity](#m11-windows-metadata-parity) | Done | M10 |
| [M12. Extraction controls](#m12-extraction-controls) | Done | M10 |
| [M13. Remaining codecs](#m13-remaining-codecs) | Done | M10 |
| [M21. Zipper's own LZMA decoder](#m21-zippers-own-lzma-decoder) | In progress | M10 |
| [M14. Encrypted archives](#m14-encrypted-archives) | Done | M13 |
| [M15. Split, spanned and self-extracting archives](#m15-split-spanned-and-self-extracting-archives) | Done | M13 |
| [M16. Package formats](#m16-package-formats) | Done | M13 |
| [M17. Disc and disk images](#m17-disc-and-disk-images) | Planned | M16 |
| [M18. RAR](#m18-rar) | Planned | M14, M15 |
| [M19. Legacy formats](#m19-legacy-formats) | Planned | M13 |
| [M20. Zipper without 7-Zip](#m20-zipper-without-7-zip) | Planned | M11 to M19, M21 |
| [M22. Zipper's own ZIP directory reader](#m22-zippers-own-zip-directory-reader) | Done | M21 |
| [M23. The LZMA decoder as its own crate](#m23-the-lzma-decoder-as-its-own-crate) | Planned | M21 |

M4, M5 and M6 closed on 2 November 2026. The [Linux and Windows scoreboard](../benchmarks/cross-platform/windows-folder-cap/README.md) shows the default beating default bsdtar on every workload on all three platforms. M5 deferred two items, parallel decoding of one Deflate entry and memory-mapped input, because the measured gain left for them is about 10 ms on one workload. M7 closed on 2 November 2026. The default engine reads plain, gzip, zstd and xz tar in process, with one decoding thread and four writers. Plain tar beats default bsdtar on every workload on macOS and Linux and on five of seven on Windows. The [Linux and Windows tar rows](../benchmarks/cross-platform/tar-scoreboard/README.md) leave one open item. On those platforms the zlib-rs decoder loses the large gzip entries to bsdtar, by up to 1.59× on Windows. A faster streaming inflate there, such as zlib-ng, is the next experiment. M8 closed on 3 November 2026. Applications can call `zipper::extract_archive` to extract into an existing folder, select ZIP paths, follow progress and cancel. Four adversarial reviews found five issues, each fixed with a test that failed first. The 7-Zip backend reports no progress yet. M9 closed the same day. The default engine reads 7z in process with all of these options, and sends BCJ2, PPMd, BZip2, encrypted and split 7z archives to 7-Zip. An early timing on one Mac shows the reader faster than 7-Zip on non-solid archives and 1.85× slower on solid Silesia, where 7-Zip decodes the two LZMA2 blocks at once. Splitting LZMA2 at its dictionary resets is the next experiment. M10 and M11 closed on 4 November 2026. `docs/reference/parity.md` lists 112 rows of 7-Zip extraction, and the gate fails when a row loses its proof. On Windows every reader now copies the archive's Mark of the Web, restores read-only, hidden and system attributes, and creates links that stay inside. 7-Zip stores NTFS alternate streams only in WIM, so they move to M17. Two adversarial reviews found no escape in the links. M12 closed the same day. `--existing` takes 7-Zip's skip and rename modes, `--include` and `--exclude` select in every native reader, tar included, and `--flat`, `-` and `--stdout` cover `7z e`, `-si` and `-so`. Two adversarial reviews found a race that replaced files written during extraction and two link escapes through links the destination keeps, all fixed with tests. Their shared formal checks were added afterwards, on 4 November 2026. An exhaustive check follows every published link on disk across 25,200 runs. The ZIP decoder budget has a TLA+ model, and the new header parsers have Kani proofs. Every stream end is tested at every truncation and padding.

M13 closed on 5 November 2026. Every ZIP method, 7z codec and compressed tar in the M10 matrix reads in process, except ZIP's Shrink and Implode, which moved to M19, and encryption, which is M14. The Codecs section of [Results](results.md) extracts one Silesia archive per codec at one thread. Zipper beats 7-Zip on 7z Deflate64 by 1.31× and bsdtar on lzip by 1.11×, and matches 7-Zip on PPMd. It is slower on the rest. 7z BCJ2 takes 4.44 times 7-Zip's time, LZ4 tar 3.26 times bsdtar's and `compress` tar 2.71 times 7-Zip's. BZip2 takes 1.39 to 1.40 times, ZIP Deflate64 1.46 times, because it runs two decoders to refuse trailing data, and LZMA, xz and Swap4 1.24 to 1.31 times. Those gaps are the next experiments, after M21 replaces liblzma. Building the rows found that a `.tar.lz4` whose first block was over 1 MiB extracted as one file, fixed in `39712da`.

## M0. Verified workflow

The repository enforces its own rules.

- `cargo xtask gate fast` runs format, docs and comment lints, clippy, a Windows lint, tests, a release build and a byte-exact round trip. The pre-push hook runs it. Tests build with the `ci` profile, optimized without LTO and with overflow checks and debug assertions on. The release build keeps LTO.
- `cargo xtask gate full` adds the counters build and a benchmark smoke run. CI runs it on Linux.
- `cargo xtask lint-docs` checks writing rules, links, record headings, reachability and every path a page names.
- Benchmark scripts record CPU time and refuse comparisons without a 1-thread Zipper row.
- The skills in `.claude/skills/` cover benchmarking, review, documentation and reflection.

## M1. Correctness contract for ZIP

The native reader extracts the same bytes as a reference extractor and survives hostile archives.

Scope:

- A reference comparison that extracts each corpus archive with Zipper, bsdtar and 7-Zip and compares the trees. Done in the [reference corpus](reference/zip-reader.md#reference-corpus), which still lacks Java `jar` output.
- A corpus of real producer output. It covers Java `jar`, Windows Explorer, macOS Archive Utility, Info-ZIP `zip`, Python `zipfile` and 7-Zip. It includes ZIP64 by size and by entry count, data descriptors and archive comments.
- A hostile collection with a test for each case. It covers path traversal, absolute paths, drive letters, duplicate names, names that collide by case, overlapping entries, a zero declared size hiding real data, local and central headers that disagree, truncation and inconsistent ZIP64 fields.
- Fuzz targets for the central-directory and local-header parsers. Done as [mutation testing](reference/zip-reader.md#mutation-testing) on stable Rust. Coverage-guided fuzzing still needs a nightly toolchain.
- Local headers that agree with the central directory, and records that do not overlap. Done in the [local-header check](../benchmarks/native-zip/local-header-check/README.md).
- Unicode path extra fields (`0x7075`) that rename an entry are rejected, so every reader uses the same name. Done in the [Unicode path field check](../benchmarks/native-zip/unicode-path/README.md).
- Records that tile the archive, with checked data descriptors, the local encryption flag and local ZIP64 sizes. Done in the [record layout check](../benchmarks/native-zip/record-layout/README.md).
- Deflate streams that end exactly at their compressed size, and stored entries that cannot hide a descriptor. Done in [exact decoding](../benchmarks/native-zip/exact-decoding/README.md).
- A limit on total extracted bytes, with a documented default. Done. Output must fit in free space unless `--max-extracted-bytes` sets a cap.

Exit checks:

1. The reference comparison finds no byte mismatch on any corpus archive.
2. Every hostile case is rejected or stays inside the destination, and its test fails if the check is removed.
3. A short fuzz run executes in `cargo xtask gate fast` and `gate full`. Longer runs find no crash.
4. Each parser change passed the `zipper-review` skill.

## M2. Metadata fidelity

The native reader restores what users expect from a ZIP extractor.

Scope:

- Unix permission bits for entries made on Unix, masked to `0o777`. Done in [permission restore](../benchmarks/native-zip/permissions/README.md).
- Modification times from the DOS fields and the extended timestamp field. Directory times are set after their children. Done in [timestamp restore](../benchmarks/native-zip/timestamps/README.md).
- A symlink policy, decided and documented before implementation. The [ZIP reader reference](reference/zip-reader.md#symlinks) records the decision, which is to create only links that stay inside the destination.
- Filename decoding for UTF-8 and CP437, and a Unicode path field that spells a CP437 name in UTF-8. Done in the [ZIP reader reference](reference/zip-reader.md#supported-archives). No three reference readers agree on a CP437 name on macOS, so the corpus cannot hold one, and `tests/hostile_zip.rs` covers it.
- A Unicode normalization policy. Done in the [ZIP reader reference](reference/zip-reader.md#name-normalization). Zipper keeps stored name bytes, as `unzip`, Python and 7-Zip do.

Exit checks:

1. The reference comparison also compares permissions and modification times.
2. `docs/reference/zip-reader.md` lists exactly what is restored and what is not.
3. Benchmarks compare against default bsdtar, because both sides now do the same metadata work. Done in `scripts/extraction_matrix.py`. The [bsdtar comparison](../benchmarks/native-zip/bsdtar-comparison.md) predates metadata restore, so M3 measures it again.

## M3. Fixed scoreboard

One command measures every tracked workload the same way and publishes the numbers.

Scope:

- A new `bench` subcommand of `cargo xtask` runs fixed workloads. They are 10,000 flat tiny files, 4,000 grouped files, the dependency corpus, a Silesia ZIP, one large compressible entry, one large random entry and a mixed tree.
- Every run includes the parity rows, CPU time and peak memory.
- The command generates `docs/results.md` from the committed JSON.
- Instrumentation reports decoder CPU, bytes read and written, and output operations, outside timed runs. Done with the `counters` feature. Decoder CPU is reported as wall time inside decoder calls.

Exit checks:

1. `docs/results.md` is generated, and the gate fails when it does not match the committed JSON. Done in the `results` gate step.
2. Two sessions on one host record the session-to-session drift for each workload. Done on 2026-11-02 in the drift column of [Results](results.md). Most rows moved less than 5%, and rows near 25 ms moved up to 11%.
3. One command reproduces every published number. `cargo xtask bench` regenerates every number in [Results](results.md).

## M4. Small-file output path

Extraction of many small files gets faster at matched threads and CPU, starting on macOS.

Scope:

- Trace macOS extraction outside timed runs and name the limiter before changing code.
- Restate the 3× target against default bsdtar at matched parity before the first experiment. Done in [small-file creation cost on macOS](../benchmarks/native-zip/create-cost/README.md). On this Mac the system-call floor allows at most about 1.33× at one thread and 2.3× at four threads, so 3× is unreachable at M2 parity.
- Test one change at a time, following the design gates. Replicate on Linux and Windows after macOS.

Exit checks:

1. A kept change beats the spread on the M3 small-file workloads at matched threads, with CPU time reported.
2. Linux and Windows show no regression on the same workloads.

## M5. Remaining extraction headroom

Every scoreboard workload that is not at its measured limit gets faster, without unbounded memory.

Scope:

- Profile each slow path outside timed runs and name its limiter before changing code.
- The copy path of a large entry. Native takes 57 ms for the 256 MiB random entry, where bsdtar takes 43 ms.
- One-thread Deflate decoding. Native takes 239 ms on Silesia at one thread, where bsdtar takes 184 ms.
- The read amplification of tiny entries, which read 21 times the ZIP size on flat-tiny.
- Parallel decoding of a single Deflate entry above a size threshold, with streaming as the fallback.
- A memory-mapped input experiment, with a fallback when the archive changes during extraction.

Exit checks:

1. Each kept change beats the session drift on its scoreboard rows, with CPU time and peak memory reported.
2. The memory bound is documented and tested, and CRC checks still cover every byte.
3. Hostile Deflate streams pass the `zipper-review` skill.

## M6. Native reader as the default

`zipper extract` uses the in-process reader for ZIP without any flag.

Scope:

- Make the native reader the default `--zip-engine` and choose between `native` and `native-fast` from M3 data. Done. The default `auto` engine uses `native`, which peaks under 9 MiB on every scoreboard workload.
- Keep 7-Zip for formats other than ZIP.
- Update the Windows package and the docs.

Exit checks:

1. Default ZIP extraction runs in-process on all three operating systems in CI.
2. The M1 and M2 checks pass on all three operating systems.
3. `docs/usage.md` and `docs/reference/zip-reader.md` describe the new default.

## M7. Native tar family

`zipper extract` reads tar and compressed tar in process, so the archives most people extract on macOS and Linux no longer need 7-Zip.

Scope:

- A streaming tar reader with the M1 safety contract and the M2 metadata contract, applied to tar entries. Done in the [tar reader reference](reference/tar-reader.md).
- Decompression for gzip, zstd and xz in front of it, so `.tar.gz`, `.tgz`, `.tar.zst` and `.tar.xz` extract their files and not an inner `.tar`. Done.
- POSIX ustar, pax and GNU headers, plus old tar without the `ustar` marker. Done, except GNU sparse files.
- A reference corpus from GNU tar, bsdtar and Python `tarfile`. Done in the [tar reference corpus](reference/tar-reader.md#reference-corpus).
- Hostile tar tests for traversal, absolute names, links that escape, hard links, special files, duplicate names and truncation. Done in `tests/hostile_tar.rs`, after an adversarial review.
- Scoreboard rows for the tar workloads against bsdtar. Done in [Results](results.md) for macOS, and in the [Linux and Windows tar rows](../benchmarks/cross-platform/tar-scoreboard/README.md).

Exit checks:

1. The default engine extracts every corpus tar format in process on all three operating systems, with the same tree as the reference readers.
2. Every hostile tar layout has a test that failed before its fix, and the adversarial review finds no escape.
3. `docs/reference/` documents the tar reader, and the scoreboard shows its tar rows.

## M8. Extraction API for applications

An application can call Zipper as a Rust library to update an installed folder, extract part of an archive, show progress and stop midway.

Scope:

- Extraction into an existing folder, where each archive file replaces the file of the same name by rename. Done with `zipper::Existing::Overwrite` and `zipper extract --overwrite`, in every reader.
- Selected folders and files from a ZIP archive. Done with `paths` in `native_zip::ReadOptions` and `zipper extract --path`.
- A progress callback that reports files and decoded bytes. Done for ZIP and tar with `zipper::Control`.
- Cancellation from another thread. A cancelled extraction changes nothing in the destination. Done for ZIP and tar, with `tests/control.rs`.
- One library function that picks the reader for an archive, as `zipper extract` does. Done with `zipper::extract_archive`.

Exit checks:

1. Tests show that a failed, rejected or cancelled extraction leaves an existing destination unchanged and leaves no staging folder.
2. The adversarial review finds no write outside the destination, including through links that already exist in it.
3. `docs/usage.md` and the reader references describe each option.

## M9. Native 7z reader

`zipper extract` reads `.7z` in process, so 7-Zip is no longer needed for the archives applications most often ship.

Scope:

- A 7z container parser with the M1 safety contract, covering the header, packed streams, folders and substreams. Done in the [7z reader reference](reference/7z-reader.md).
- LZMA and LZMA2 through liblzma, plus the BCJ x86 and ARM64 filters. Other codecs and encrypted archives are refused with a clear error. Done, with Copy, Deflate, Delta and every liblzma branch filter. The default engine sends the rest to 7-Zip.
- The M8 options, including selected folders, progress and cancellation. Done.
- A reference corpus from 7-Zip and libarchive, and hostile 7z tests. Done in `tests/sevenz_corpus.rs`, `tests/hostile_7z.rs` and `tests/sevenz_fuzz.rs`.

Exit checks:

1. The default engine extracts every corpus 7z archive in process on all three operating systems, with the same tree as 7-Zip and bsdtar.
2. Every hostile 7z layout has a test that failed before its fix, and the adversarial review finds no escape.
3. `docs/reference/` documents the 7z reader.

## M10. Parity matrix

One generated table shows what Zipper extracts natively, what it hands to 7-Zip and what it cannot extract. Each of M11 to M20 moves rows in it.

Scope:

- A script lists every format, method and filter that `7z i` reports, and every `7z x` switch that changes extraction.
- Each row records native, backend or missing, with the test or corpus archive that proves it.
- A corpus archive for each format, written by 7-Zip or the format's own tool, with its tree as 7-Zip extracts it.
- The table lives in `docs/reference/` and is generated, like `docs/results.md`.

Exit checks:

1. A gate step regenerates the table and fails when it differs from the committed copy.
2. Every native and partial row names a proof that passes in CI. Every backend row with a corpus archive extracts it through 7-Zip in CI.
3. The table counts the rows with no proof yet. The milestone that moves such a row adds its proof.

## M11. Windows metadata parity

Files extracted on Windows carry the same safety marks and attributes as with 7-Zip.

Scope:

- Mark of the Web. When the archive has a `Zone.Identifier` stream, every extracted file gets one, as 7-Zip's `-snz` does. This comes first, because without it Windows stops warning about downloaded files once they leave the archive.
- The read-only, hidden, system and archive attributes from ZIP, 7z and tar entries made on Windows.
- Symlinks on Windows. Zipper creates links that stay inside the destination when the process may create them, and refuses the archive otherwise, with a clear error. Junctions are never created.
- NTFS alternate data streams stored by 7-Zip's `-sns`, behind an option, with names checked like paths. 7-Zip 26.05 stores them only in WIM, so they move to M17, and `src/ntfs_streams.rs` holds the stream writer they will use.

Exit checks:

1. A Windows CI test downloads a file through a path that sets `Zone.Identifier`, extracts it, and finds the mark on every output file.
2. A Windows corpus round-trips attributes and links, and its output matches 7-Zip's.
3. The adversarial review finds no stream name or link that writes outside the destination.

## M12. Extraction controls

The command line and the library offer the extraction choices 7-Zip offers.

Scope:

- Overwrite modes that skip existing files, rename new files or rename existing files, beside `Refuse` and `Overwrite`. They follow 7-Zip's `-aos`, `-aou` and `-aot`.
- Include and exclude patterns with wildcards, like 7-Zip's `-i` and `-x`, for every format. Tar selects entries while streaming.
- Extraction without folders, like `7z e`. Two archive files with one name follow the overwrite mode.
- Reading the archive from standard input and writing one file to standard output, like `-si` and `-so`.
- No option keeps files from a damaged archive. Decided on 4 November 2026, because it would end the rule that a damaged archive publishes nothing.

Exit checks:

1. The exhaustive overwrite check covers every overwrite mode.
2. Each control has a test on every native reader, and its output matches 7-Zip's on the M10 corpus.
3. `docs/usage.md` documents each control.

## M13. Remaining codecs

Every method 7-Zip decodes inside ZIP, 7z and tar decodes in process.

Scope:

- ZIP methods Deflate64, BZip2, LZMA, Zstandard, xz and PPMd.
- 7z methods BZip2, PPMd and BCJ2, which the default engine now sends to 7-Zip.
- Tar inside bzip2, lzip, LZ4 and the old `compress` format (`.Z`).
- A single compressed file outside tar, such as `.gz`, `.bz2`, `.xz`, `.zst` or `.lzma`, which extracts to one file named from the archive.
- Each decoder stops exactly at its declared size and checks the CRC of every byte. A Rust crate is used only after the `zipper-review` skill checks it on hostile input.

Exit checks:

1. Every method row in the M10 matrix for ZIP, 7z and tar reads native, except ZIP's Shrink and Implode, which moved to M19.
2. Each codec has hostile tests for truncation, a stream longer than declared and a wrong CRC, where its format carries a length or a CRC. `compress` (`.Z`) carries neither, and the stream reference says so.
3. The scoreboard gains a row per new codec, compared with 7-Zip at one thread. lzip and LZ4, which 7-Zip does not read, are compared with bsdtar.

Done so far, by 5 November 2026:

- ZIP Deflate64, BZip2, LZMA, Zstandard, xz and PPMd.
- 7z BZip2, Deflate64, PPMd, BCJ2 with its own decoder, and Swap2 and Swap4.
- Tar and single files in bzip2, lzip, LZ4 with its own decoder, and `compress` with its own decoder.

The scoreboard rows came last, and M13 closed. ZIP's Shrink and Implode moved to M19 on 5 November 2026. No current tool writes them, and how Shrink's partial clear treats the code read just before it has to be settled by tests against 7-Zip first. A Shrink decoder and its tests are kept as a starting point.

## M21. Zipper's own LZMA decoder

LZMA and LZMA2 decode through Zipper's own Rust code, at least as fast as 7-Zip at one thread.

A comparison on 5 November 2026 showed why. At one thread, Zipper takes 1.25× 7-Zip's time on solid Silesia, and that gap is liblzma against 7-Zip's own decoder. sevenz-rust2, whose decoder is pure Rust, also decodes an LZMA setting (`lc=8`) that liblzma refuses, so Zipper sends that archive to 7-Zip. The parallel LZMA2 segments that put Zipper ahead of sevenz-rust2 cost 347 MiB against 7-Zip's 256 MiB at default threads.

Scope:

- A range decoder, LZMA and LZMA2 in safe Rust, with every `lc`, `lp` and `pb` the format allows, used by 7z, ZIP LZMA, `.lzma`, lzip and the parallel LZMA2 segments.
- The xz container, with its CRC-32, CRC-64 and SHA-256 checks.
- The branch filters (x86, ARM, ARM Thumb, ARM64, RISC-V, PowerPC, SPARC, IA-64) and Delta, so liblzma is no longer needed to read.
- The decoder's dictionary and buffers count against the shared decoder budget, and parallel segments hold no more memory than 7-Zip does at default threads.

Exit checks:

1. A differential test decodes every LZMA, LZMA2 and xz stream in the corpora with Zipper's decoder and with liblzma, and finds the same bytes or the same refusal. It also runs a pseudo-random set of streams built from them.
2. Kani proves the range decoder, the bit-tree and length decoders and the match copy never panic or read or write out of bounds, up to a bound. An exhaustive test covers what Kani cannot finish.
3. Each branch filter round-trips every short input under liblzma's encoder, and its hostile tests cover every truncation.
4. A benchmark record shows Zipper's decoder at least as fast as 7-Zip's at one thread on solid Silesia, with ranges apart, and peak memory at default threads no higher than 7-Zip's.
5. The 7z corpus archive with `lc=8` reads natively, and its parity row moves from 7-Zip to native.

Done so far, by 5 November 2026:

- A raw LZMA decoder in safe Rust, `src/lzma.rs`, used only by tests so far. It decodes 64 MiB of Silesia in 436.9 ms, against 400.9 ms for liblzma and about 310 ms for 7-Zip, as the [hot-loop record](../benchmarks/lzma/hot-loop/README.md) shows.
- Differential tests against liblzma for every `lc`, `lp` and `pb`, every cut stream and an end marker before the declared size.
- An exhaustive test of the match copy in every window state. A Kani proof of it ran out of memory and was dropped for that test.
- An LZMA2 reader on the same decoder, also used only by tests, with the same bytes or the same refusal as liblzma for every control code, every properties byte and every cut. A builder agent wrote it, and its tests found a read that could return bytes a chunked match copy had just overrun, fixed in `a69a361`.
- An xz container reader on the same LZMA2 decoder, with the branch filters and Delta, also used only by tests so far. Kani proves the range coder and symbol decoders, and a reader fails every read after its first error.

Done by 7 November 2026:

- The decoder takes 0.878 times liblzma's time on the probe stream, as the [tree-preload record](../benchmarks/lzma/tree-preload/README.md) shows.
- 7z decodes LZMA and LZMA2 with it, and the `lc=8` corpus archive reads natively. Silesia extracts 7 to 9% faster than with liblzma, and 7-Zip is still about 8% faster at one thread, as the [switch record](../benchmarks/native-7z/own-decoder-shipped/README.md) shows. Exit check 4 is not met yet.
- 7z decodes its branch and Delta filters with `src/xz_filters.rs`, at unchanged speed, as the [filters record](../benchmarks/native-7z/own-filters/README.md) shows. The 7z reader no longer uses liblzma outside tests.
- Parallel LZMA2 segments peak at 146 MiB on solid Silesia at default threads, against 257 MiB for 7-Zip, as the [segment memory record](../benchmarks/native-7z/segment-memory/README.md) shows. That meets the memory half of exit check 4.
- Two changes from a machine-level study of 7-Zip's decoder made 7z Silesia extract about 3.5% faster, as the [kernel record](../benchmarks/lzma/7zip-study-kernel/README.md) shows. 7-Zip is still about 11% faster at one thread, so exit check 4 is not met yet.
- tar, the ZIP XZ method and RPM decode xz with Zipper's own reader. Silesia `.tar.xz` extracts 11 to 15% faster than with liblzma, as the [xz record](../benchmarks/xz/own-decoder/README.md) shows.
- `.lzma`, lzip, the ZIP LZMA method and RPM's LZMA payloads decode with Zipper's own readers, and liblzma is only a test dependency, as the [lzip and `.lzma` record](../benchmarks/xz/lzip-lzma-zip/README.md) shows. The release build no longer links liblzma.

## M14. Encrypted archives

Zipper extracts encrypted archives when given the password, and never writes a byte decrypted with a wrong one.

Scope:

- A password option on the command line, a prompt when no password is given, and a password field in the library options.
- ZipCrypto and WinZip AES in ZIP. ZipCrypto is read only for compatibility, and the docs say it is weak.
- 7z AES-256, including archives whose header is encrypted.
- A wrong password fails before publish, from the password check or the CRC, and the error says the password is wrong.
- The password stays out of errors, logs and progress reports, and its memory is cleared after use.

Exit checks:

1. A corpus of encrypted archives from 7-Zip, WinZip and Info-ZIP extracts the same trees as 7-Zip.
2. A test for each format shows that a wrong password changes nothing in the destination.
3. An exhaustive test fails decryption at every point it can fail, for each format, and finds the destination unchanged and no staging folder left each time.
4. The adversarial review covers the key derivation and the authentication checks.

Done so far, by 8 November 2026:

- ZIP ZipCrypto and WinZip AES decrypt in process, with `--password`, the prompt on Unix and the library option, as the [ZIP reader reference](reference/zip-reader.md) describes. WinZip itself does not run on macOS, so the WinZip AES archives in `tests/data/encrypted-zip` come from 7-Zip instead. Info-ZIP writes only ZipCrypto, so it adds ZipCrypto archives alone.
- 7z AES-256 decrypts in process, in every folder layout 7-Zip writes and in encrypted headers, as [Encryption](reference/7z-reader.md#encryption) describes. The default engine no longer sends an encrypted 7z to 7-Zip. A wrong password fails through the header CRC or the data CRC, before publish. A key derivation power above 7-Zip's limit of 24 is refused before any work. `tests/encrypted_7z.rs` checks 26 archives from `7zz` against the trees 7-Zip gave, a wrong password and none on each, and every point where decryption can fail. A Kani proof covers the property parser under `[7z-aes-properties]`.
- M14 closed on 8 November 2026. WinZip does not run on macOS and Info-ZIP writes only ZipCrypto, so the WinZip AES archives of exit check 1 come from 7-Zip, which writes the same format.

## M15. Split, spanned and self-extracting archives

Archives split into volumes, or wrapped in an executable, extract in process.

Scope:

- Raw split volumes (`.001`, `.002` and on) for any format the readers support.
- 7z volumes, and ZIP split and spanned archives (`.z01` with `.zip`).
- Self-extracting ZIP and 7z executables, read from the archive that follows the executable. The ZIP reader refuses bytes before the first record today. It keeps refusing them when they are not a known executable stub.
- A missing or truncated volume fails before publish and names the volume.

Exit checks:

1. A corpus of split and self-extracting archives from 7-Zip, WinZip and Info-ZIP extracts the same trees as 7-Zip.
2. Hostile tests cover volumes that overlap, repeat or come in the wrong order.

Done so far, by 6 November 2026:

- Raw numbered volumes (`.001`, `.002` and on) read in process for every native format, as the split volumes section of `docs/reference/zip-reader.md` describes. Seven adversarial reviews shaped the rules for damaged sets. A damaged set is refused and names the volume at fault. A single compressed file whose last volume is full is refused, because a missing volume cannot be told apart.
- ZIP split sets from Info-ZIP `zip -s`, read in process with every disk number and offset placed inside its own volume, as [ZIP split sets](reference/zip-reader.md#zip-split-sets) describes. 7-Zip writes no `.z01` sets and WinZip was not available, so Info-ZIP is the only writer in the corpus.
- Self-extracting ZIP, with offsets adjusted by `zip -A` or not, as [self-extracting ZIP](reference/zip-reader.md#self-extracting-zip) describes. The rule for the stub changed. Recognizing a known stub was dropped, because stubs differ by writer and version. The stub must be a whole PE, ELF or Mach-O executable or a `#!` script. The archive must start exactly where the executable's own data ends, and no range its headers name may reach it. The stub may hold no other archive's signature. Three of five adversarial reviews found files that another reader opened as a different archive, and each is closed by one of these rules.
- Self-extracting 7z, found within 8 MiB after a PE, ELF or Mach-O stub, as [self-extracting 7z](reference/7z-reader.md#self-extracting-7z) describes. `7zz a -sfx` has no SFX module on macOS, so the tests build the stubs by hand.
- M15 closed on 8 November 2026. WinZip does not run on macOS, and 7-Zip on macOS has no self-extractor module. So the corpus of exit check 1 holds Info-ZIP split sets and self-extracting archives whose stubs the tests build. Every one extracts the tree 7-Zip extracts.

## M16. Package formats

Software packages extract in process on every operating system.

Scope:

- cpio in its binary, odc and newc forms.
- `ar`, Debian `.deb` and RPM, with their compressed payloads.
- Windows CAB with MSZIP, LZX and Quantum, and MSI through its compound file.
- `xar` and macOS `.pkg`.

Exit checks:

1. The M10 rows for these formats read native, with corpus archives from each format's own tools.
2. Each format has hostile tests for traversal, links, duplicate names and truncation.

Done so far, by 8 November 2026:

- cpio in its binary, odc and newc forms, `ar` in its System V, GNU and BSD forms, and Debian packages read in process, as `docs/reference/cpio-reader.md` and `docs/reference/ar-reader.md` describe. Six adversarial reviews found wrong-inode hard links, memory charges below real use and a second header pass that a rewritten archive could fool, each fixed with a test that failed first.
- xar archives and flat macOS packages read in process, as the [xar reader reference](reference/xar-reader.md) describes. Real archives from `xar`, `pkgbuild` and `productbuild` extract to the trees `7zz x -txar` and `xar -x` write.
- CAB files with stored, MSZIP and LZX folders read in process, with Zipper's own LZX decoder, as `docs/reference/cab-reader.md` describes. Every corpus CAB and 400 generated ones extract as 7-Zip extracts them. Quantum folders decode in process with Zipper's own decoder.
- CAB cabinet sets read in process, with folders and data blocks joined across cabinets. 172 generated sets extract to their source files and as `7zz x -tcab` extracts them, and a missing, cut, misnumbered or inconsistent set is refused with the cabinet named. The CAB row of the parity matrix reads native, with a `gcab` cabinet and a generated three-cabinet set in the parity corpus. No CAB written by Microsoft's own tools is in either corpus.
- MSI, MSP and MSM installers and other compound files, such as `.doc`, `.xls` and `.msg`, read in process, as `docs/reference/msi-reader.md` describes. An installer whose one CAB stream is a CAB extracts that CAB, as `7zz x` does. Every corpus file, 160 generated ones and 15 public installers and patches extract as 7-Zip extracts them.
- M16 closed on 8 November 2026. Every M10 row for these formats reads native. The CAB corpus comes from `gcab` and Zipper's own writer, because no permissively licensed CAB from a Microsoft tool was found.

## M17. Disc and disk images

Disc images and Windows images extract in process.

Scope:

- ISO 9660 with Joliet and Rock Ridge, and UDF, choosing the same tree 7-Zip chooses.
- WIM with XPRESS, LZX and LZMS, and the NTFS alternate streams it stores, written on Windows behind an option.
- VHD and VHDX disks holding FAT or NTFS.
- DMG with its UDIF compression. **Decision before work starts:** whether to read the HFS+ and APFS file systems inside, which is the largest single item in this roadmap, or to leave them to 7-Zip.

Exit checks:

1. The M10 rows for these formats read native or are recorded as decided against, with the reason.
2. Each image reader bounds memory by the image's directory size, not its data size.

Done so far, by 8 November 2026:

- ISO 9660 with Joliet, Rock Ridge and El Torito, and UDF 1.02 to 2.60 with metadata partitions, read in process in the tree 7-Zip chooses, as the [ISO reader reference](reference/iso-reader.md) describes. A UDF tree 7-Zip opens wins, then the last Joliet volume, then the primary volume. Every `hdiutil` image in the corpus and every generated disc extract as `7zz x` extracts them, modes included. `xorriso` and `mkisofs` were not installed, so no image from them was compared. The reader keeps one node for each entry and reads one folder at a time, so its memory follows the folders, not the data.
- WIM and ESD images read in process, with Zipper's own XPRESS and LZMS decoders and the LZX decoder in its WIM mode, as the [WIM reader reference](reference/wim-reader.md) describes. Every corpus WIM from `wimlib-imagex` and 36 generated ones of every method and chunk size extract as `7zz x` extracts them, with hard links and links. A split WIM goes to 7-Zip. Alternate data streams are refused, because the shared name check refuses `:`, and the Windows option that writes them as NTFS streams is not built yet.

## M18. RAR

Zipper extracts RAR 4 and RAR 5 archives, including encrypted and multi-volume ones.

Scope:

- Zipper's own decoder, decided on 4 November 2026. The `unrar` licence forbids using its source to build a RAR compatible archiver. 7-Zip's RAR code derives from it and carries the same restriction.
- Its authors read neither the `unrar` source nor 7-Zip's RAR code. They work from RARLAB's RAR 5 technote, which covers the container, and from libarchive's BSD licensed RAR readers. Code that follows libarchive keeps its BSD notice.
- RAR 5 and RAR 4 containers, solid archives, and the RAR filters.
- RAR AES encryption and RAR volumes, with the M14 and M15 contracts.

Exit checks:

1. A corpus written by WinRAR extracts the same trees as 7-Zip and `unrar`.
2. Hostile tests and the adversarial review cover the container and each filter.

## M19. Legacy formats

The remaining formats 7-Zip reads extract in process, or are recorded as dropped.

Scope:

- ARJ, LZH, CHM, NSIS installers and the other rows M10 still marks as backend.
- ZIP's Shrink and Implode methods. Every Shrink decoder needs a guard against an entry whose prefix is itself, which a partial clear can create.
- Usage data decides the order. A format nobody reports using can be dropped instead, with its row recorded in the M10 matrix.

Exit checks:

1. No row in the M10 matrix reads backend.

Decided, on 8 November 2026:

- Intel HEX stays with 7-Zip. 7-Zip finds it from the content alone and writes one file for each block of addresses, so it is not one stream to one file. It would need its own reader.

Done so far, by 8 November 2026:

- ZIP Shrink and Implode read in process, with Zipper's own decoders in `src/shrink.rs` and `src/implode.rs`. Test encoders build the streams, and Info-ZIP `unzip` and 7-Zip check each one first. The [legacy methods](reference/zip-reader.md#legacy-methods) section gives the rules. A partial clear can make a Shrink entry its own prefix. Such an entry is refused only when a code names it, as both tools do. Where `unzip` and 7-Zip disagree on which leaves a partial clear frees, Zipper reads as 7-Zip does, and no real PKZIP archive yet shows which one PKZIP matches.
- ZIP Reduce (methods 2 to 5) is dropped. 7-Zip does not read it and the macOS `unzip` is built without it, so no oracle could check a decoder. The M10 matrix has no Reduce row, because 7-Zip lists no such method.
- LZH archives of header levels 0 to 3 read in process, with the stored, `-lh4-` to `-lh7-` and folder methods. So do ARJ archives with methods 0 to 4. The [LZH reader reference](reference/lzh-reader.md) and the [ARJ reader reference](reference/arj-reader.md) describe them. Both use Zipper's own LZSS and Huffman decoder. LHa for UNIX archives, 30 archives from Lhasa's test suite and generated LZH and ARJ of every method extract as `7zz x` extracts them. Damaged streams are refused exactly when 7-Zip refuses them. No ARJ tool was installed, so every ARJ came from Zipper's own test writer.
- Microsoft SZDD files, lzma86 files, PPMd `.pmd` files and Base64 `.b64` files read in process, as the [single compressed file reader reference](reference/stream-reader.md) describes. Generated files of each extract to the tree `7zz x` extracts, and files with one damaged byte are refused exactly when 7-Zip refuses them. KWAJ is not read, because 7-Zip does not read it.

## M20. Zipper without 7-Zip

The default build extracts every format in the M10 matrix with no 7-Zip installed.

Scope:

- The 7-Zip backend becomes an optional build feature, then is removed when no row needs it.
- The Windows package ships without 7-Zip.
- `docs/usage.md`, the reader references and the Windows notes describe the result.

Exit checks:

1. The release checks run the full M10 corpus on all three operating systems, on machines with no 7-Zip installed.
2. The scoreboard compares every format with 7-Zip, with CPU time and memory.

## M22. Zipper's own ZIP directory reader

Zipper reads the ZIP central directory with its own parser, so the `zip` crate leaves the release build.

The native ZIP reader already checks the end record for ambiguity, every local header, ZIP64 fields and data descriptors itself, with Kani proofs. It still lists entries through the `zip` crate's `ZipArchive`, which alone brings `indexmap`, `memchr`, `nix` and `typed-path` into the release build. An audit on 5 November 2026 found no other dependency to remove outside M21, which removes liblzma.

Scope:

- The end record, the ZIP64 end record and locator, the central directory entries, their extra fields and names, read by Zipper's own code within the documented limits.
- The `zip` crate stays a test dependency for writing archives.

Exit checks:

1. A differential test lists every archive in the ZIP corpora and the real-world set with the new parser and with the `zip` crate, and finds the same entries or the same refusal.
2. Kani proves the central directory entry, its extra fields and the ZIP64 records never panic or overflow, up to a bound.
3. Every test in `tests/hostile_zip.rs` and `tests/zip_fuzz.rs` passes, and an adversarial review finds no layout the new parser accepts that the old one refused.
4. `cargo tree -e normal` shows no `zip` crate, and the scoreboard shows no slower listing.

Done so far, by 8 November 2026:

- The reader in `src/zip_directory.rs` lists every ZIP entry in the release build, for `zipper list`, both native engines, encrypted entries, split sets and self-extractors. The `zip` crate is a dev-dependency only. Its removal took `zip`, `indexmap`, `hashbrown`, `equivalent`, `memchr`, `nix` and `typed-path` out of `cargo tree -e normal`.
- A differential test agrees with the `zip` crate on the corpora, the encrypted set, hand-built shapes and damaged archives, or both refuse. It also compares what `native_zip::list` gives, and lists the corpus split set against the same tree in one piece. Its ignored real-world case listed all 95 ZIPs of the real-world set the same.
- The ten Kani proofs of `[zip-directory]`, run by `cargo xtask verify-sync zip-directory`.
- Every test in `tests/hostile_zip.rs` and `tests/zip_fuzz.rs` passes.
- An adversarial review of the wired path ran about 6,850 archives on `zipper list` and both engines. It confirmed no layout that the new path accepts and the `zip` crate refuses, and no listing that differs. Peak memory of `zipper list` stayed at or below the old build's on archives at the entry and metadata limits.
- A run of 5,000 damaged copies of each archive found one empty archive of ZIP64 records without sentinels that the new reader listed and the old one refused. The reader now refuses it, as the old reader and 7-Zip do, and a built case covers it.
- One layout stays accepted on purpose. A ZIP64 end record without sentinels after a directory with entries lists its real entries, as the `zip` crate and Python list them, though the old reader refused it. Refusing it would risk archives that real writers produce.
- Listing takes the same time as with the `zip` crate, as the [listing record](../benchmarks/native-zip/own-directory-listing/README.md) shows, and the `[zip-directory]` proofs passed on the final code. M22 closed on 8 November 2026.

## M23. The LZMA decoder as its own crate

Zipper's LZMA, LZMA2 and xz decoder is published on crates.io as a crate of its own, so other Rust programs can read these formats without liblzma.

The [tree-preload record](../benchmarks/lzma/tree-preload/README.md) shows the decoder taking 350.0 ms on the probe stream against 398.4 ms for liblzma in the same runs. That is one Mac and one stream. The crate is worth publishing only if the speed holds on other machines and every use M21 covers.

Scope:

- `src/lzma.rs` and `src/xz_filters.rs` move into a crate in this workspace, with Zipper depending on it. Its proofs, tests and coverage entries stay in this repository's gate.
- A public API of `Read` adapters for raw LZMA, LZMA2 and xz, with a memory limit, documented as a public contract.
- liblzma stays a test dependency of the crate, as the oracle for its differential tests.

Exit checks:

1. M21 is done, so Zipper itself reads every LZMA format with the crate and the release build has no liblzma.
2. A benchmark record compares the crate with liblzma on Linux x86-64, macOS arm64 and Windows, on the probe stream and on xz files of Silesia, with ranges apart or the verdict stated as a tie.
3. A fuzzing run of the public API on every format finds no panic, hang or memory beyond the limit, and its corpus is kept.
4. `cargo publish --dry-run` passes, and the crate's README states what it checks and what it does not.

## Close a milestone

1. Confirm every exit check with its gate step, test or record.
2. Run every owed slow sync that `cargo xtask verify --owed` lists.
3. Set the milestone's status to Done in this page.
4. Run the `zipper-reflect` skill and commit what it encodes.
