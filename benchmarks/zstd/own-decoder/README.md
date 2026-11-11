# Zipper's own Zstandard decoder against libzstd

**Kept, and the gap on the M2 Max is closed. One-thread extraction of Silesia takes 0.97 to 0.98 times the cycles of libzstd 1.5.7 on an Apple M2 Max and 0.89 times on an Apple M5, in safe Rust. It took 1.12 to 1.14 times on the M2 Max while the compiler kept the sequence loop's FSE states on the stack. A file of 3,983 small frames still takes 1.05 times libzstd's cycles on the M2 Max. In wall time on a quiet M2 Max, `zipper extract` is 0.88 and 0.92 times libzstd on Silesia tar at one thread, ranges apart, and level with it on the other four workloads.** One Apple M5 and one Apple M2 Max.

## Question

M25 asks for a release build that compiles no C. The `zstd` feature linked libzstd through the `zstd` crate for ZIP method 93, `.zst` files, tar and RPM payloads and SquashFS blocks. Can a decoder in safe Rust replace it without costing decode work? The metric is retired cycles of a decode loop in one process, Zipper's decoder against libzstd on the same bytes, with retired instructions and peak memory beside it. A study of 7-Zip before [M25](../../../docs/roadmap/m25-a-release-build-without-c.md) found ZIP Zstandard extraction level with 7-Zip in cycles, 0.55 G against 0.56 G, so libzstd is the bar.

## Setup

- Apple M5 with 10 cores and 32 GB, macOS 26.7.1. Other agents built and ran Kani on the machine during every run, so no wall time was taken.
- The probe is the ignored test `decode_loop_for_counters` in `src/zstandard/tests.rs`. It reads a `.zst` file into memory and decodes it three times through a 128 KiB buffer, with `zstd::stream::read::Decoder` from the `zstd` crate 0.14.3, which holds libzstd 1.5.7, or with `ZstdReader`.
- [`count.sh`](count.sh) runs the probe under `/usr/bin/time -l`, five times for each decoder in turn, and prints the retired instructions, the cycles and the peak memory footprint of each run.
- Four streams. `silesia.tar`, 211,957,760 bytes, and a tar of the dependency sources, 81,319,936 bytes, each written by `zstd -3` and by `zstd -19 -T0`.
- Two more streams for memory. `silesia.tar` twice in a row under `zstd -3 --long=27`, which fills the 128 MiB window limit, and the 3,983 dependency source files as 3,983 frames in one file.
- Every stage passed the differential tests against libzstd before it was counted.
- Apple M2 Max with 12 cores, the second machine. Other agents ran their gates on it from time to time, so each count there waited until no process used a quarter of a core.
- [`count-stages.sh`](count-stages.sh) runs `zipper extract --threads 1` on `zip-zstd.zip` and on `silesia.tar` under `zstd -3 -T0`. It runs each binary once unrecorded and then seven times in turn under `/usr/bin/time -l`, and prints every run with the median, lowest and highest cycles.
- The ZIP holds 14,458,009 sequences and the tar stream 14,490,061, counted by a temporary counter in the decoder.

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

On an Apple M2 Max the coordinator timed the build that returned one block a read. ZIP Zstandard Silesia took 266.1 ms against 228.9 ms at one thread, and Silesia `.tar.zst` 226.1 ms against 202.5 ms, ranges apart. The build that fills the buffer took 250.8 ms against 228.6 ms on the ZIP and 225.3 ms against 201.7 ms on the tar, ranges apart.

The same whole-process counts on the M2 Max, with [`count-extract.sh`](count-extract.sh), five runs in turn, median. The in-process probe there took 1,795 M cycles against 1,589 M on Silesia `-3`. A sampled profile put the sequence loop at 20% more samples than libzstd's, and the Huffman loops, the checksum and the table builders level.

