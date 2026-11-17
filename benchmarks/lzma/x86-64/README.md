# Zipper's LZMA decoder against liblzma on x86-64

**No change is kept and the gap stays. Seven experiments made no decode faster with ranges apart, on either corpus. The limiter is named. On x86-64 with GCC, liblzma 5.8.4 decodes its bit trees, matched literals and direct bits in hand-written assembly, and Zipper's kernel retires 1.49 times the instructions for the same misses. The decoder on main takes 1.08 to 1.21 times liblzma's time on LZMA and 1.04 to 1.14 times on xz.** Hosted x86-64 Linux runners, a different processor in each run. No Apple M2 Max run, since no decoder code changed.

## Question

[M35](../../../docs/roadmap/m35-zstandard-and-lzma-at-c-speed-on-x86-64.md) asks why Zipper's LZMA and xz decoder trails liblzma on x86-64 Linux, and what closes the gap in safe Rust. The metric is the wall time of one decode of the corpus stream, timed inside `zipper-decode-bench decode`. Each experiment compares the build before a change with the build after it and with liblzma, in turn on one runner.

## Setup

- Hosted GitHub Actions runners, 4 cores, Linux 7.0, rustc 1.99.1 with no `RUSTFLAGS`, and GCC 15.2.0. Each run got its own processor, which the tables name. A ratio compares only rows of one run.
- The two M31 corpora and their streams. `silesia.tar` is 211,957,760 bytes and `dependencies.tar` is 90,931,200 bytes. Each is an xz stream in one block with CRC-64 and an LZMA-alone stream with an end marker, from liblzma at preset 6.
- liblzma is 5.8.4, which `liblzma-sys` 0.4.9 bundles and builds with the runner's C compiler.
- The profiles are three runs of `m35-profile.yml` on the dependencies LZMA stream, with 10 decodes a process. Run 44022441626 got an Intel Xeon 6975P-C, which counts hardware events. Run 44022951771 holds the annotated assembly, and run 44022451898 built the Rust code for x86-64-v3.
- The first two profile runs wrote empty annotate files. perf 7.0 prints two more columns after a symbol, and `scripts/m35_profile.py` passed them on as part of the name. The fix is on this branch, and run 44022951771 used it.
- Each experiment is one commit on the decoder of `06402764`, pushed to a branch named `m35-exp/lzma-NAME`. The A/B mode of `m35-profile.yml` built the bench before and after the commit and ran 15 rounds after one warmup, or 21 in a second pair of runs of experiment 7. A round runs the three rows in a new seeded order, one process a sample, and checks every decoded byte. Each JSON names its base as the commit that adds the profile script's fix to `06402764`, which leaves the decoder as it was.
- Experiment 2's direct bit loop compiles to the compare, select and subtraction that the annotated assembly of the decoder before it already holds. So its rows show what the method reports where little or nothing changed. Its four ratios are 0.995 to 1.001, ranges overlapping.
- The instruction shapes of each experiment were read from `src/lzma.rs` and `src/xz_filters.rs` compiled alone by rustc 1.99.1 for x86-64, with `-C opt-level=3`. That build names no time. It differs from the bench's build, which inlines the literal and direct bit code into the kernel.
- Raw samples of every A/B run are in [`data/`](data/), one JSON a run, named by experiment, corpus and workflow run. The profile folders there hold each run's summary, report and processor.

## Results

### What the gap is made of

Counts of one decode of the dependencies LZMA stream, from `perf stat` on the Intel Xeon 6975P-C in run 44022441626.

| Event | Zipper | liblzma | Zipper over liblzma |
| --- | ---: | ---: | ---: |
| Cycles | 1,218,273,484 | 1,097,853,014 | 1.110 |
| Instructions | 3,036,966,233 | 2,037,572,732 | 1.490 |
| Branches | 293,412,369 | 181,569,700 | 1.616 |
| Branch misses | 14,452,152 | 14,411,626 | 1.003 |
| L1 data cache load misses | 3,557,267 | 3,282,199 | 1.084 |

- Both decoders miss the same branches, 14.4 million a decode, so prediction does not separate them. Zipper retires 2.49 instructions a cycle and liblzma 1.86.
- `decode_symbols` holds 94.56% of Zipper's samples in that run, and `lzma_decode` holds 84.86% of liblzma's.
- liblzma's `range_decoder.h` sets `LZMA_RANGE_DECODER_CONFIG` to `0x1F0` when GCC or Clang builds it for x86-64, and to 0 for every other compiler and target. That mask swaps the C macros for inline x86-64 assembly in five places. They are the bit trees, the fixed and the sized reverse trees, the matched literal and the direct bits.
- So on these runners liblzma's hot loops are hand-written assembly. On the Apple M2 Max and under MSVC on Windows they are C. That matches M31 and M34, where Zipper leads liblzma on both of those machines and trails it only on x86-64 Linux.
- Both kernels load the two children of a tree node before its bit resolves, and both wait for the same chain from one bit to the next. The chain is a select, a shift, a multiply and a compare. That is about 6 cycles by those instructions' usual latencies, which no counter here measured.

