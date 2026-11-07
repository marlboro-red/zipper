# LZMA kernel changes from a study of 7-Zip's decoder

**Kept two changes of three. 7-Zip's probability update and a 1,024-entry literal coder made the probe decode 1.5% and 1.4% faster, and 7z Silesia extracts about 3.5% faster with ranges apart. Holding the loop's state in locals made it 19.5% slower and was reverted.** 7-Zip still extracts solid Silesia about 11% faster at one thread. All runs are on one Apple M2 Max.

## Question

A machine-level study of 7-Zip 26.05 found that its hand-written arm64 decoder runs no shorter a chain between bits than Zipper's, but retires 3.00 G instructions on the probe stream against Zipper's 4.80 G. It ranked ten ways to remove instructions. Which of the top three make the decoder faster? The metric is Zipper's median time on the probe in `src/lzma.rs`, with the ratio to liblzma's time in the same round beside it. The second metric is the median wall time of `zipper extract`, with 7-Zip in the same rotation.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0, release builds. A builder machine with nothing else of this project running on it. macOS background services, mainly `dasd` and Spotlight, used 25% CPU or more during most probe samples, so the ratio to liblzma in the same round is the main comparison.
- Four probe binaries, each the `decode_speed_against_liblzma` test built at one commit, with binary SHA-256 prefixes:
  - `9802d0b`, the base, `e771e587ee0b1b7c`;
  - `f179ddb`, change 1, `ab29b55b48bd5814`;
  - `2d61052`, change 2, `efca3b903c61767a`;
  - `2be258a`, change 3, `4c8084bca14d78f8`.
- The four ran in rotating order for nine rounds of three probe rounds each. The first round is warmup, which leaves 24 samples per binary.
- For extraction, `before` is main at `49cd511`, prefix `3a93c60bb703a942`, and `after` is changes 1 and 2 on it at `a658ff2`, prefix `2c0f4d819f1f1c29`. `scripts/compare_zip_readers.py --runs 7 --cooldown 5 --allow-busy` ran `silesia.7z`, `silesia-lzma1.7z` and `silesia-9e.tar.xz` from the [7z switch](../../native-7z/own-decoder-shipped/README.md) and [xz](../../xz/own-decoder/README.md) records. 7-Zip 26.05 ran in the same rotation on the 7z archives. Every run checks each extracted byte.
- Change 1 is covered by an exhaustive test of all 2,049 probabilities and both bits, and by two Kani proofs. Change 2 is covered by Kani proofs of the child indices and an exhaustive test of all 65,536 match byte and symbol pairs. Change 3 was covered by a lockstep differential against the old loop. An adversarial review of about 1,050 archives found no failure.

## Results

Times are in milliseconds for the probe and seconds for extraction. Raw samples are in [`samples-kernel.json`](samples-kernel.json) and the JSON files beside it.

| Binary | Retired instructions | Probe median and range | Ratio to liblzma |
| --- | ---: | --- | ---: |
| `9802d0b`, base | 4.80 G | 453.3, 452.2 to 456.7 | 0.899 |
| `f179ddb`, change 1 | 4.37 G | 446.5, 445.7 to 449.1 | 0.886 |
| `2d61052`, change 2 | 4.34 G | 440.2, 439.1 to 442.8 | 0.873 |
| `2be258a`, change 3 | 4.18 G | 526.1, 524.5 to 534.8 | 1.043 |

Extraction, with changes 1 and 2:

| Archive | Threads | `before` | `after` | 7-Zip |
| --- | --- | --- | --- | --- |
| `silesia.7z`, LZMA2 | 1 | 1.535 (1.530 to 1.539) | 1.480 (1.478 to 1.486) | 1.330 |
| `silesia.7z`, LZMA2 | 4 | 1.015 (1.011 to 1.018) | 0.980 (0.978 to 0.987) | 0.885 |
| `silesia-lzma1.7z` | 1 | 1.546 (1.543 to 1.549) | 1.491 (1.489 to 1.533) | 1.339 |
| `silesia-9e.tar.xz` | 1 | 1.615 (1.614 to 1.619) | 1.562 (1.559 to 1.567) | not run |

Peak memory did not change. It was 39 MiB at one thread and 320 MiB at four, against 7-Zip's 36 and 257 MiB.

## Decision

Kept changes 1 and 2. Each lies wholly below its parent, and together they cut extraction time by 3.5% with ranges apart on every archive.

Reverted change 3. It retired 4% fewer instructions and took 19.5% longer. Holding the state machine's state, the pending match, the properties and the dictionary in locals leaves `decode_symbols` with more live values than arm64 has registers, so values on the bit chain spill. The builder had already found that also holding the reps or a window cursor added instructions. The commits stay on the branch `worktree-agent-af7827b0503240590`.

## Limits

- One machine, the M2 Max, and one session. The earlier records ran on an M5. Linux and Windows were not measured.
- macOS background services were busy during most probe samples. The rotation and the ratio to liblzma in the same round guard against that, and every range is tight.
- Four threads is the script's default, not Zipper's default thread count.
- The study's predicted saving for change 2 did not hold. Matched literals went from 50 to 43 instructions per bit, not about 22, because the index arithmetic stayed.
- The four slow Kani groups that changes 1 and 2 touch are owed under the project's two-speed rule, and run before the milestone closes.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at each commit with `cargo test --release --lib --no-run`, and copy each beside [`run-kernel.py`](run-kernel.py) as `probe-<commit>`. Then run `python3 run-kernel.py`. The Silesia files must be in `target/bench-corpora/silesia/silesia` under the directory the binaries were built in. For extraction, build release binaries at `49cd511` and `a658ff2` and run `scripts/compare_zip_readers.py` as Setup describes.
