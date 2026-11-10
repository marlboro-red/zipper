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
- The doubling copy for short-distance LZMA matches and parallel LZMA2 segments behind 7z filters are timed and kept, and ship next.

Still to try, from the report:

- A worker cap for ZIPs and non-solid 7z that are mostly file creation, on macOS, after a thread sweep.
- A writer pool for solid 7z folders of small files.
- BCJ2 decoding its main stream a slice at a time.
- One Deflate decoder per ZIP worker, and reads of the archive without a lock.
- The LZMA literal path, only after a count of mispredicted branches shows where the remaining gap to 7-Zip sits.