Instructions for one bit on the path that does not normalize. Zipper's are counted in the annotated assembly of run 44022951771, on an AMD EPYC 9V45, with each loop's share of the kernel's samples. liblzma's are counted in the assembly macros of `range_decoder.h`.

| Loop | Share of samples | Zipper | liblzma |
| --- | ---: | ---: | ---: |
| Distance slot tree, unrolled | 14.5% | 27 | 21 |
| Plain literal | 8.9% | 32 | 21 |
| Matched literal | 6.9% | 62 | 27 |
| Direct bits | 9.3% | 14 | 11 |
| Match copy, with its loop head | 12.2% | not a bit loop | not a bit loop |

- rustc compares the code with the bound two or three times a bit, because a subtraction or an `xor` between two selects overwrites the flags. liblzma's assembly subtracts once and reads that one result five times.
- rustc builds the code's update from a zeroed register, a select and a subtraction. It also widens the 16-bit probability again before each multiply.
- rustc steps the tree index with a flag set and an OR. liblzma steps it with one `sbb`.
- The matched literal keeps more values alive than x86-64 has registers. Its loop reads or writes the stack 7 times a bit.

The x86-64-v3 profile, run 44022451898 on an AMD EPYC 9V45, spent 235 ms a decode against liblzma's 215 ms, a ratio of 1.091. The baseline profile on another AMD EPYC 9V45, run 44022951771, spent 256 ms against 224 ms, a ratio of 1.143. Two runs do not compare, so experiment 5 measured BMI code in one run.

### Experiments

Each row is one run and one stream. Times are the median in milliseconds with the lowest and highest sample, of 15 samples unless the row names 21 rounds. "Before" is the decoder of `06402764`.

