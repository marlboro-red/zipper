# LZ4 block decoder at the reference speed

**Kept five changes. One-thread extraction of a Silesia `.tar.lz4` takes 114.8 ms against 351.2 ms before them, and 1.09 times bsdtar's 105.3 ms, on the second machine. bsdtar runs `lz4 -d` as a second process beside its tar reader, so its time overlaps two processes. One xxHash32 rewrite was dropped, and LLVM's vectorizing of the checksum lanes stays the main cost left.** One Apple M2 Max, shared with other agents.

## Question

[M27](../../../docs/roadmap/m27-lz4-and-compress-at-the-reference-speed.md) found Zipper 3.3 times slower than bsdtar on a Silesia `.tar.lz4` at one thread. What limits `src/lz4.rs`, and can safe Rust close the gap without dropping a check? The metric is wall time of `zipper extract --threads 1` against bsdtar, with retired instructions and cycles for each stage and peak memory beside them.

## Setup

- The second machine, an Apple M2 Max with 12 cores. Other agents built and ran Kani on it, so stages were judged by counters and only the final pair was timed.
- The archive is `tar-lz4.tar.lz4` from `scripts/codec_scoreboard.py build`. It is `lz4 -9` of the ustar Silesia tar, 78,001,939 bytes, with independent 4 MiB blocks and a content checksum.
- [`count.sh`](count.sh) builds a tree, counts `zipper --threads 1 test` under `/usr/bin/time -l`, and compares `extract` with bsdtar byte for byte. Raw counts are in [`data/counters.txt`](data/counters.txt).
- Profiles came from `xctrace` Time Profiler on `zipper test`, outside any timed run.
- The timed pair is `a5cd62f` against `17b22c5`, with `scripts/compare_zip_readers.py`, nine rounds after a warmup, interleaved. The JSON is in [`data/extract.json`](data/extract.json) and [`data/test.json`](data/test.json). One rustc used a full core during both runs.
- [`mem2.sh`](mem2.sh) takes peak memory of three builds on the adversary reviews' hostile frames, interleaved. [`rss_check.sh`](rss_check.sh) is the check the memory plant ran.

## Results

The base profile put 73% of the time in `decode_block`, which pushed each match byte onto a `Vec`. `Xxh32::update` held 13% and `memmove` 10%. The tar sniff decodes the first block twice, about 5%.

| Commit | Change | Instructions | Cycles | Peak memory |
| --- | --- | ---: | ---: | ---: |
| `a5cd62f` | before | 4,380 M | 1,180 to 1,210 M | 21.9 to 26.2 MB |
| `caa3be5` | literals and matches copied 16 bytes at a time | 1,647 M | 479 to 513 M | 18.5 to 22.2 MB |
| `daa8dfa` | block and window buffers kept between blocks | 1,498 M | 472 to 504 M | 14.7 to 16.2 MB |
| `f629909` | short sequences read from one fixed slice | 1,099 M | 458 to 498 M | 14.8 MB |
| `dc59884` | content hash run one stripe per sequence inside the decode loop | 1,178 M | 384 to 402 M | 14.7 MB |
| `1239695` | window grown as each block needs it | 1,212 M | 379 to 394 M | 14.7 MB |

`lz4 -d` alone retires 657 M instructions in 343 to 357 M cycles. bsdtar's own process retires 532 to 569 M beside it.

With the hash switched off, `daa8dfa` took 321 M cycles against 491 M. The checksum cost a third of the decode, at 1.3 bytes a cycle, because LLVM folds the four lanes into one NEON chain bounded by its latency. Holding the lanes in locals, 64-bit arithmetic and 64-bit loads all compiled back to that chain. Interleaving the hash with decoding moved it into idle issue slots. Without the short-sequence path the interleaved build took 434 to 459 M cycles, so that path stays although it saved few cycles alone.

| Timed run, medians of nine | Old | New | bsdtar |
| --- | ---: | ---: | ---: |
| `extract` | 351.2 ms, 340 to 358 | 114.8 ms, 114 to 119 | 105.3 ms, 100 to 110 |
| `test` | 353.1 ms, 342 to 426 | 113.2 ms, 111 to 120 | not run |
| Peak memory in `extract` | 28.6 MiB | 22.8 MiB | 33.6 MiB |

Peak memory on hostile frames is in [`data/memory.txt`](data/memory.txt). Two reviews each found a regression. The first build zero-filled a whole 4 MiB window before decoding, and the second copied a linked window into a fresh one while the old one lived. A linked frame of a raw block and an expanding block peaked at 19.8 MB against 15.5 MB before. The final build peaks at 15.6 MB there and at 14.5 MB against 14.5 MB on `lz4 -B7 -BD` of random bytes. The other hostile frames stay within 0.12 MB of the old code, and frames that decode to 4 MiB peak about 3 MB lower. A plant that restored the copy made `rss_check.sh` report both repros as regressed.

## Decision

Keep all five commits. The scoreboard row moves from 3.3 times to 1.09 times bsdtar. Drop the xxHash32 rewrites, which compiled to the same code.

## Limits

- One machine, shared with other agents. One rustc ran during the timed rounds.
- bsdtar decodes in a second process, so its wall time hides its decode behind its tar writes. Zipper decodes and unpacks on one thread.
- The timed build is `17b22c5`. The final head changes only how a linked window grows, which independent 4 MiB blocks never reach after the first block.
- Linked frames and `compress` were not timed.

## Reproduce

```sh
python3 scripts/codec_scoreboard.py build <target>/codecs
python3 scripts/compare_zip_readers.py <target>/codecs/tar-lz4.tar.lz4 target/bench-corpora/silesia/silesia \
  --archive-format tar --threads 1 --runs 9 \
  --reader old native <target>/build-old/release/zipper \
  --reader new native <target>/build-new/release/zipper \
  --reader bsdtar bsdtar /usr/bin/bsdtar
```

Add `--operation test` and drop the bsdtar reader for the decode-only rows. Run `SCRATCH=<folder> zsh mem2.sh old=<old binary> new=<new binary>` after placing the hostile frames under `<folder>/out` for the memory table.
