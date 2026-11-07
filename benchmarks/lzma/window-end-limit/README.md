# LZMA output limit stopped at the window end

**Reverted. Stopping the decode kernel at the window end, so that `put` and the match copy skip their wrap test, retired 1.8% fewer instructions and left the probe time unchanged, with ranges overlapping.** The change also made a kernel entered at the window end spin instead of fail. One Apple M2 Max.

## Question

The [7-Zip study kernel record](../7zip-study-kernel/README.md) kept two changes and left 7-Zip about 11% faster at one thread. The study's change 9 passes `decode_symbols` an output limit no further than the window end, so the hot path never tests for a wrap. Does it make the decoder faster? The metric is Zipper's median time on the probe in `src/lzma.rs`, with the ratio to liblzma's time in the same round beside it.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0. macOS background services were busy during most samples, as in the kernel record.
- Two probe binaries, the `decode_speed_against_liblzma` test built at each commit:
  - `7018fa5`, the two kept kernel changes, binary SHA-256 prefix `b0a4a7de25f3e4cb`;
  - `8b3b3f2`, the change, prefix `ae747b2efa062300`.
- Nine rounds in alternating order, three probe rounds each, the first round warmup, leave 24 samples per binary.
- The change's builder counted 4,356 M net retired instructions before and 4,277 M after. Two planted breaks were caught by the `[lzma]` tests.

## Results

Times are in milliseconds. Raw samples are in [`samples-window-end.json`](samples-window-end.json).

| Binary | Probe median and range | Ratio to liblzma |
| --- | --- | ---: |
| `7018fa5` | 441.0, 440.1 to 444.6 | 0.875 |
| `8b3b3f2` | 440.1, 439.6 to 442.1 | 0.875 |

## Decision

Reverted. The ranges overlap and the ratio to liblzma is the same, so no gain is established. The change also moves the truncation check and makes a kernel entered with no room loop forever, which is a worse failure than the panic it replaces. The commit stays on the branch `lzma-kernel-round2`.

## Limits

One machine and one session, with background services active. The study's changes 4 and 10 were not tried. Change 4, a `subs` form, cannot be written in safe Rust, because LLVM rewrites every safe form back to `cmp`, `csel` and `sub`. Change 10 needs a proof that one symbol reads at most a bounded number of bytes, which does not exist yet.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at `7018fa5` and `8b3b3f2` with `cargo test --release --lib --no-run`, and copy each beside the kernel record's [`run-kernel.py`](../7zip-study-kernel/run-kernel.py) as `probe-<commit>`. Set `shas` to the two commits and the rotation to `rnd%2`, then run it.
