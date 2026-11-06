# LZMA decoder state kept in registers

**Kept one change. With the range coder's state owned by the decode kernel, Zipper's LZMA decoder takes 1.09 times liblzma's time on the probe stream, against 1.29 times before, and 434.3 ms on an idle machine.** The first session was loaded by other agents, so its verdict rests on the ratio to liblzma in the same round. An idle re-time rejected the three further changes. Two moved nothing, and the third overlaps its parent's range.

## Question

An outside review of `src/lzma.rs` by Codex proposed several changes to the decode loop. The disassembly of the release test binary at `da108d5` showed `range` and `code` stored to memory after every decoded bit, the cost that experiment 13 of the [hot-loop record](../hot-loop/README.md) had removed once. Which of the proposed changes make the decoder faster? The metric is the ratio of Zipper's time to liblzma's time for the same stream in the same round of the probe in `src/lzma.rs`.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, release test binaries.
- The stream is the probe's own. It is the first 64 MiB of the Silesia files, in `read_dir` order, as raw LZMA1 at preset 6 with an 8 MiB dictionary. Every round checks that both outputs equal the input bytes.
- Five binaries, each the `decode_speed_against_liblzma` test built at one commit, with binary SHA-256 prefixes:
  - `da108d5`, the base, `b2f9d8a3324ae9bb`;
  - `d6b83f6`, the kernel owns `Bits` and returns its state on every exit, `02e95f2c1cd709b5`;
  - `9656259`, the plain literal in eight explicit steps, `2d6623c2655a1c07`;
  - `53ab48a`, the matched literal in eight explicit steps, `229261a866acd71b`;
  - `fbead1c`, the probability update toward one selected target, `80553efccd4b9931`.
- [`run.py`](run.py) ran eight rounds of the five binaries in rotating order. Each binary run holds three probe rounds. The first rounds are warmup, leaving 21 measured samples per binary.
- Before each binary run, the script recorded every other process at 25% CPU or more. 78 of the 120 samples saw one, mostly `rustc`, `cbmc` and a test binary from another agent's build. A Spotlight indexer that had held one core for eight days was stopped before the run.
- An adversarial review compared the four commits with `da108d5` on about 1.78 million decoder runs, covering bytes, errors, the read each error arrives on, the reads after it and the decoder's internal state. All were identical.

## Results

Raw samples are in [`samples.json`](samples.json). Times are in milliseconds.

| Binary | Samples | Zipper median | liblzma median | Ratio median | Ratio range |
| --- | ---: | ---: | ---: | ---: | --- |
| `da108d5` | 21 | 519.7 | 403.2 | 1.287 | 1.272 to 1.634 |
| `d6b83f6` | 21 | 503.3 | 461.7 | 1.091 | 1.074 to 1.199 |
| `9656259` | 21 | 482.2 | 447.5 | 1.088 | 0.606 to 1.200 |
| `53ab48a` | 21 | 470.4 | 433.4 | 1.087 | 1.068 to 1.155 |
| `fbead1c` | 21 | 479.1 | 441.6 | 1.074 | 0.945 to 1.195 |

The absolute times swing with the load. On the three samples of `da108d5` that saw no other busy process, Zipper took 514.0 to 519.7 ms against 399.8 to 403.7 ms for liblzma. The hot-loop record measured 436.9 ms for the decoder of that time, so the stores it removed had come back by `da108d5`. When they came back is not bisected.

A second session ran on an idle machine, with Spotlight off and no agents running. 3 of its 108 samples saw another process at 25% CPU or more, and that process was the session's own shell. [`run.py`](run.py) ran nine rounds of `d6b83f6` and the three changes on top of it, in rotating order, leaving 24 measured samples each. Raw samples are in [`samples-quiet.json`](samples-quiet.json).

| Binary | Samples | Zipper median | Zipper range | liblzma median | Ratio median |
| --- | ---: | ---: | --- | ---: | ---: |
| `d6b83f6` | 24 | 434.3 | 430.8 to 436.9 | 399.2 | 1.088 |
| `9656259` | 24 | 434.5 | 431.8 to 438.0 | 399.1 | 1.088 |
| `53ab48a` | 24 | 435.5 | 432.6 to 438.2 | 401.2 | 1.086 |
| `fbead1c` | 24 | 431.6 | 427.5 to 434.1 | 400.6 | 1.076 |

The disassembly of `decode_symbols` shows the change. At `da108d5` it holds 2,256 instructions and 221 stores. At `d6b83f6` it holds 1,972 instructions and 88 stores, and the matched-literal loop keeps `range`, `code` and the input position in registers.

## Decision

Kept `d6b83f6`. Its whole ratio range lies below the base's. Passing `Bits` by value was not enough, because the ABI passes a 48-byte struct by pointer, so the kernel moves it into its own local and returns its parts on every exit. The LZMA2 error-latch fix found by the same review ships with it.

`9656259`, `53ab48a` and `fbead1c` were reverted. On the idle machine the two literal unrollings moved the median by less than 1.3 ms, inside the ranges, and `53ab48a` also grows the kernel by 231 instructions. `fbead1c` lowered the median by 3.9 ms, 0.9%, but its range overlaps the range of `53ab48a`, so no gain is established. `d6b83f6` decodes the probe in 434.3 ms on the idle machine, back to the 436.9 ms the hot-loop record measured.

## Limits

One Mac and one stream, in one loaded session and one idle session. The ratio to liblzma cancels load that hits both decoders alike, but not load that lands on one of them, which is why some ranges reach 1.6 or fall to 0.6. 7-Zip was not measured in this session. Linux and Windows were not measured. No reader uses the decoder yet.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at each commit with `cargo test --release --lib --no-run`, and copy each one beside [`run.py`](run.py) as `probe-<commit>`. The Silesia files must be in `target/bench-corpora/silesia/silesia` under the directory the binaries were built in. Then run `python3 run.py`, which writes `samples.json`.
