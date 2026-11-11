# Zipper's own Zstandard decoder against libzstd

**Kept, and not yet timed. Zipper's safe Rust decoder retires 1.01 to 1.04 times the cycles of libzstd 1.5.7 on four large streams, and 1.09 times on a file of 3,983 small frames. It retires 19 to 25% more instructions, with peak memory within 0.8 MB. This record counts cycles and instructions only, and the extraction timing that decides shipping is still owed.** One Apple M5, shared with other jobs.

## Question

M25 asks for a release build that compiles no C. The `zstd` feature linked libzstd through the `zstd` crate for ZIP method 93, `.zst` files, tar and RPM payloads and SquashFS blocks. Can a decoder in safe Rust replace it without costing decode work? The metric is retired cycles of a decode loop in one process, Zipper's decoder against libzstd on the same bytes, with retired instructions and peak memory beside it. A study of 7-Zip before [M25](../../../docs/roadmap/m25-a-release-build-without-c.md) found ZIP Zstandard extraction level with 7-Zip in cycles, 0.55 G against 0.56 G, so libzstd is the bar.

## Setup

- Apple M5 with 10 cores and 32 GB, macOS 26.7.1. Other agents built and ran Kani on the machine during every run, so no wall time was taken.
- The probe is the ignored test `decode_loop_for_counters` in `src/zstandard/tests.rs`. It reads a `.zst` file into memory and decodes it three times through a 128 KiB buffer, with `zstd::stream::read::Decoder` from the `zstd` crate 0.14.3, which holds libzstd 1.5.7, or with `ZstdReader`.
- [`count.sh`](count.sh) runs the probe under `/usr/bin/time -l`, five times for each decoder in turn, and prints the retired instructions, the cycles and the peak memory footprint of each run.
- Four streams. `silesia.tar`, 211,957,760 bytes, and a tar of the dependency sources, 81,319,936 bytes, each written by `zstd -3` and by `zstd -19 -T0`.
- Two more streams for memory. `silesia.tar` twice in a row under `zstd -3 --long=27`, which fills the 128 MiB window limit, and the 3,983 dependency source files as 3,983 frames in one file.
- Every stage passed the differential tests against libzstd before it was counted.

## Results

Counts are in millions, for three decodes of the stream. Each cell is instructions, then the median of the cycles. The first two stages were counted once and the rest five times. libzstd retired 6,428 to 6,432 M instructions on Silesia at level 3 in every run. Its cycles moved between 1,536 M and 1,578 M with the load. So each stage is shown with the libzstd cycles of its own run. Raw runs of the last row are in [`counters.txt`](counters.txt).

| Stage | Silesia `-3` | Silesia `-19` | Dependencies `-3` | Dependencies `-19` |
| --- | --- | --- | --- | --- |
| First working decoder | 8,387 and 1,986, libzstd 1,578 | 8,010 and 1,956, libzstd 1,498 | 1,879 and 461, libzstd 356 | 1,531 and 382, libzstd 298 |
| Sequence state in locals, table entries packed in one word | 7,726 and 1,877, libzstd 1,558 | 7,325 and 1,910, libzstd 1,492 | 1,668 and 441, libzstd 356 | not counted |
| Rare copies behind cold calls | 7,904 and 1,667, libzstd 1,557 | 7,566 and 1,697, libzstd 1,507 | 1,728 and 391, libzstd 355 | 1,391 and 339, libzstd 301 |
| Every match copy inline, 32 bytes a step | 7,861 and 1,532, libzstd 1,536 | 7,369 and 1,559, libzstd 1,601 | 1,705 and 349, libzstd 355 | 1,369 and 304, libzstd 300 |
| The shipped decoder, after the review's fixes | 7,882 and 1,554, libzstd 1,540 | 7,391 and 1,555, libzstd 1,490 | 1,713 and 361, libzstd 356 | 1,376 and 308, libzstd 301 |

What each stage changed.

1. The first decoder followed libzstd's structure. Its four-stream Huffman loops, its table builders and its checksum already sampled level with libzstd's. The sequence loop took 28% more samples, because the compiler kept the bit reader, the three states and the repeated offsets in memory.
2. Holding that state in locals and loading each table entry as one 64-bit word cut 8% of the instructions.
3. A copy longer than 16 bytes called `memcpy` from the hot loop, so every value lived on the stack across it. Moving the long, short-offset and lap-crossing copies behind `#[cold]` functions kept the loop's state in registers. Instructions rose and cycles fell by 11%.
4. Those cold calls ran for 12 to 17% of the sequences. Copying 32 bytes a step inline, doubling the pattern for offsets under 16, and reading the previous lap of the window buffer in the same loop removed them. Cycles fell by another 8 to 11%.

