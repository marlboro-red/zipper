# LZMA decoder hot loop

**Kept six changes. Zipper's own LZMA decoder now decodes 64 MiB of Silesia 1.27× faster than its first version, in 537.2 against 680.4 ms.** It is still 1.34× slower than liblzma 5.8.4, which took 400.8 ms. The decoder is not yet wired into any reader, so no extraction changed.

## Question

M21 asks for an LZMA decoder of Zipper's own that reaches 7-Zip's speed at one thread. The first version, written from the LZMA SDK specification, decoded the probe stream in 675 ms against 400 ms for liblzma. Which single changes to the decode loop close that gap? The metric is the median of the probe in `src/lzma.rs`, which decodes one stream with both decoders in each round.

## Setup

- Apple M5, macOS 26.7.1, load average 1.76 at the start. Release test binaries.
- The stream is the first 64 MiB of the Silesia files, in `read_dir` order, as raw LZMA1 at preset 6 with an 8 MiB dictionary, `lc=3`, `lp=0` and `pb=2`. liblzma writes it.
- `before` is the first version, binary SHA-256 prefix `37b8dd1c13b1`. `after` is this change, prefix `a0b54403baa2`. Each binary is the `decode_speed_against_liblzma` test.
- Each experiment was screened with one run of the probe, which has three rounds. The final comparison ran the two binaries alternately, four times each, for 12 rounds per decoder and 24 for liblzma.
- After the measurement, review renamed two types and one field, and removed the unused `Window::reset`. The decode loop is the one measured.
- Every round checks that both outputs equal the input bytes.
- Profiles came from `sample` on the `decode_loop_for_profiling` test, outside timed runs.

## Results

Final comparison, median and range in milliseconds. Raw samples are in [`samples.txt`](samples.txt).

| Decoder | Rounds | Median | Range |
| --- | ---: | ---: | --- |
| `before` | 12 | 680.4 | 677.1 to 681.9 |
| `after` | 12 | 537.2 | 536.3 to 538.3 |
| liblzma 5.8.4 | 24 | 400.8 | 399.3 to 405.8 |

Screening runs, three rounds each, in order. Each row compares with the last kept version.

| Experiment | Hypothesis | Median before | Median after | Verdict |
| --- | --- | ---: | ---: | --- |
| 1. Infallible byte reads with a cold refill | `io::Result` on every bit costs the most | 675 | 711 | Reverted |
| 2. Copy output with two slice copies | A `%` per output byte costs time | 675 | 646 | Kept |
| 3. Copy matches in runs with `copy_within` | One `put` per match byte costs time | 646 | 639 | Kept |
| 4. Refill input once per symbol, with 64 zero bytes of padding | A branch per input byte costs time | 639 | 604 | Kept |
| 5. Fixed-size arrays for trees and literal coders | Bounds checks cost time | 604 | 609 | Reverted |
| 6. Branchless bit decode | Literal bits mispredict | 604 | 576 | Kept |
| 7. Branching decode for control bits only | Control bits predict well | 576 | 569 | Kept |
| 8. Matched literal by offset mask | The early exit mispredicts | 569 | 566 | Kept as neutral |
| 9. Byte loop for runs under 16 bytes | `memmove` calls cost 9% of samples | 566 | 565 | Reverted |
| 10. Inline trees and length decoders | Calls cost time | 566 | 539 | Kept |
| 11. Cache the previous byte | Two branches per literal cost time | 539 | 538 | Kept as neutral |
| 12. Copy a match as soon as it decodes | The second pass through `pending` costs time | 538 | 544 | Reverted |

## Decision

Kept experiments 2, 3, 4, 6, 7 and 10, which each moved the median beyond the ranges. Experiment 8 is the specification's own form and no more complex, so it stays without a speed claim. Experiment 11 stays for LZMA2, which must see a previous byte of 0 after a dictionary reset. A cached byte can reset with the window, where `back(1)` would read a stale byte. Experiments 1, 5, 9 and 12 were reverted.

## Limits

One Mac and one stream, from one session with warm caches. The screening runs have three rounds each and were not interleaved. Experiments 7, 8 and 11 moved the median by less than 2%, which is inside the drift seen between sessions in other records. 7-Zip was not measured here, so the M21 goal remains unproven. Linux and Windows were not measured, and other encoder settings were not tried.

## Reproduce

```sh
cargo test -q --release --lib decode_speed -- --ignored --nocapture
```

The Silesia files must be in `target/bench-corpora/silesia/silesia`. For the comparison, build the test binary once at the first version of `src/lzma.rs` and once at this change, then run each four times, alternately, with `decode_speed --ignored --nocapture`.
