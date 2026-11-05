# LZMA decoder hot loop

**Kept nine changes. Zipper's own LZMA decoder now decodes 64 MiB of Silesia 1.56× faster than its first version, in 436.9 against 681.1 ms.** It is still 1.09× slower than liblzma 5.8.4 in the same rounds. 7-Zip 26.05 took about 310 ms in a separate process timing, so the M21 goal is not met. No reader uses the decoder yet.

## Question

M21 asks for an LZMA decoder of Zipper's own that reaches 7-Zip's speed at one thread. The first version, written from the LZMA SDK specification, decoded the probe stream in 675 ms against 400 ms for liblzma. Which single changes to the decode loop close that gap? The metric is the median of the probe in `src/lzma.rs`, which decodes one stream with both decoders in each round.

## Setup

- Apple M5, macOS 26.7.1, load average 1.48 at the start. Release test binaries.
- The stream is the first 64 MiB of the Silesia files, in `read_dir` order, as raw LZMA1 at preset 6 with an 8 MiB dictionary, `lc=3`, `lp=0` and `pb=2`. liblzma writes it.
- `first` is the first version, binary SHA-256 prefix `a0b54403baa2`. `shipped` is `80af2b5`, prefix `0a00db75e639`. `chunked` is this change, prefix `fd98dcf8a3d6`. Each binary is the `decode_speed_against_liblzma` test.
- Each experiment was screened with one run of the probe, which has three rounds. The final comparison ran the three binaries in rotation, four times each, for 12 rounds per decoder and 36 for liblzma.
- Every round checks that both outputs equal the input bytes.
- Profiles came from `sample` on the `decode_loop_for_profiling` test, and from `objdump` of the release test binary, outside timed runs.
- 7-Zip 26.05 and xz 5.8.4 decoded the same stream as a `.lzma` file, written by `xz --format=lzma`, at one thread into `/dev/null`. `/usr/bin/time` measured seven runs of each, after one byte check against the input.

## Results

Final comparison, median and range in milliseconds. Raw samples are in [`samples.txt`](samples.txt).

| Decoder | Rounds | Median | Range |
| --- | ---: | ---: | --- |
| `first` | 12 | 681.1 | 678.3 to 685.4 |
| `shipped` | 12 | 458.0 | 456.8 to 459.7 |
| `chunked` | 12 | 436.9 | 433.9 to 456.5 |
| liblzma 5.8.4 | 36 | 400.9 | 399.4 to 431.6 |

Separate process timing, seven runs each. All seven 7-Zip runs took 0.31 s of wall and user time. All seven xz runs took 0.41 s of wall time and 0.40 s of user time.

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
| 13. Decode from a local `Bits` copy, refilled only when under 64 bytes remain | Range, code and position go to memory after every bit | 538 | 461 | Kept |
| 14. Copy state and reps into a local | Probability stores force reloads of the state | 461 | 467 | Reverted |
| 15. Fixed arrays for the length decoder's low and middle tables | Two `Vec` loads per length | 461 | 458 | Kept |
| 16. Copy matches of distance 16 or more in 16-byte chunks that may run into 16 bytes of slack | `memmove` calls on short matches cost time | 458 | 431 | Kept |
| 17. The same copy with `ptr::copy_nonoverlapping` | Bounds checks on the chunks cost time | 431 | 433 | Reverted |

## Decision

Kept experiments 2, 3, 4, 6, 7, 10, 13, 15 and 16, which each moved the median beyond the ranges. Experiment 13 was found from the disassembly. The probability tables share a struct with the range coder, so LLVM stored `range` and `code` after every bit. Experiment 8 is the specification's own form and no more complex, so it stays without a speed claim. Experiment 11 stays for LZMA2, which must see a previous byte of 0 after a dictionary reset. A cached byte can reset with the window, where `back(1)` would read a stale byte. Experiments 1, 5, 9, 12, 14 and 17 were reverted. Experiment 17 shows that unsafe code buys nothing here, because the compiler already removes the bounds checks on fixed 16-byte slices. Experiment 16 keeps 16 bytes of slack past the dictionary, so a chunk's overrun lands only on bytes no match can reach. `chunked_match_copy_matches_a_byte_at_a_time_copy_in_every_window_state` checks every window state of four sizes against a byte-at-a-time copy. A Kani proof of the same copy ran out of memory after 22 minutes, so the exhaustive test replaced it.

## Limits

One Mac and one stream, from one session with warm caches. The screening runs have three rounds each and were not interleaved. Experiments 7, 8, 11 and 15 moved the median by less than 2%, which is inside the drift seen between sessions in other records. The 7-Zip and xz times include process start and reading the file, and their timer has 10 ms resolution, so they compare only roughly with the probe. Linux and Windows were not measured, and other encoder settings were not tried.

## Reproduce

```sh
cargo test -q --release --lib decode_speed -- --ignored --nocapture
```

The Silesia files must be in `target/bench-corpora/silesia/silesia`. For the comparison, build the test binary once at the first version of `src/lzma.rs` and once at this change, then run each four times, alternately, with `decode_speed --ignored --nocapture`.