5. Keeping the three predefined sequence tables built, and copying them in, was reverted. On the 3,983 frames it cut instructions from 797 M to 791 M and left cycles at 167 M against 166 M, ranges overlapping.
6. Passing the sequence stream to the loop as one struct, to bring its argument count under the lint's limit, was reverted. Instructions rose by 0.7% and cycles by 8%, to 1,664 M against 1,541 M for libzstd on Silesia `-3`, ranges apart. Grouping the tables, the repeated offsets and the bounds in one struct kept the earlier counts. The loop's speed depends on which values the compiler keeps in registers.
7. One `read` now fills the caller's buffer across blocks. It had returned one block, at most 128 KiB, so the ZIP reader ran its CRC and write step eight times as often as with libzstd. Kept, with the extraction counts below.
8. Copying a match's first 32 bytes through one split of the output, with two bounds checks, was reverted. Extraction of the ZIP retired 2,655 M instructions against 2,836 M. It took 741 M cycles against 614 M for libzstd in the same run, because the compiler then kept the three FSE states on the stack.

The two memory streams, decoded once, five runs each. Cycles are the median, and the peak is the largest peak memory footprint, with the file held in memory.

| Stream | libzstd cycles | Zipper cycles | libzstd peak | Zipper peak |
| --- | ---: | ---: | ---: | ---: |
| Silesia twice, `--long=27`, 131,323,742 bytes | 1,009.7 M | 1,047.2 M | 267,616,832 | 267,715,112 |
| 3,983 frames of dependency sources, 14,405,664 bytes | 156.1 M | 170.6 M | 16,990,568 | 17,760,616 |

Whole `zipper extract --threads 1` processes on this machine, five runs in turn, median cycles. The libzstd build is `25dcb88`. Raw runs are in [`extract-before.txt`](extract-before.txt) and [`extract-after.txt`](extract-after.txt).

| Archive | Build | Instructions | Cycles | libzstd cycles in the same run |
| --- | --- | ---: | ---: | ---: |
| `zip-zstd.zip` | one block a read | 3,016 M | 658.4 M | 604.0 M |
| `zip-zstd.zip` | a full buffer a read | 2,839 M | 618.8 M | 616.0 M |
| `silesia.tar` under `zstd -3 -T0` | one block a read | 2,796 M | 608.9 M | 596.7 M |
| `silesia.tar` under `zstd -3 -T0` | a full buffer a read | 2,795 M | 613.7 M | 606.9 M |

On an Apple M2 Max the coordinator timed the build that returned one block a read. ZIP Zstandard Silesia took 266.1 ms against 228.9 ms at one thread, and Silesia `.tar.zst` 226.1 ms against 202.5 ms, ranges apart. The build that fills the buffer has not been timed there.

The adversarial review built a ZIP of 2,000 Zstandard entries, each a 10-byte frame that declares a 128 MiB window. The first wiring zeroed a whole window for every entry, 547 MB at peak and 6.15 s of CPU against 4.9 MB and 0.14 s. The window buffer is now reserved without being touched and filled as blocks arrive, and a thread keeps its decoder for the next frame. `zipper extract` on that ZIP now retires 1,721 M instructions with a 5.1 MB peak, against 1,740 M and 5.6 MB for the libzstd build.

## Decision

Kept. The decoder is wired in behind the `zstd` feature, and the `zstd` crate is now only a test oracle, so the default build no longer compiles libzstd. On the large streams, cycles are within 1 to 4% of libzstd, which the run-to-run spread on a busy machine does not separate from level. The coordinator times `zipper extract` on the zstd workloads before this ships, and reverts the wiring if a workload is slower beyond the spread.

Instructions stay 19 to 25% above libzstd. Bounds checks on the output buffer and two shifts where libzstd uses one account for most of the gap, and they run at a higher rate of instructions per cycle. No `unsafe` was tried, because cycles came within a few percent without it.

## Limits

- No wall time. Cycles from `/usr/bin/time -l` moved by up to 7% for the same binary between runs while other jobs ran, so a difference of a few percent here is not established either way.
- The counts come from one Apple M5. On an M2 Max the tar path was 11% slower in wall time where this machine counts 1 to 2% more cycles. The decoder retires 19 to 25% more instructions than libzstd, which a narrower core may not absorb.
- libzstd uses hand-written assembly for its Huffman loops on x86-64, which this record does not measure.
- The probe decodes from memory into a 128 KiB buffer. That hid the cost of returning one block a read, which only the extraction counts showed.
- SquashFS blocks decode through `decode_all`, one whole block at a time, which the probe does not count.
- Zipper's decoder is 98 KB above libzstd in peak memory at the window limit and 770 KB above on the 3,983 frames, where it reserves a larger buffer each time a frame needs one.
- Small frames cost 9% more cycles than libzstd, ranges apart. The record does not say which part of a small frame costs it.
- A thread that has decoded a compressed block keeps one decoder, about 550 KB, until it exits.

## Reproduce

Build the probe with `cargo test --release --lib --no-run`. Write the streams with `zstd -3` and `zstd -19 -T0` from `target/bench-corpora/codecs/silesia.tar` and from a tar of `target/bench-corpora/dependencies`. Then run `benchmarks/zstd/own-decoder/count.sh <test binary> 3 <streams>`. The earlier stages were not kept as separate commits, so only the last row can be recounted.
