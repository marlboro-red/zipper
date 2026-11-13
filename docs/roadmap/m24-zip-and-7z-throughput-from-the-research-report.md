# M24. ZIP and 7z throughput from the research report

Zipper keeps every idea from the 10 November 2026 research report on ZIP and 7z extraction that a timing shows to be faster, and records the rest.

The report ranked ideas by expected gain against size of change. It found the LZMA kernel's per-bit chain already at 7-Zip's 6 cycles, and put the larger gains in BCJ2 input, Deflate64, short-distance matches, filtered folders and file creation.

Scope:

- Each idea is one experiment, timed with interleaved runs against main on one quiet machine and recorded in `benchmarks/`, kept or reverted.
- No idea drops a safety check, a budget or a formal check. Parsing and concurrency changes get an adversarial review.

Exit checks:

1. Each idea in the report has a record, or a sentence here saying why it was not tried.
2. Every kept change passes `cargo xtask gate full`, with its proof sections synced.

Done so far, by 11 November 2026:

- [Buffered BCJ2 branch streams](../../benchmarks/native-7z/bcj2-buffered-branches/README.md). A BCJ2 7z of Silesia extracts 2.25 times faster at one thread. The review found a Deflate64 coder charged 64 KiB of its 142,368-byte state, now fixed.
- [One Deflate64 decoder per ZIP entry](../../benchmarks/native-zip/deflate64-one-inflater/README.md). A Deflate64 ZIP of Silesia extracts 1.83 times faster at one thread, and faster than 7-Zip.
- [Each 7z output folder created once](../../benchmarks/native-7z/create-folders-once/README.md). Kept as a cleanup, with no measured change.
- [The doubling copy for short-distance LZMA matches](../../benchmarks/lzma/repeating-copy/README.md). A 7z of 256 MiB of zeros extracts 7.3 times faster, and faster than 7-Zip.
- [Parallel LZMA2 segments behind 7z filters](../../benchmarks/native-7z/filtered-lzma2-segments/README.md). An ARM64-filtered 7z extracts 1.29 times faster at four threads.
- [A macOS worker cap for ZIPs of mostly small files](../../benchmarks/native-zip/small-files-cap/README.md). The dependency sources and the mixed tree extract about 10% faster at default threads. The same sweep found no wall-time gain for a cap on non-solid 7z, so 7z keeps its worker count.

- [The BCJ2 main stream copied a slice at a time](../../benchmarks/native-7z/bcj2-slice-scan/README.md). A BCJ2 7z of Silesia extracts a further 1.38 times faster at one thread. On one machine, [before and after the research report's changes](../../benchmarks/research-report/before-after/README.md) times BCJ2 2.85 times faster than before this milestone.
- [Zipper against the Go ZIP extractors pzip and fastzip](../../benchmarks/native-zip/go-zip-tools/README.md), timed as a check on the ZIP reader. Zipper is about 2.5 times faster on Silesia, and its content-only mode beats both on small files.

- [A writer pool for solid 7z folders of small files](../../benchmarks/native-7z/solid-writer-pool/README.md). The solid dependency sources extract 1.60 times faster at default threads, and a TLA+ model of the hand-off holds 8 rules.

- [ZIP workers read the archive without a lock](../../benchmarks/native-zip/lock-free-reads/README.md). 512 MiB of stored entries extract 1.18 times faster at 64 threads.

- [BZip2 blocks decoded in parallel](../../benchmarks/bzip2/block-parallel/README.md). Silesia BZip2 extracts 5.0 times faster as 7z and 7.9 times faster as tar at default threads, and 4.2 times faster than 7-Zip on 7z.

- [Zipper's own BZip2 decoder at one thread and in ZIP](../../benchmarks/bzip2/own-decoder-one-thread/README.md). BZip2 now beats 7-Zip at every thread count.

- [One Deflate decoder per ZIP worker](../../benchmarks/native-zip/deflate-decoder-per-worker/README.md), tried and reverted. Reusing each worker's decoder across its entries gave no measurable gain on the dependency sources or on flat tiny files, at default threads or at one thread.

[Before and after the research report's changes](../../benchmarks/research-report/before-after/README.md) times main before and after all of them on one machine, with 7-Zip in every row.

Declined, with the evidence:

- **Detecting ZIP folder merges from `mkdir` results instead of a `stat` for each folder entry.** The idea removes one `stat` per folder entry and keeps every `mkdir`. In the [write path study](../../benchmarks/write-path/study/README.md) traces of the dependency sources ZIP, the 1,000 `stat` calls took 1.75 to 1.83 traced ms in all six runs, at one thread and at default threads. The 998 `mkdir` calls took 31.6 to 36.7 ms, and the three default-thread runs spanned 211 to 273 ms of wall time. The tracer adds two clock reads per call, so 1.8 ms bounds the saving from above. That is under 1% of a run, and the study's untraced timing of the same ZIP at default threads ranged from 227.8 to 245.0 ms. A failed `mkdir` also does not name the spelling that made the folder, so the merge check that four readers share would still need a `stat` after each one. [Making the folders in parallel](../../benchmarks/native-zip/parallel-folders/README.md) was tried and reverted on 11 November, since APFS makes folders no faster on more threads.
- **The LZMA literal path.** The count of mispredicted branches that this idea waited for is no longer needed to place the gap. The [kernel study](../../benchmarks/lzma/7zip-study-kernel/README.md) found that 7-Zip's hand-written arm64 loop runs no shorter a chain between bits than Zipper's, and differs in retiring 3.00 G instructions against 4.80 G. The literal path already decodes its bits without branches, as the [hot loop record](../../benchmarks/lzma/hot-loop/README.md) shows. It loads both children of each bit early, as the [literal preload record](../../benchmarks/lzma/literal-preload/README.md) shows, and uses 7-Zip's 1,024-entry literal coder. Removing instructions in safe Rust then stopped paying. In the [unchecked tables record](../../benchmarks/lzma/unchecked-tables/README.md), 3.1% fewer instructions made solid Silesia 0.4% faster, and in the kernel study 4% fewer made the decoder 19.5% slower through register spills. What remains is 7-Zip's 1.335 s against Zipper's 1.478 s at one thread, a gap only hand-written assembly is shown to close. Zipper stays pure Rust by the user's decision of 8 November 2026, recorded in [M21](m21-zippers-own-lzma-decoder.md), so the gap is an accepted limit.

## Closed on 14 November 2026

- Exit check 1 holds. Each idea in the report has a record above, or a reason in the declined list.
- Exit check 2 holds. `cargo xtask ship` moved main only after CI's full gate passed on each kept change, among them Zipper's own BZip2 decoder in `9c898aa`. `cargo xtask verify` passes and `cargo xtask verify --owed` lists no owed section.