| Experiment | Corpus | Stream | Processor | Run | Before | After | liblzma | After over before | After over liblzma |
| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1. One subtraction a bit, 32-bit probabilities | Silesia | `lzma` | AMD EPYC 9V74 | 44024261778 | 1718.3, 1682.9 to 1818.2 | 1717.4, 1694.7 to 1816.8 | 1441.4, 1428.6 to 1496.8 | 0.999, overlap | 1.192, apart |
| 1. One subtraction a bit, 32-bit probabilities | Silesia | `xz` | AMD EPYC 9V74 | 44024261778 | 1831.4, 1801.0 to 1886.6 | 1827.4, 1810.2 to 1928.3 | 1613.4, 1596.0 to 1649.9 | 0.998, overlap | 1.133, apart |
| 1. One subtraction a bit, 32-bit probabilities | Dependencies | `lzma` | Intel Xeon 6975P-C | 44024265297 | 320.7, 317.1 to 323.1 | 318.8, 316.2 to 323.5 | 286.9, 284.4 to 288.9 | 0.994, overlap | 1.111, apart |
| 1. One subtraction a bit, 32-bit probabilities | Dependencies | `xz` | Intel Xeon 6975P-C | 44024265297 | 369.9, 367.3 to 375.9 | 369.7, 367.4 to 375.4 | 353.7, 351.0 to 355.2 | 0.999, overlap | 1.045, apart |
| 2. Direct bit from an overflowing subtraction | Silesia | `lzma` | AMD EPYC 9V74 | 44024264427 | 1680.7, 1667.5 to 1697.9 | 1681.9, 1673.4 to 1721.2 | 1428.6, 1420.8 to 1445.7 | 1.001, overlap | 1.177, apart |
| 2. Direct bit from an overflowing subtraction | Silesia | `xz` | AMD EPYC 9V74 | 44024264427 | 1790.2, 1780.3 to 1815.6 | 1792.0, 1785.6 to 1811.2 | 1592.3, 1585.4 to 1606.6 | 1.001, overlap | 1.125, apart |
| 2. Direct bit from an overflowing subtraction | Dependencies | `lzma` | Intel Xeon 6975P-C | 44024268472 | 316.7, 315.2 to 319.7 | 315.4, 313.2 to 316.5 | 285.9, 282.6 to 287.8 | 0.996, overlap | 1.103, apart |
| 2. Direct bit from an overflowing subtraction | Dependencies | `xz` | Intel Xeon 6975P-C | 44024268472 | 368.4, 364.6 to 375.6 | 366.4, 362.6 to 370.7 | 352.6, 349.9 to 353.9 | 0.995, overlap | 1.039, apart |
| 3. Selects before the normalization branch | Silesia | `lzma` | Intel Xeon Platinum 8573C | 44025227291 | 2309.6, 2283.2 to 2395.4 | 2334.5, 2315.7 to 2367.3 | 2127.6, 2114.2 to 2161.8 | 1.011, overlap | 1.097, apart |
| 3. Selects before the normalization branch | Silesia | `xz` | Intel Xeon Platinum 8573C | 44025227291 | 2475.0, 2451.0 to 2528.4 | 2497.0, 2479.6 to 2546.1 | 2325.7, 2315.6 to 2368.9 | 1.009, overlap | 1.074, apart |
| 3. Selects before the normalization branch | Dependencies | `lzma` | AMD EPYC 7763 | 44025230657 | 379.1, 375.8 to 385.2 | 381.0, 378.9 to 386.0 | 316.9, 313.3 to 328.2 | 1.005, overlap | 1.202, apart |
| 3. Selects before the normalization branch | Dependencies | `xz` | AMD EPYC 7763 | 44025230657 | 435.3, 433.1 to 437.8 | 436.6, 433.8 to 439.4 | 398.0, 394.7 to 401.9 | 1.003, overlap | 1.097, apart |
| 4. Direct bit picked by the sign | Silesia | `lzma` | AMD EPYC 9V45 | 44025231583 | 1355.6, 1309.6 to 1428.5 | 1314.4, 1274.3 to 1422.8 | 1250.6, 1202.4 to 1306.6 | 0.970, overlap | 1.051, overlap |
| 4. Direct bit picked by the sign | Silesia | `xz` | AMD EPYC 9V45 | 44025231583 | 1451.3, 1362.2 to 1510.1 | 1421.1, 1352.0 to 1533.2 | 1375.2, 1333.9 to 1456.2 | 0.979, overlap | 1.033, overlap |
| 4. Direct bit picked by the sign | Dependencies | `lzma` | Intel Xeon 6975P-C | 44025235078 | 314.5, 310.7 to 315.7 | 314.2, 312.1 to 316.0 | 282.3, 279.7 to 286.0 | 0.999, overlap | 1.113, apart |
| 4. Direct bit picked by the sign | Dependencies | `xz` | Intel Xeon 6975P-C | 44025235078 | 363.8, 361.9 to 367.0 | 363.4, 359.1 to 372.5 | 349.7, 345.7 to 352.9 | 0.999, overlap | 1.039, apart |
| 5. BMI1 and BMI2 build chosen at run time | Silesia | `lzma` | AMD EPYC 9V74 | 44025335338 | 1674.5, 1667.5 to 1720.2 | 1696.8, 1683.8 to 1821.8 | 1425.2, 1419.2 to 1433.5 | 1.013, overlap | 1.191, apart |
| 5. BMI1 and BMI2 build chosen at run time | Silesia | `xz` | AMD EPYC 9V74 | 44025335338 | 1792.1, 1782.2 to 1823.7 | 1806.3, 1800.0 to 1848.7 | 1592.5, 1587.0 to 1605.2 | 1.008, overlap | 1.134, apart |
| 5. BMI1 and BMI2 build chosen at run time | Dependencies | `lzma` | AMD EPYC 9V74 | 44025338680 | 312.3, 305.5 to 325.6 | 315.4, 309.3 to 330.1 | 258.7, 255.1 to 265.8 | 1.010, overlap | 1.219, apart |
| 5. BMI1 and BMI2 build chosen at run time | Dependencies | `xz` | AMD EPYC 9V74 | 44025338680 | 364.4, 353.2 to 375.3 | 366.2, 357.4 to 378.4 | 332.3, 324.6 to 335.4 | 1.005, overlap | 1.102, apart |
| 6. Plain literal in eight written-out steps | Silesia | `lzma` | AMD EPYC 9V45 | 44025700166 | 1478.1, 1372.8 to 1744.3 | 1489.6, 1360.4 to 1688.7 | 1366.6, 1270.3 to 1652.3 | 1.008, overlap | 1.090, overlap |
| 6. Plain literal in eight written-out steps | Silesia | `xz` | AMD EPYC 9V45 | 44025700166 | 1543.5, 1441.7 to 1785.0 | 1617.5, 1451.8 to 1840.7 | 1486.1, 1404.1 to 1715.7 | 1.048, overlap | 1.088, overlap |
| 6. Plain literal in eight written-out steps | Dependencies | `lzma` | AMD EPYC 9V74 | 44025703929 | 306.7, 301.9 to 317.6 | 311.0, 304.6 to 373.7 | 252.8, 250.0 to 262.0 | 1.014, overlap | 1.231, apart |
| 6. Plain literal in eight written-out steps | Dependencies | `xz` | AMD EPYC 9V74 | 44025703929 | 356.7, 349.1 to 363.7 | 359.5, 353.3 to 368.1 | 325.0, 321.3 to 334.3 | 1.008, overlap | 1.106, apart |
| 7. Tree index stepped by a borrow | Silesia | `lzma` | Intel Xeon Platinum 8370C | 44026540631 | 2026.2, 2003.6 to 2257.6 | 2031.3, 2013.0 to 2057.2 | 1797.4, 1780.8 to 1813.3 | 1.003, overlap | 1.130, apart |
| 7. Tree index stepped by a borrow | Silesia | `xz` | Intel Xeon Platinum 8370C | 44026540631 | 2147.0, 2123.1 to 2175.3 | 2153.2, 2132.7 to 2219.9 | 1976.0, 1959.2 to 1993.7 | 1.003, overlap | 1.090, apart |
| 7. Tree index stepped by a borrow, 21 rounds | Silesia | `lzma` | Intel Xeon 6975P-C | 44027142807 | 1684.8, 1680.6 to 1696.1 | 1681.7, 1675.6 to 1696.8 | 1560.5, 1557.3 to 1562.8 | 0.998, overlap | 1.078, apart |
| 7. Tree index stepped by a borrow, 21 rounds | Silesia | `xz` | Intel Xeon 6975P-C | 44027142807 | 1800.1, 1794.4 to 1814.1 | 1796.2, 1792.2 to 1803.2 | 1715.6, 1711.5 to 1748.4 | 0.998, overlap | 1.047, apart |
| 7. Tree index stepped by a borrow | Dependencies | `lzma` | AMD EPYC 7763 | 44026544495 | 366.7, 362.2 to 373.9 | 363.9, 360.4 to 372.0 | 304.5, 302.0 to 311.5 | 0.992, overlap | 1.195, apart |
| 7. Tree index stepped by a borrow | Dependencies | `xz` | AMD EPYC 7763 | 44026544495 | 423.9, 418.3 to 429.8 | 419.2, 413.9 to 428.0 | 384.1, 381.7 to 394.5 | 0.989, overlap | 1.092, apart |
| 7. Tree index stepped by a borrow, 21 rounds | Dependencies | `lzma` | AMD EPYC 9V74 | 44027146279 | 387.9, 386.8 to 401.3 | 393.4, 391.4 to 396.0 | 321.6, 321.2 to 329.5 | 1.014, overlap | 1.223, apart |
| 7. Tree index stepped by a borrow, 21 rounds | Dependencies | `xz` | AMD EPYC 9V74 | 44027146279 | 448.9, 447.7 to 451.9 | 455.1, 453.6 to 460.8 | 414.0, 413.4 to 434.8 | 1.014, apart | 1.099, apart |

