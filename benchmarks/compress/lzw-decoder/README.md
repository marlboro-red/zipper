# `compress` decoder ahead of 7-Zip

**Kept three changes and one fix from review. One-thread extraction of a Silesia `.tar.Z` takes 269.8 ms against 1,218.1 ms before them, and 0.64 times 7-Zip 26.06's 419.0 ms, with the ranges apart, on an idle second machine. 7-Zip decodes in one process and reads the tar in a second one. Peak memory in `extract` rises from 10.5 to 12.6 MiB, because the code table grows from 192 KiB to 1 MiB.** One Apple M2 Max.

## Question

[M27](../../../docs/roadmap/m27-lz4-and-compress-at-the-reference-speed.md) found Zipper 2.7 times slower than 7-Zip on a Silesia `.tar.Z` at one thread. What limits `src/lzw.rs`, and can safe Rust close the gap without dropping a check? The metric is wall time of `zipper extract --threads 1` against 7-Zip's tar pipeline, with retired instructions and cycles for each stage and peak memory beside them.

## Setup

- The second machine, an Apple M2 Max with 12 cores. Other agents built on it while the stages were counted, so stages were judged by counters. The timed runs found no busy process.
- The counted archive comes from [`mkcorpus.py`](mkcorpus.py). It is `compress -c` of the ustar Silesia tar, 88,607,181 bytes, with macOS `compress` at its default of 16 bits.
- The timed archive is the same `compress -c` of the M31 ustar `silesia.tar`, 88,599,135 bytes.
- [`count.sh`](count.sh) builds a tree, counts `zipper --threads 1 test` three times under `/usr/bin/time -l`, and compares `extract` with the plain tar byte for byte. Raw counts and the sampled profiles are in [`data/counters.txt`](data/counters.txt).
- Profiles came from `sample` on `zipper test`, outside any timed run, on builds that mark the decoder's functions `#[inline(never)]`.
- The coordinator timed `25f8466b` against the branch head before its rebase onto `3b324db6`, whose `src/lzw.rs` is the one at `85138b5a`. The tool was `scripts/compare_zip_readers.py`, nine rounds after a warmup, interleaved, with 7-Zip as the `7zip-tar` reader. The JSON is in [`data/extract.json`](data/extract.json) and [`data/test.json`](data/test.json).
- [`hostile.py`](hostile.py) writes the hostile streams, and [`mem.sh`](mem.sh) takes peak memory of two builds on them, interleaved.

## Results

The base profile put 68% of the samples in the code-table walk. It pushed one byte onto a `Vec` for each prefix step and then reversed the string. Reading each code one bit at a time took 27%, the reversal 3.5% and the output copy 1%. The tar layer retires 66 M instructions on the plain tar, so it is not the limit.

| Commit | Change | Instructions | Cycles | Peak memory |
| --- | --- | ---: | ---: | ---: |
| `25f8466b` | before | 16.29 G | 4.55 to 4.65 G | 4.8 MB |
| `38172ad8` | each table entry holds the last piece of its string, up to 8 bytes, with the string's length and the code of the rest, and each string is written forward one 8-byte store per piece | 12.87 G | 2.19 to 2.22 G | 5.6 MB |
| `0749f4a3` | the 8 codes of a group come from one 16-byte load, each a shift and a mask | 4.33 G | 0.89 to 0.93 G | 5.6 MB |
| `b26d60fa` | a full table defines no entry for the next free code | 4.33 G | 0.90 to 0.93 G | 5.6 MB |

`7zz x -so -mmt=1` alone retires 2.73 G instructions in 1.33 G cycles on the same archive. Its tar stage retires another 0.30 G in 0.14 G cycles in the second process.

A first build of `38172ad8` kept the piece as a `[u8; 8]` and stored the new byte by index. It took 3.87 to 3.89 G cycles, and `define` held 41% of the samples. The narrow store into a piece that was then loaded whole could not be forwarded. Building the piece with a shift in a `u128` cut the cycles to 2.19 G.

The adversary review of `0749f4a3` found that a 9-bit stream, which reads 10-bit codes once its table is full, could name code 512 and define an entry past the table's end. `free` then grew past 65,536. One crafted stream panicked on a group of 18-bit codes, others decoded where the old code refused them, and a debug build overflowed the 16-bit string length. `b26d60fa` restores the old behaviour, and all twelve of the review's files are accepted or refused as `25f8466b` does them.

| Timed run, medians of nine | Old | New | 7-Zip 26.06 |
| --- | ---: | ---: | ---: |
| `extract` | 1,218.1 ms, 1,206 to 1,220 | 269.8 ms, 264 to 271 | 419.0 ms, 405 to 423 |
| CPU time in `extract` | 1,235.0 ms | 286.9 ms | 442.4 ms |
| `test` | 1,215.5 ms, 1,215 to 1,217 | 269.0 ms, 268 to 269 | not run |
| Peak memory in `extract` | 10.5 MiB | 12.6 MiB | 3.7 MiB |
| Peak memory in `test` | 4.6 MiB | 5.3 MiB | not run |

Peak memory on hostile streams is in [`data/memory.txt`](data/memory.txt). The new code peaks 0.7 to 0.8 MB higher on every stream that fills its table. A stream of 400,000 clear codes and a stream refused at its start peak within 0.05 MB of the old code. The table is fixed at 1 MiB, so no stream can raise it further.

## Decision

Keep all four commits. The `compress` row moves from 2.7 times 7-Zip's time to 0.64 times.

## Limits

- One machine. The stage counts ran beside other agents' builds, and only the final pair was timed.
- 7-Zip decodes and reads the tar in two processes joined by a pipe, so its wall time overlaps the two. Zipper decodes and unpacks on one thread.
- The stage counts use an archive 8,046 bytes larger than the timed one, built from a tar that Python's `tarfile` wrote separately.
- Zipper still retires 1.6 times 7-Zip's instructions on the decode. No further change was tried.
- Only macOS `compress` output was timed. Streams with many clear codes and small widths were checked for memory, not time.

## Reproduce

```sh
python3 mkcorpus.py <target>/codecs <target>/bench-corpora/silesia/silesia
python3 scripts/compare_zip_readers.py <target>/codecs/tar-compress.tar.Z <target>/bench-corpora/silesia/silesia \
  --archive-format tar --threads 1 --runs 9 \
  --reader old native <target>/build-old/release/zipper \
  --reader new native <target>/build-new/release/zipper \
  --reader 7zip 7zip-tar /opt/homebrew/bin/7zz
```

Add `--operation test` and drop the 7-Zip reader for the decode-only rows. For the counters, run `SCRATCH=<folder> zsh count.sh <tree> <label>` with the archive in `<folder>/out`. For the memory table, run `python3 hostile.py <folder>/hostile`, then `SCRATCH=<folder> zsh mem.sh <label> ...` with each binary at `<folder>/out/zipper-<label>`.
