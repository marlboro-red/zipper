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

[Before and after the research report's changes](../../benchmarks/research-report/before-after/README.md) times main before and after all of them on one machine, with 7-Zip in every row.

Still to try, from the report and the read of pzip and fastzip:

- One Deflate decoder per ZIP worker. M25 replaced the Deflate decoder with Zipper's own, as the [own inflate record](../../benchmarks/native-zip/own-inflate/README.md) shows, and no record or decision closes this idea yet.
- Detecting ZIP folder merges from `mkdir` results instead of a `stat` for each folder entry. [Making the folders in parallel](../../benchmarks/native-zip/parallel-folders/README.md) was tried and reverted, since APFS makes folders no faster on more threads.
- The LZMA literal path, only after a count of mispredicted branches shows where the remaining gap to 7-Zip sits.

Exit check 2 holds for every kept change so far, since `cargo xtask ship` moves main only after CI's full gate passes. Exit check 1 waits on the three ideas above.