What each experiment changed, and the branch it ran on.

1. `m35-exp/lzma-e1`. The bit step takes the bound from the code with one overflowing subtraction and selects on its borrow. Probabilities stay 32 bits wide between two bits. The compiler turned both back into the earlier instructions, with the same count.
2. `m35-exp/lzma-e2`. A direct bit takes its value from one overflowing subtraction. In the standalone build the compiler emits a compare, a select and a subtraction for it, as the bench's build did before.
3. `m35-exp/lzma-e4`. Each tree bit decodes, stores, selects the next probability and loads the next children before the normalization branch. In the standalone build the compiler still compares three times a bit, and the count stays 27.
4. `m35-exp/lzma-e5`. A direct bit is picked by the sign of the code less the halved range, as liblzma and the LZMA SDK pick it. In the standalone build the code then waits for a subtraction and a select a bit, not a compare, a select and a subtraction. This form reads a code at or above the range differently, which only a damaged stream can reach.
5. `m35-exp/lzma-e6`. `decode_symbols` runs a build with BMI1 and BMI2 when the processor has both, through one `unsafe` call, as the Zstandard sequence loop does. The kernel's helpers are forced inline so both builds keep them in the loop.
6. `m35-exp/lzma-e8`. The plain literal is eight written-out steps, so no loop counter and no reload of the table's address remain.
7. `m35-exp/lzma-e11`. A tree index steps as twice the index plus one, less the zero flag. In the standalone build the compiler then emits `lea`, `cmp` and `sbb`, and the next children's address is ready one instruction after the compare.

