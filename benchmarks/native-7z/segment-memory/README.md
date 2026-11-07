# Parallel LZMA2 segments in 146 MiB

**Kept. The front segment now decodes straight to the writer, and the other segments read their packed bytes from the file. Peak memory on solid Silesia at default threads fell from 320 to 146 MiB, below 7-Zip's 257 MiB. Extraction got 0.7% faster on both solid Silesia archives, with ranges apart.** Other archives are unchanged. One Apple M2 Max.

## Question

The [parallel segments record](../lzma2-segments/README.md) kept a 1.59 times speed-up on solid LZMA2 folders and raised peak memory from 38 to 338 MiB. Roadmap M21 asks for peak memory no higher than 7-Zip's at default threads. A heap count found the peak in two decoded output buffers of 128 and 74 MiB, packed copies of 47 MiB that `push(0)` doubled in capacity, and two 32 MiB windows. The change decodes the segment at the front of the output on the folder's own worker, straight into its buffer. Threads behind it read packed bytes from the file, and they start only while their segments and the largest front dictionary fit a 224 MiB budget. Does memory fall below 7-Zip's without losing speed? The metric is peak resident memory and median wall time of `zipper extract`, with 7-Zip in the same rotation.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0, release builds. macOS background services were active, as in the [kernel record](../../lzma/7zip-study-kernel/README.md).
- `before` is main at `fc15e12`, binary SHA-256 prefix `fd005fc8ea80d058`. `after` is the change at `4a02fd5`, prefix `db5d7377b39de0e1`. 7-Zip 26.05 for arm64 ran in the same rotation.
- `silesia.7z`, `dependencies.7z` and `dependencies-nonsolid.7z` come from the [7z switch record](../own-decoder-shipped/README.md). Also `silesia-mmt4.7z`, 49,597,380 bytes, written by `7zz a -m0=LZMA2 -mmt=4 -ms=on`.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 5 --allow-busy` ran each archive after one warmup round, in interleaved order, at one thread and at each tool's default thread count. Every run checks each extracted byte.
- A TLA+ model of the segment reader checks its nine rules in three configurations, including the new memory bound and `OnlyTheFrontStreams`. Every planted break of the model and of the Rust code was caught. An adversarial review found one failure, a freed front dictionary that the macOS allocator kept resident, and it was fixed with a test that failed first.

## Results

Median wall time in seconds, with the range over seven runs, and peak resident memory. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | 7-Zip |
| --- | --- | --- | --- | --- |
| `silesia.7z` | 1 | 1.539 (1.537 to 1.544), 38 MiB | 1.539 (1.538 to 1.543), 39 MiB | 1.335, 36 MiB |
| `silesia.7z` | default | 1.013 (1.010 to 1.017), 320 MiB | 1.006 (1.004 to 1.009), 146 MiB | 0.885, 257 MiB |
| `silesia-mmt4.7z` | default | 1.014 (1.012 to 1.015), 320 MiB | 1.006 (1.006 to 1.008), 145 MiB | 0.884, 257 MiB |
| `dependencies.7z` | default | 0.619 (0.616 to 0.625), 41 MiB | 0.631 (0.615 to 0.643), 41 MiB | 0.618, 76 MiB |
| `dependencies-nonsolid.7z` | default | 0.368 (0.356 to 0.385), 34 MiB | 0.363 (0.357 to 0.389), 37 MiB | 0.808, 7 MiB |

CPU time held, at 1.64 and 1.63 s on `silesia.7z` at default threads.

## Decision

Kept. Peak memory on both solid Silesia archives fell by more than half and is now 111 MiB below 7-Zip's, which meets the memory half of M21's exit check 4. Both Silesia archives at default threads are slightly faster with ranges apart, and the other cells overlap. Decoding the front on the writer's thread cost nothing measurable.

## Limits

- One machine and one session, with background services active. Linux and Windows were not measured.
- 7-Zip is still about 13% faster on solid Silesia at one thread, which is the speed half of the same exit check. `before` and `after` here predate the kernel changes of the [kernel record](../../lzma/7zip-study-kernel/README.md).
- The heap count measures requested memory, not resident pages, so it cannot see an allocator that keeps freed memory. The resident peaks above come from `/usr/bin/time -l`.

## Reproduce

Build release binaries at `fc15e12` and `4a02fd5`, and write `silesia-mmt4.7z` with `7zz a -m0=LZMA2 -mmt=4 -ms=on` from `target/bench-corpora/silesia`. Then run `scripts/compare_zip_readers.py` on each archive with `--archive-format 7z --runs 7 --cooldown 5`, the two binaries and `--reader 7zip 7zip "$(command -v 7zz)"`. Add `--reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1` for one thread.