| Build on the M2 Max | ZIP instructions | ZIP cycles | Tar instructions | Tar cycles |
| --- | ---: | ---: | ---: | ---: |
| libzstd, `25dcb88` | 2,370 M | 640.2 M | 2,313 M | 606.6 M |
| The shipped decoder, `f5b5db9` | 2,844 M | 716.9 M | 2,799 M | 687.7 M |
| Sequences decoded 64 at a time, then copied | 2,833 M | 758.6 M | 2,791 M | 736.1 M |
| Largest offset tracked only where a block can pass the window | 2,802 M | 713.4 M | 2,758 M | 690.9 M |
| That, and a match's first 32 bytes through one split | 2,609 M | 734.8 M | 2,568 M | 711.0 M |
| Literals copied 16 bytes at a time | 2,845 M | 742.5 M | 2,800 M | 718.2 M |
| Literals and matches copied 16 bytes at a time | 2,827 M | 733.3 M | 2,783 M | 709.8 M |
| One bounds check a match, then unchecked steps, with `unsafe` | 2,774 M | 711.1 M | 2,731 M | 686.7 M |
| No call inside the loop, rare sequences leave it | 2,781 M | 712.8 M | 2,736 M | 688.2 M |

All seven changes were reverted. None moved cycles by more than 1% in the right direction, and the two that cut the most instructions were slower. In the shipped loop the compiler kept the FSE states on the stack. Two of them stayed there in the last row, where a rare sequence left the inner loop but its call stayed in the same function. The stages below show that this round trip was the gap.

The shipped loop was disassembled from the release binary with `objdump -d`. At its head it held 19 values in registers. Seven do not change in the loop. They were the result pointer, the block, the output pointer, the output length, that length less 16 and less 32, and the start of the stream. Two more pointed at the bounds and the tables. The other ten were the output position, the count of sequences left, the bit container, its position, its bit count, the offset state, the first two repeated offsets, the literal position and the largest offset. Each sequence then made these stack accesses on its common path.

| Value on the stack | Stores a sequence | Loads a sequence |
| --- | ---: | ---: |
| Literal length state | 1 | 1 |
| Match length state | 1 | 1 |
| Bit container | 1 | 1 |
| Second repeated offset | 1 | 1 |
| Third repeated offset | 1 on a new offset | 1 on a repeated offset |
| Bounds, end of the literals and literal bytes, which do not change | 0 | 3 |

libzstd's loop in the `25dcb88` binary keeps its three states, the container, its position and its bit count in registers. It stores the states each sequence and does not load them back on its common path.

The kept loop makes no store to the stack on its common path. It loads five values that do not change, which are the block, the three tables and the end of the literals. The table entries of the three states, the container's position and bit count, the three repeated offsets, both positions, the count left and the largest offset stay in registers.

Whole `zipper extract --threads 1` processes, seven runs in turn, median cycles in millions with the lowest and highest. The M2 Max rows are from one run and the M5 rows from another. Each stage builds on the one above unless it says otherwise. Raw runs are in [`stages-m2.txt`](stages-m2.txt) and [`stages-m5.txt`](stages-m5.txt).

| Build | ZIP instructions | M2 Max ZIP cycles | M2 Max tar cycles | M5 ZIP cycles | M5 tar cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| libzstd, `25dcb88` | 2,372 M | 640.7, 638.6 to 668.6 | 611.5, 608.7 to 614.4 | 613.8, 606.2 to 624.6 | 585.7, 580.6 to 643.7 |
| The decoder before this change | 2,845 M | 717.6, 715.9 to 727.8 | 696.2, 689.5 to 707.1 | 616.3, 612.2 to 627.3 | 598.7, 593.7 to 654.3 |
| 1. The loop in its own function, with no call that returns | 3,051 M | 666.8, 661.1 to 671.0 | 645.0, 641.5 to 682.4 | 598.9, 596.1 to 610.2 | 579.2, 577.3 to 597.3 |
| 2. The bit container loaded at the head of the loop | 2,990 M | 666.6, 665.0 to 671.9 | 643.8, 640.6 to 645.7 | 600.7, 593.9 to 601.1 | 579.7, 577.0 to 599.5 |
| 3. The first 32 literal bytes copied without a loop | 2,922 M | 654.6, 653.1 to 688.9 | 630.5, 627.7 to 633.0 | 592.8, 587.5 to 595.8 | 573.1, 568.3 to 585.8 |
| 4. A match's first 32 bytes through one split, reverted | 2,902 M | 660.4, 658.9 to 662.0 | 635.6, 630.7 to 639.3 | 603.4, 596.6 to 604.6 | 580.1, 577.4 to 596.7 |
| 5. Stage 4 with the copy loops marked cold | 2,543 M | 649.3, 643.1 to 658.5 | 628.2, 622.2 to 633.1 | 596.1, 585.1 to 601.9 | 576.4, 574.5 to 589.4 |
| 6. Table entries carried, next state by shift, OR and AND | 2,508 M | 627.8, 626.8 to 629.1 | 603.5, 598.9 to 608.4 | 548.5, 544.4 to 551.6 | 527.8, 525.2 to 540.2 |
| 7. Stage 6 without the cold marks, reverted | 2,522 M | 628.9, 625.1 to 629.9 | 607.3, 603.8 to 610.5 | 552.0, 548.8 to 558.6 | 531.8, 528.5 to 545.2 |
| 8. Stage 6 with `copy_within` and no split, kept | 2,513 M | 619.3, 618.2 to 621.6 | 596.9, 595.0 to 605.4 | 544.8, 540.7 to 550.2 | 522.4, 521.1 to 525.7 |

