# M30. gzip end to end at libdeflate's speed

`zipper extract` on a single `.gz` file takes no longer than `libdeflate-gunzip` writing the same file, with the staging, cancel and memory rules kept.

Zipper's Deflate decoder alone already takes fewer cycles than libdeflate 1.26 on an Apple M2 Max. The whole workflow does not. On 12 November 2026 a 212 MB file gzipped at level 6 took 230.9 ms to extract with Zipper, 220.2 ms with `libdeflate-gunzip` and 256.1 ms with `gzip -d`, 15 rounds in turn on that machine. So the gap of about 5% sits around the decoder.

Scope:

- Split Zipper's time on that file into decode, CRC-32, reads, writes, staging and start-up, outside timed runs, before any change.
- Measure streaming. That means read and write sizes, copies between buffers and system calls, against libdeflate's one call over the whole input. Any larger buffer has a stated memory bound.
- Measure staging. That means the private folder, the rename into place, timestamps, the format sniff and start-up, without weakening publish-nothing-on-failure.
- Compare CRC-32 code. That means `crc32fast` against libdeflate's folding CRC and the ARMv8 `crc32` instructions, and whether it can run on data still in cache.

Exit checks:

1. A record times `zipper extract` against `libdeflate-gunzip` and `gzip -d` on that file on the second machine, with Zipper's median at or below libdeflate's, or the verdict naming the cost left and why it is kept.
2. `.tar.gz`, ZIP and small-file rows of the scoreboard show no workload slower beyond the spread.
3. The `[inflate]` and `[deflate-framing]` checks, and `[overwrite]` if staging changes, pass on the final code, and a planted break shows each new check can fail.

Met on 13 November 2026:

1. The [gzip file record](../../benchmarks/deflate/gzip-file/README.md), from `87c7dbe` and `1053226`, times one 212 MB gzip file on the second machine over 15 rounds. Zipper takes 209.5 ms against 218.7 ms for `libdeflate-gunzip` and 255.3 ms for `gzip`, with the ranges apart. The kept changes are `e87a20a`, `eaf5775`, `18926c8`, `48a913c` and `f98475b`. One machine and one corpus.
2. The record's rows that must not get slower show no Silesia, random or dependency-source `.tar.gz` row slower beyond its range. The ZIP rows do not pass through the changed code and did not move.
3. `dc7304b` synced `[deflate-framing]` on the final code, and `f247c9e` again after the disk image removal. `[inflate]` is in step, and staging did not change. The record lists eleven planted breaks, each caught by its test, and an adversarial review.
