# Zipper's Zstandard decoder against libzstd on x86-64

**Two changes are kept and the gap is narrowed, not closed. A decode takes 0.82 to 0.91 times the time it took, with ranges apart on both corpora. That is 1.09 to 1.13 times libzstd's time, where it was 1.20 to 1.38 times. The hosted runners build libzstd for x86-64-v3 and Zipper for baseline x86-64, so part of the first gap was the C compiler's default and not the decoder.** Hosted x86-64 Linux runners, a different processor in each run, and one Apple M2 Max.

## Question

[M35](../../../docs/roadmap/m35-zstandard-and-lzma-at-c-speed-on-x86-64.md) asks why Zipper's Zstandard decoder takes 1.26 to 1.34 times libzstd's time on x86-64 Linux, and what closes the gap in safe Rust. The metric is the wall time of one decode of the corpus stream, timed inside `zipper-decode-bench decode`. Each experiment compares the build before a change with the build after it and with libzstd, in turn on one runner.

## Setup

- Hosted GitHub Actions runners, 4 cores, Linux 7.0, rustc 1.99.1 with no `RUSTFLAGS`, and GCC 15.2.0. Each run got its own processor, which the tables name. A ratio compares only rows of one run.
- The two M31 corpora and their streams. `silesia.tar` is 211,957,760 bytes and `dependencies.tar` is 90,931,200 bytes, each as one frame from libzstd at level 3 with a checksum and no content size.
- The bench reads Zipper's decoder through one `read` into a buffer of the decoded size. It calls libzstd through `zstd::bulk::Decompressor::decompress_to_buffer`, which decodes in one call.
- `m35-profile.yml` ran on branches named `m35-exp/NAME`, with the tooling in [`experiment-tooling.patch`](experiment-tooling.patch). The patch adds a `base` input and the `m35_ab.py` driver. The job builds the bench at `base` and at the branch, then runs 15 rounds after one warmup. A round runs the three rows in a new seeded order, one process a sample. It also fixes the symbol names that `perf annotate` was given, so the annotated assembly is no longer empty.
- A run with the same decoder on both sides gave 79.6 ms against 79.6 ms on the dependencies and 222.9 ms against 225.3 ms on Silesia, ranges overlapping. So the method shows no difference where there is none.
- The Apple M2 Max rows used [`m35_ab_local.py`](m35_ab_local.py), the same method with 11 rounds. It refuses to start while another process uses 25% of a core, as `scripts/bench_common.py` decides, and it records each process's peak RSS.
- Raw samples of every run are in [`data/`](data/), one JSON a run, named by experiment, corpus and workflow run.

## Results

### What the gap was made of

`perf` sampled both decoders on an AMD EPYC 9V74 in run 43988178662, where one decode took 220 ms against 166 ms. The shares are in [`data/profile-diag-base/`](data/profile-diag-base/). In milliseconds of one decode they come to this.

| Part | Zipper | libzstd | Difference |
| --- | ---: | ---: | ---: |
| Sequence loop | 141 | 110 | 31 |
| Huffman literals | 21 | 10 | 11 |
| Copies in libc | 14 | 1 | 13 |
| Sequence tables | 9 | 5 | 4 |
| Checksum | 14 | 13 | 1 |

- The runner's GCC targets x86-64-v3 by default. `cc -Q --help=target` prints `-march=x86-64-v3`, and it defines `__BMI2__` and `__AVX2__`. So libzstd's sequence loop holds `shlx`, `shrx` and `bzhi`, and libzstd compiles out its own runtime choice of a BMI2 build. Zipper's loop was built for baseline x86-64, where a shift takes its count only in `cl` and a mask costs four instructions.
- With `-C target-cpu=x86-64-v3` for the Rust code as well, run 43988183680 on an AMD EPYC 9V45, the two sequence loops took 90 ms and 87 ms. The whole decode still took 164 ms against 131 ms, because the checksum rose from 9 ms to 24 ms under AVX2.
- The copies in libc are the reader's window buffer. Zipper decoded each block into its own buffer and copied it to the caller, 212 MB for Silesia. libzstd's one call decodes where the caller wants the bytes.
- libzstd's Huffman loop on x86-64 is the assembly in `huf_decompress_amd64.S`, which `zstd-sys` builds on Linux. `perf` shows it without a symbol name.
- Zipper's loop showed about 155 sampled instructions on its common path against libzstd's 118, counted in the annotated assembly. The M2 Max has 31 registers and a one-instruction bit-field extract. x86-64 has 15 registers, so the loop's 22 live values spill, and each field of a table entry costs a move, a shift and a mask.