What each stage changed.

1. The loop moved into its own function, `fast_sequences`, which makes no call that returns. A sequence that needs a careful copy writes the loop's state back and returns, and the caller copies it and calls again. That happens for 37 of the ZIP's sequences. Instructions rose by 7% and M2 Max cycles fell by 7%, since the states then stayed in registers. An earlier try, the last row of the table above, had kept the call for a rare sequence in the loop's own function.
2. The bit container is loaded at the head of the loop, so it is not held across the copy. Cycles did not move, ranges overlapping. It stayed because stage 6 is built on it.
3. The first 32 literal bytes are copied with one bounds check and no loop, and the range check on a match from the previous lap moved to that path. M2 Max cycles fell by about 2%.
4. A match's first 32 bytes went through one split of the output. Reverted. Cycles rose by 1 to 2% on the M5, with the ranges apart on the ZIP. The compiler moved eight registers before and after every literal copy and put the bit count on the stack, because it weighs an inner copy loop as hotter than the loop around it.
5. `std::hint::cold_path` marks the copy loops for literals and matches longer than 32 bytes and the copies at a distance under 32. Instructions fell by 12% and cycles stayed where stage 3 had them, ranges overlapping.
6. The loop carries the three table entries, not the three states. Each entry holds its next state's base, the bits it reads in a sequence and the largest state it can reach. The next state is the container shifted once, OR the base, AND the largest state, which needs no bounds check. The shift for each state is two subtractions from fields of the three entries. The offsets table grew from 256 to 512 entries so that every state indexes without a mask. Cycles fell by 3 to 4% on the M2 Max and by 8% on the M5.
7. Stage 6 without the cold marks on the copy loops. Reverted. Its medians are 0.2 to 0.8% above stage 6 in all four columns, ranges overlapping, so the marks stayed.
8. Stage 6 with the match copied by `copy_within` as before stage 4, and no split. Kept. It is the simpler code. Its medians are 0.7 to 1.4% below stage 6 in all four columns, with the ranges apart on the M2 Max ZIP.

The loop is limited by the chain of operations from one sequence's states to the next, not by its instruction count. Stage 1 retired 7% more instructions in 7% fewer cycles, and stage 5 retired 12% fewer in the same cycles. In the shipped loop two states went through a store and a load on that chain. In the kept loop the chain is a table load, a field extract, two subtractions, a shift, an OR and an AND. No hardware event counter was read, so this is inferred from the assembly and the counts.

The in-process probe of the kept decoder, five runs each in turn, median cycles in millions. The four large streams are decoded three times and the two memory streams once. The dependency tar was rebuilt and is 81,977,856 bytes. Raw runs are in [`probe-m5.txt`](probe-m5.txt) and [`probe-m2.txt`](probe-m2.txt).

