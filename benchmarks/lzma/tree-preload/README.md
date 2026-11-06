# LZMA bit trees with both children loaded early

**Kept four changes of five. They shorten the chain between consecutive bits in every bit tree, the matched literal and the control bits. Zipper's LZMA decoder now takes 350.0 ms on the probe stream on an idle machine against 398.4 ms for liblzma in the same runs, a ratio of 0.878.** The decoder still builds only for tests, so no extraction is faster yet.

## Question

A machine-level study of liblzma 5.8.4 and Zipper's decoder found the remaining gap in the chain of dependent instructions from one bit's result to the next bit's multiply. liblzma's C macros index the next probability only after the current bit resolves, so each tree bit waits for its compare, select, load and multiply. Zipper's plain literal already loads both children early, the change [kept before](../literal-preload/README.md). The study proposed the same shape for five more paths:

1. `Bits::tree`, the distance slot and length trees.
2. `reverse_tree`, the align and special-distance bits, with the last bit outside the loop.
3. The matched literal, loading both candidate probabilities and selecting one.
4. The control bits, normalising inside each arm so that LLVM keeps a real branch.
5. The literal context, holding the previous byte and the `lc` and `lp` shifts in locals.

Which of them make the decoder faster? The metric is Zipper's median time on the probe in `src/lzma.rs`, with the ratio to liblzma's time in the same round beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, release test binaries. Spotlight indexing off, no agents running.
- The stream is the probe's own. It is the first 64 MiB of the Silesia files as raw LZMA1 at preset 6 with an 8 MiB dictionary. Every round checks that both outputs equal the input bytes.
- Six binaries, each the `decode_speed_against_liblzma` test built at one commit, with binary SHA-256 prefixes:
  - `7dba90c`, the base, `a3e508b80019bdcb`;
  - `cc269ce`, change 1, `ef48befa4f93ab7b`;
  - `279f756`, change 2, `be6b89c7b186fbbf`;
  - `4e635ea`, change 3, `2985910f375300c8`;
  - `5741daa`, change 4, `f7d22a6663601980`;
  - `e39d515`, change 5, `4f1eacd64d860f3c`.
- [`run-trees.py`](run-trees.py) ran nine rounds of all six in rotating order. Then each commit ran against its parent alone, alternating, for nine rounds. Each binary run holds three probe rounds, and the first round of each session is warmup, leaving 24 measured samples per binary.
- In the six-way session, 33 of 162 samples saw another process at 25% CPU or more, mostly Firefox. Firefox was closed before the pairs ran, and the pairs saw 3 such samples.
- Each change has a Kani proof that it equals the code it replaces, and a planted break that the proof or the tests caught.

## Results

Times are in milliseconds. Raw samples are in [`samples-trees.json`](samples-trees.json) and the `samples-pair-*.json` files.

The six-way session:

| Binary | Samples | Zipper median | Zipper range | liblzma median | Ratio median |
| --- | ---: | ---: | --- | ---: | ---: |
| `7dba90c` | 24 | 411.0 | 406.8 to 421.1 | 402.6 | 1.020 |
| `cc269ce` | 24 | 378.0 | 371.7 to 383.1 | 403.1 | 0.936 |
| `279f756` | 24 | 368.6 | 365.9 to 374.2 | 402.6 | 0.916 |
| `4e635ea` | 24 | 356.2 | 353.7 to 373.8 | 403.2 | 0.884 |
| `5741daa` | 24 | 353.3 | 349.4 to 364.4 | 402.5 | 0.877 |
| `e39d515` | 24 | 349.9 | 345.5 to 359.4 | 401.2 | 0.874 |

Each commit against its parent:

| Change | Parent median and range | Change median and range | Ratio, parent to change |
| --- | --- | --- | --- |
| 2, `279f756` | 377.2, 372.0 to 392.7 | 367.9, 365.7 to 373.6 | 0.938 to 0.917 |
| 2, `279f756`, second run | 375.4, 373.3 to 378.7 | 367.8, 365.0 to 369.5 | 0.937 to 0.915 |
| 3, `4e635ea` | 366.2, 363.3 to 367.4 | 354.0, 351.7 to 356.8 | 0.917 to 0.885 |
| 4, `5741daa` | 353.1, 351.6 to 355.2 | 350.0, 348.2 to 351.4 | 0.886 to 0.878 |
| 5, `e39d515` | 350.4, 348.6 to 354.5 | 346.7, 344.3 to 350.2 | 0.878 to 0.868 |

In the disassembly of `decode_symbols`, change 1 replaces `cinc`, `lsl` and `ldrh` on the chain with one `csel`. Change 3 replaces seven instructions with one `csel`. Change 4 leaves `subs` and `b.hs` straight after the `mul`, and the `csel` count falls from 250 to 220.

## Decision

Kept changes 1 to 4. Change 1 lies wholly below the base, and changes 3 and 4 below their parents. Change 2 touched its parent's range by 1.6 ms in the first pair and lay wholly below it in the second.

Reverted change 5. Its range overlaps its parent's, it adds three stack spills, and no proof covers which previous byte the kernel passes. It stays on the branch `worktree-agent-ad8bfd2565a74bfbc`.

## Limits

One Mac and one stream. The six-way session ran with Firefox busy in a fifth of its samples. The ratio to liblzma cancels load that hits both decoders alike, and the pairs ran without Firefox. Linux and Windows were not measured. The decoder builds only for tests, so the 7z swap held in the [own-decoder record](../../native-7z/own-lzma-decoder/README.md) must be timed again before any extraction gains.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at each commit with `cargo test --release --lib --no-run`, and copy each beside [`run-trees.py`](run-trees.py) as `probe-<commit>`. The Silesia files must be in `target/bench-corpora/silesia/silesia` under the directory the binaries were built in. Then run `python3 run-trees.py`. For a pair, set `shas` in a copy of the script to the parent and the change, and use `rnd%2` for the rotation.