### Experiments

Each row is one run. Times are the median in milliseconds with the lowest and highest of 15 samples. "Before" is the build the change was made on.

| Experiment | Corpus | Processor | Before | After | libzstd | After over before | After over libzstd |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1. Sequence loop built with BMI2, chosen at run time | Silesia | Intel Xeon Platinum 8573C | 284.7, 283.3 to 288.1 | 265.8, 265.1 to 268.3 | 212.8, 211.1 to 215.8 | 0.934, apart | 1.249 |
| 1. The same | Dependencies | Intel Xeon 6975P-C | 66.2, 65.4 to 68.7 | 61.7, 60.9 to 62.3 | 48.4, 47.6 to 49.0 | 0.932, apart | 1.274 |
| 2. Blocks decoded in the caller's buffer | Silesia | Intel Xeon 6975P-C | 205.0, 203.5 to 207.9 | 182.3, 181.4 to 183.3 | 159.2, 158.1 to 161.6 | 0.889, apart | 1.145 |
| 2. The same | Dependencies | Intel Xeon 6975P-C | 62.3, 61.2 to 62.5 | 54.3, 53.7 to 56.1 | 48.4, 47.9 to 48.9 | 0.873, apart | 1.122 |
| 3. The BMI2 loop built with AVX2 as well, reverted | Silesia | AMD EPYC 9V74 | 211.9, 211.5 to 217.8 | 208.5, 207.6 to 210.0 | 164.9, 164.2 to 169.9 | 0.984, apart | 1.265 |
| 3. The same | Dependencies | AMD EPYC 9V74 | 61.6, 61.4 to 62.7 | 60.5, 60.3 to 62.1 | 48.2, 48.0 to 49.8 | 0.983, overlap | 1.255 |
| 4. The three sequence tables in one struct, reverted | Silesia | Intel Xeon 6975P-C | 187.3, 185.6 to 191.3 | 186.0, 184.2 to 189.0 | 162.0, 160.5 to 165.0 | 0.993, overlap | 1.148 |
| 4. The same | Dependencies | AMD EPYC 9V45 | 41.6, 40.5 to 44.1 | 40.9, 40.2 to 41.8 | 40.0, 38.8 to 46.1 | 0.984, overlap | 1.023 |
| 5. Table symbols spread in two passes, reverted | Silesia | AMD EPYC 7763 | 259.6, 248.9 to 265.9 | 258.1, 248.3 to 266.4 | 205.5, 201.7 to 210.4 | 0.994, overlap | 1.256 |
| 5. The same | Dependencies | AMD EPYC 9V45 | 39.5, 38.5 to 46.7 | 38.8, 38.4 to 42.5 | 37.2, 36.4 to 74.9 | 0.982, overlap | 1.042 |
| Both kept changes against the decoder before them | Silesia | AMD EPYC 9V45 | 154.9, 152.6 to 185.9 | 140.7, 137.5 to 151.2 | 129.7, 126.4 to 135.7 | 0.908, apart | 1.085 |
| The same | Dependencies | Intel Xeon 6975P-C | 65.4, 64.0 to 65.8 | 53.6, 52.6 to 54.3 | 47.6, 47.0 to 48.3 | 0.819, apart | 1.126 |

What each experiment changed.

1. `fast_sequences` runs `fast_sequences_bmi2` when `is_x86_feature_detected!("bmi2")` holds, and `fast_sequences_portable` otherwise. Both are the same loop, `sequence_loop`, compiled twice. The BMI2 build is reached through one `unsafe` call, as `src/crc32.rs` reaches its folding. Experiment 2 was made on this build, and experiments 3 to 5 on the build with both.
2. A read that starts a frame decodes its blocks into the caller's buffer while the buffer has room for each next block. When a block may not fit, the reader copies the last window of what it decoded into its window buffer and goes on there, as before. A frame read into a buffer with room for it takes no window buffer at all. The first run of this experiment measured the commit before a fix that holds back a frame's last block until its checksum passes. The last two rows measure the fixed code.
3. `#[target_feature(enable = "bmi2,avx2")]` lets the loop copy 32 bytes in one load and one store. It gained 1.6% on Silesia with ranges apart, and the ranges overlap on the dependencies, so it is not kept.
4. One struct holding the three tables lets x86-64 address them from one register. The ranges overlap on both corpora.
5. libzstd lays a table's symbols out in order and then steps through them when no symbol has a count below one. The same two passes in `spread_symbols` left the ranges overlapping on both corpora.