| Stream | Machine | libzstd instructions | Zipper instructions | libzstd cycles | Zipper cycles | Ratio |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Silesia `-3` | M5 | 6,428 M | 6,888 M | 1,545.5 | 1,335.9 | 0.86 |
| Silesia `-19` | M5 | 6,043 M | 6,444 M | 1,490.7 | 1,312.9 | 0.88 |
| Dependencies `-3` | M5 | 1,426 M | 1,523 M | 357.5 | 315.8 | 0.88 |
| Dependencies `-19` | M5 | 1,162 M | 1,243 M | 301.9 | 275.3 | 0.91 |
| Silesia twice, `--long=27` | M5 | 4,260 M | 4,600 M | 1,011.5 | 915.2 | 0.90 |
| 3,983 frames | M5 | 638 M | 717 M | 156.0 | 152.1 | 0.98 |
| Silesia `-3` | M2 Max | 6,427 M | 6,887 M | 1,589.7 | 1,529.1 | 0.96 |

The 3,983 frames on the M2 Max, with [`count-frames.sh`](count-frames.sh), five runs in turn. Raw runs are in [`frames-m2.txt`](frames-m2.txt).

| Decoder | Instructions | Median cycles | Lowest | Highest | Largest peak |
| --- | ---: | ---: | ---: | ---: | ---: |
| libzstd | 637 M | 164.2 M | 163.6 M | 165.0 M | 17,220,016 |
| The decoder before this change | 798 M | 196.4 M | 195.8 M | 197.1 M | 17,973,704 |
| The kept decoder | 717 M | 172.7 M | 171.8 M | 174.9 M | 17,990,088 |

Wall time of `zipper extract` on the M2 Max, measured with `scripts/compare_zip_readers.py` with nothing else running, 7 runs in turn after one warmup and 2 s apart. The `libzstd` binary is `25dcb88`, and `own` is the sequence loop's commit. Both outputs matched the source tree byte for byte on every run. Raw samples are in [`wall-m2/`](wall-m2/).

| Archive | Size | Threads | libzstd median | Range | Zipper median | Range | Ratio | CPU ratio |
| --- | ---: | ---: | ---: | --- | ---: | --- | ---: | ---: |
| Silesia tar, `-3` | 66,217,060 | 1 | 0.215 s | 0.212 to 0.218 | 0.198 s | 0.196 to 0.199 | 0.92 | 0.92 |
| Silesia tar, `-19` | 52,849,367 | 1 | 0.225 s | 0.222 to 0.225 | 0.197 s | 0.191 to 0.200 | 0.88 | 0.89 |
| Dependencies tar, `-3` | 9,025,417 | 1 | 0.410 s | 0.399 to 0.416 | 0.412 s | 0.406 to 0.425 | 1.01 | 0.99 |
| Dependencies tar, `-3` | 9,025,417 | default | 0.291 s | 0.274 to 0.295 | 0.285 s | 0.276 to 0.295 | 0.98 | 0.99 |
| Silesia ZIP, method 93 | 66,217,767 | 1 | 0.229 s | 0.226 to 0.239 | 0.224 s | 0.216 to 0.237 | 0.98 | 0.98 |
| Silesia ZIP, method 93 | 66,217,767 | default | 0.099 s | 0.095 to 0.103 | 0.101 s | 0.095 to 0.102 | 1.03 | 1.06 |

Peak memory stayed within 1 MB of libzstd in every row. The Silesia tar rows are the only ones with ranges apart. The dependency tar is 67 MB of small files, so file creation limits it, not decoding. Before this change, the same workloads took 1.00 to 1.12 times libzstd's wall time.

The adversarial review built a ZIP of 2,000 Zstandard entries, each a 10-byte frame that declares a 128 MiB window. The first wiring zeroed a whole window for every entry, 547 MB at peak and 6.15 s of CPU against 4.9 MB and 0.14 s. The window buffer is now reserved without being touched and filled as blocks arrive, and a thread keeps its decoder for the next frame. `zipper extract` on that ZIP now retires 1,721 M instructions with a 5.1 MB peak, against 1,740 M and 5.6 MB for the libzstd build.

## Decision

