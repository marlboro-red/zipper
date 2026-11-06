# LZMA plain literals with both children loaded early

**Kept one change of three. Loading both possible next probabilities of a plain literal before its bit resolves cuts the probe stream from 434.8 to 408.7 ms on an idle machine. Zipper's decoder now takes 1.03 times liblzma's time, against 1.09 before.** Eight direct bits per normalisation made the decoder slower. One `is_match` branch with one update moved nothing that the ranges can show.

## Question

A second outside review by Codex proposed three changes to the decode loop in `src/lzma.rs`:

1. A plain literal loads both children of the next bit's probability before the current bit resolves, so the load no longer waits on the compare.
2. Direct bits decode eight at a time between normalisation checks when the range allows it.
3. `is_match` compares once and each branch applies only its own probability update.

Which of them make the decoder faster? The metric is Zipper's median time on the probe in `src/lzma.rs`, with the ratio to liblzma's time in the same round beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, release test binaries.
- The stream is the probe's own. It is the first 64 MiB of the Silesia files as raw LZMA1 at preset 6 with an 8 MiB dictionary. Every round checks that both outputs equal the input bytes.
- The machine was idle, with Spotlight off and no agents running. No sample of either session saw another process at 25% CPU or more.
- Binaries, each the `decode_speed_against_liblzma` test built at one commit, with binary SHA-256 prefixes:
  - `95e69b5`, the base, `164adc228d745002`;
  - `138e57b`, the literal preload, `56777d94443b0b06`;
  - `c6e1541`, eight direct bits on top of it, `7c3533475773903e`;
  - `29cb44e`, the single `is_match` branch on top of that, `bb407ad868c1417b`;
  - `687fa7e`, the single `is_match` branch on the preload alone, labelled `fused` in the samples, `9613a183a860be23`.
- [`run-experiments.py`](run-experiments.py) ran nine rounds of the first four binaries in rotating order. [`run-fused.py`](run-fused.py) then ran nine rounds of `138e57b` and `687fa7e`. Each binary run holds three probe rounds, and the first round of each session is warmup, leaving 24 measured samples per binary.
- Each change has a Kani proof that it equals the code it replaces, and a planted break that the proof or the tests caught.

## Results

Times are in milliseconds. Raw samples are in [`samples-experiments.json`](samples-experiments.json) and [`samples-fused.json`](samples-fused.json).

| Binary | Samples | Zipper median | Zipper range | liblzma median | Ratio median |
| --- | ---: | ---: | --- | ---: | ---: |
| `95e69b5` | 24 | 434.8 | 430.8 to 437.7 | 399.1 | 1.089 |
| `138e57b` | 24 | 408.7 | 405.4 to 410.7 | 398.9 | 1.026 |
| `c6e1541` | 24 | 416.8 | 411.2 to 417.9 | 400.3 | 1.038 |
| `29cb44e` | 24 | 413.9 | 411.4 to 419.2 | 398.8 | 1.037 |

The second session isolates the single branch from the direct bits.

| Binary | Samples | Zipper median | Zipper range | liblzma median | Ratio median |
| --- | ---: | ---: | --- | ---: | ---: |
| `138e57b` | 24 | 407.7 | 406.3 to 410.2 | 399.2 | 1.022 |
| `687fa7e` | 24 | 407.3 | 401.4 to 409.4 | 398.7 | 1.018 |

In the disassembly of `decode_symbols`, the preload issues both child loads before the compare and picks one with `csel`. Before, the load waited on the bit through `cinc`, `lsl` and `ldrh`.

## Decision

Kept the literal preload. Its whole range lies below the base's, and the median fell by 26.1 ms, 6.0%.

Reverted the other two:

- Eight direct bits raised the median by 8.1 ms, with its range above the preload's. The guard and the eight straight-line steps most likely cost more than the normalisation checks they skip, since only long distances have eight direct bits left.
- The single `is_match` branch lowered the median by 0.4 ms on the preload, and the ranges overlap, so no gain is established. It adds four helpers.

The rejected commits stay on the branches `worktree-agent-a32cd2256d1607192` and `lzma-preload-fused`.

## Limits

One Mac, one stream and one session per comparison. The ratio to liblzma is now 1.03, so the 7z swap held in the [own-decoder record](../../native-7z/own-lzma-decoder/README.md) has not been re-timed. The counts of direct-bit runs that reach eight bits were not taken, so the cause of the direct-bits loss is inferred. Linux and Windows were not measured. The decoder builds only for tests, so no `zipper` run uses it yet, and the gain changes no extraction time.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at each commit with `cargo test --release --lib --no-run`, and copy each one beside the scripts as `probe-<commit>`, with `687fa7e` copied as `probe-fused`. The Silesia files must be in `target/bench-corpora/silesia/silesia` under the directory the binaries were built in. Then run `python3 run-experiments.py` and `python3 run-fused.py`.