Three more shapes were read in the standalone build and not timed, since none retired fewer instructions a bit.

- The code's update as the unsigned minimum of the code and the code less the bound. The subtraction's flags then feed one select and the flag set, but the minimum takes a compare of its own, and the count stays 27.
- The matched literal in eight written-out steps. The literal function grew from 364 to 773 instructions, with 99 stack reads and writes against 46.
- The literal decoder outside the kernel, in a function of its own. Its plain loop still takes about 30 instructions a bit and its matched loop about 60.

### Apple M2 Max

Not timed. No experiment is kept, so the decoder on main is the one M31 measured there.

Experiment 7 changes the instructions of the aarch64 kernel too. A form that keeps the OR step on every target but x86-64 compiles the aarch64 kernel to the same instructions as before, with two registers swapped in the matched literal. It was written and not kept, since experiment 7 itself is not established.

## Decision

All seven experiments are reverted, and `src/lzma.rs` is unchanged. In 31 of the 32 rows the decoder after a change overlaps the decoder before it. In the other row, experiment 7 on the dependencies xz stream with 21 rounds, it is 1.014 times slower with ranges apart. The medians moved by 0.970 to 1.048 times, and by 0.989 to 1.014 times in the runs with steady ranges.

The verdict for M35's LZMA half stays "not ready yet", by the third exit check. The decoder of `06402764` took 1.080 to 1.213 times liblzma's time on LZMA and 1.039 to 1.135 times on xz, over the 16 runs listed above. Its ranges were apart from liblzma's in every run but the two Silesia runs on an AMD EPYC 9V45. There xz overlapped twice and LZMA once.

The limiter is the instruction count of the bit step. liblzma's x86-64 build runs hand-scheduled assembly at 11 to 27 instructions a bit. rustc 1.99.1 emits 14 to 62 for the same steps, and it turned each reshaped source back into those instructions. M35 allows no assembly, so two routes remain, and neither was measured.

- A matched literal with fewer live values, which is where Zipper is furthest from liblzma, 62 instructions a bit against 27.
- Another compiler release. Only rustc 1.99.1 was tried, and it chose the instructions of every reshaped bit step.

## Limits

- Every number is from a hosted runner with 4 shared cores. One run compares its own three rows. The ratio to liblzma moved from 1.08 to 1.21 between processors for the same code, so no ratio here holds for x86-64 in general.
- Only the Intel Xeon 6975P-C run counted hardware events, and only on the dependencies LZMA stream. The shares of samples come from the processor clock on another machine.
- The instruction counts a bit are read from one build's assembly. They count the path without a normalization, which most bits take.
- liblzma's count is read from its macros, not from the runner's binary. `perf annotate` ran for Zipper's symbols only.
- A change of 1% or less cannot be told from none here. Outside the two runs on an AMD EPYC 9V45, no median moved by more than 1.4%, in either direction.
- The two Silesia runs on an AMD EPYC 9V45 had ranges 120 to 390 ms wide, so experiments 4 and 6 have one steady corpus each.
- No extraction was timed. `zipper extract` reads xz through smaller buffers than the bench.
- No experiment ran the test suite or a Kani proof, since none is kept. Each passed the bench's check of every decoded byte on both corpora. Experiment 4 changes what a damaged stream decodes to and experiment 5 adds `unsafe`, so either would need a review.

## Reproduce

Profile the decoder on main, then read `summary.md` and the annotate files in the artifact.

```sh
gh workflow run m35-profile.yml --ref main -f corpus=dependencies -f codec=lzma \
  -f target_cpu=baseline -f repeats=10
gh run download RUN_ID --dir target/m35/lzma-profile
```

Time a change by pushing it to a branch named `m35-exp/NAME` that holds `06402764`, and dispatching the A/B mode once for each corpus.

```sh
gh workflow run m35-profile.yml --ref m35-exp/NAME -f corpus=silesia -f codec=all \
  -f target_cpu=baseline -f base=06402764 -f rounds=15 -f profile=false
gh run download RUN_ID --dir target/m35/NAME
```

The experiment branches were deleted after their runs, so their rows cannot be rebuilt from this repository. Each JSON in [`data/`](data/) names the commit it measured, and the list above says what that commit changed.

To read liblzma's assembly choice, open `range_decoder.h` in the `rangecoder` folder of xz 5.8.4's liblzma and find `LZMA_RANGE_DECODER_CONFIG`.