Kept. The decoder is wired in behind the `zstd` feature, and the `zstd` crate is now only a test oracle, so the default build no longer compiles libzstd. Extraction of Silesia takes fewer cycles than with libzstd on both machines, 0.97 to 0.98 times on the M2 Max and 0.89 times on the M5, with the ranges apart. Wall time agrees. `zipper extract` is faster than with libzstd on Silesia tar and level on the other four workloads, with no workload slower beyond the spread.

Instructions stay 6 to 7% above libzstd in extraction. Of the eight stages, stages 1, 2, 3, 5, 6 and 8 are in the kept loop. It uses no `unsafe`.

## Limits

- The cycle counts were taken while other jobs ran. Cycles from `/usr/bin/time -l` moved by up to 7% for the same binary between runs, so a difference of a few percent in them is not established either way. The wall times were taken on a quiet M2 Max only, not on the M5.
- libzstd's wall time on Silesia tar at one thread was 0.200 s and 0.202 s in an earlier session on the same machine, against 0.225 s and 0.215 s here. Each ratio compares runs taken in turn in one session.
- The loop's speed depends on which values rustc 1.99.1 keeps in registers, and nothing in the gate checks that. A change to `fast_sequences`, or a compiler update, can move the states back to the stack. Only a cycle count on the M2 Max shows it, since the M5 took 1.00 to 1.02 times libzstd's cycles with the states on the stack.
- No hardware event counter was read. The dependency chain is inferred from the assembly and from cycles that did not follow instructions.
- The M2 Max ran other agents' gates between the counts. The lowest and highest cycles in the table show what load still reached a run.
- No `unsafe` is used. The one unchecked copy tried on the shipped loop moved M2 cycles by less than 1%, and none was tried on the kept loop.
- The offsets table has 512 entries where it had 256, which adds 2 KB to each decoder.
- libzstd uses hand-written assembly for its Huffman loops on x86-64, which this record does not measure.
- The probe decodes from memory into a 128 KiB buffer. That hid the cost of returning one block a read, which only the extraction counts showed.
- SquashFS blocks decode one whole block at a time through `decode_frames_into`, which the probe does not count. That path refuses a block that decodes past its frame's largest block size, whatever room the output has. libzstd's one-shot call accepts some of those blocks, while its streaming call refuses them.
- Zipper's decoder is 98 KB above libzstd in peak memory at the window limit and 770 KB above on the 3,983 frames, where it reserves a larger buffer each time a frame needs one.
- The 3,983 small frames take 0.98 times libzstd's cycles on the M5 and 1.05 times on the M2 Max, ranges apart on both. The record does not say which part of a small frame costs the M2 Max more.
- A thread that has decoded a compressed block keeps one decoder, about 550 KB, until it exits.

## Reproduce

Build the probe with `cargo test --release --lib --no-run`. Write the streams with `zstd -3` and `zstd -19 -T0` from `target/bench-corpora/codecs/silesia.tar` and from a tar of `target/bench-corpora/dependencies`. Then run `benchmarks/zstd/own-decoder/count.sh <test binary> 3 <streams>`. The earlier stages were not kept as separate commits, so only the last row can be recounted.

For the stage table, build `zipper` at `25dcb88` as `libzstd` and at this record's commit as `kept`. Put both in one folder with `count-stages.sh`, `target/bench-corpora/codecs/zip-zstd.zip` and `silesia-3.tar.zst`, which is `zstd -3 -T0 -c target/bench-corpora/codecs/silesia.tar`. Then run `./count-stages.sh libzstd kept`. Stages 1 to 7 were not kept as commits. For the wall times, put both binaries as `zipper-25dcb88` and `zipper-zstd3` beside the archives and run `scripts/compare_zip_readers.py <archive> <source> --archive-format native --runs 7 --cooldown 2 --reader libzstd native zipper-25dcb88 --reader own native zipper-zstd3`, with `--archive-format zip` for the ZIP and `--reader-threads` for the thread count. For the frames, write each file under `target/bench-corpora/dependencies` in sorted order with `zstd -3 -c` into one file, copy the test binary as `probe-kept`, and run `./count-frames.sh` on it. The `shipped` rows need the test binary of the commit before this change as `probe-shipped`.