### Apple M2 Max

M2_MAX_RESULTS

### Peak memory

PEAK_MEMORY_RESULTS

## Decision

Experiments 1 and 2 are kept, and 3, 4 and 5 are reverted. The verdict for M35 stays "not ready yet" on these runs. A decode takes 1.09 times libzstd's time on Silesia and 1.13 times on the dependencies, with ranges apart.

What is left, from the profiles of the last two rows.

- The sequence loop still takes about 1.15 times libzstd's time on the Intel Xeon 6975P-C, and it is within the spread on the AMD EPYC 9V45. It retires more instructions a sequence than libzstd's loop. The bounds checks of its two 32-byte copies and the field extracts are what remain.
- libzstd's Huffman literals run in hand-written assembly at about half the time of Zipper's loop, 6 ms against 14 ms on Silesia. M35 accepts that gap, since Zipper uses no assembly.
- Building the sequence tables takes 6 ms against 5 ms on Silesia.

## Limits

- Every x86-64 number is from a hosted runner with 4 shared cores. One run compares its own three rows. Ratios against libzstd moved from 1.02 to 1.26 between processors for the same code, so no ratio here holds for x86-64 in general.
- The runners expose no hardware counters, except the Intel Xeon 6975P-C runs. Instruction counts a sequence come from reading the annotated assembly, not from a counter.
- libzstd was built by GCC 15.2.0 with `-march=x86-64-v3` as its default. A C compiler that targets baseline x86-64 would build libzstd's own runtime choice of BMI2 code, which no run here measured.
- Experiment 1 needs BMI2, which Intel has shipped since Haswell in 2013 and AMD since Excavator in 2015. An older processor runs the loop as before.
- Experiment 2 helps a caller whose buffer has room for at least one block of 128 KiB when a frame starts, and most when it holds the whole frame, as the bench's does. `zipper extract` reads a `.tar.zst` through smaller buffers, and no extraction was timed here.
- With experiment 2, a read may write zeros past the count it returns, inside the buffer it was given, as the [tar reader's page](../../../docs/reference/tar-reader.md) states. A frame damaged twice in one block can also report a different error when read in place.
- The x86-64-v3 build of Zipper has a slower checksum than the baseline build, 24 ms against 9 ms on Silesia. No experiment here changed that.
- Experiments 3 to 5 each moved the median by 0.6% to 1.8% in the same direction on both corpora. Hosted runners cannot tell that from no change, so each stays unproven and none is claimed.
- The sums in the first table come from sampled shares of a process that also reads its input, so each is good to a few milliseconds.

## Reproduce

Apply [`experiment-tooling.patch`](experiment-tooling.patch) to the commit to measure and push it to a branch named `m35-exp/NAME`. Then dispatch the workflow on that branch, once for each corpus, and read `ab-zstd.md` in the artifact.

```sh
git apply benchmarks/zstd/x86-64/experiment-tooling.patch
gh workflow run m35-profile.yml --ref m35-exp/NAME -f corpus=silesia -f codec=zstd \
  -f target_cpu=baseline -f repeats=5 -f base=BASE_COMMIT
gh run download RUN_ID --dir target/m35/NAME
```

Passing `-f tests=yes` runs `cargo test --release --lib zstandard` on the runner first, which is where the BMI2 build of the loop is tested. Experiments 3 to 5 were commits on `m35-exp/avx2`, `m35-exp/tables` and `m35-exp/spread`, which were deleted after their runs, so their rows cannot be rebuilt from this repository.

On the M2 Max, build `zipper-decode-bench` at the base and at the new commit, and run the local driver once for each corpus folder that `scripts/m31_decoders.py corpus` wrote.

```sh
PYTHONPATH=scripts python3 benchmarks/zstd/x86-64/m35_ab_local.py --base BASE/zipper-decode-bench \
  --new NEW/zipper-decode-bench --corpus target/m31/corpus --rounds 11 --out target/m35/ab.json
```
