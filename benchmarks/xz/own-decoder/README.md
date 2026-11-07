# xz on Zipper's own decoder

**Kept. With Zipper's own xz reader in place of liblzma, Silesia `.tar.xz` extracts 11 to 15% faster, with ranges apart in five of six cells, and dependency sources 5 to 11% faster. 7z LZMA2 extraction, which shares the changed reader, is unchanged.** One Mac, one session. The four threads gave no gain on an archive of nine blocks, because the blocks still decode one after another.

## Question

The tar reader decoded `.tar.xz`, the ZIP reader its XZ method, and the RPM reader its xz payloads through liblzma. The change moves all three to the xz reader in `src/lzma.rs`, on the decoder the [7z switch record](../../native-7z/own-decoder-shipped/README.md) put into 7z. It also reuses one input buffer, window and decoder state across the blocks of a stream, and grows the window with its output. Does extraction stay as fast, and does 7z, which shares the LZMA2 reader, stay as fast? The metric is the median wall time of `zipper extract` with the default engine, with CPU time and peak memory beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off, no agents running.
- `before` is main at `749467d`, binary SHA-256 prefix `0066b9a432069045`. `after` is the change at `5872e27`, prefix `389e256896854596`.
- Six `.tar.xz` archives in `target/bench-work/xz/`, written by XZ Utils 5.8.4 from tars made with `COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata`:
  - Silesia, 211,938,580 bytes, at `xz -T1 -6` (49,231,516 bytes), `xz -T1 -9e` (48,417,880 bytes) and `xz -T0 -6` (49,497,540 bytes, nine blocks);
  - dependency sources at the same three settings (6,120,328, 5,856,016 and 6,267,504 bytes).
- `silesia.7z` from the [7z switch record](../../native-7z/own-decoder-shipped/README.md), with 7-Zip 26.05 as the reference.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 5` ran each archive at one thread and at four, the script's default, after one warmup round, in interleaved order. It checks every extracted byte against the source tree.
- No timed run lists a busy process.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | `after` / `before` |
| --- | --- | --- | --- | ---: |
| Silesia, preset 6 | 1 | 1.505 (1.465 to 2.245) | 1.277 (1.230 to 2.959) | 0.848 |
| Silesia, preset 6 | 4 | 1.475 (1.459 to 1.611) | 1.312 (1.251 to 1.402) | 0.889 |
| Silesia, preset 9e | 1 | 1.420 (1.411 to 1.423) | 1.221 (1.217 to 1.228) | 0.860 |
| Silesia, preset 9e | 4 | 1.402 (1.389 to 1.412) | 1.209 (1.194 to 1.219) | 0.862 |
| Silesia, nine blocks | 1 | 1.426 (1.421 to 1.437) | 1.224 (1.220 to 1.234) | 0.858 |
| Silesia, nine blocks | 4 | 1.431 (1.426 to 1.435) | 1.232 (1.221 to 1.235) | 0.861 |
| Dependencies, preset 6 | 1 | 0.360 (0.342 to 0.361) | 0.338 (0.316 to 0.387) | 0.939 |
| Dependencies, preset 6 | 4 | 0.337 (0.329 to 0.341) | 0.303 (0.287 to 0.308) | 0.899 |
| Dependencies, preset 9e | 1 | 0.360 (0.344 to 0.372) | 0.333 (0.321 to 0.354) | 0.925 |
| Dependencies, preset 9e | 4 | 0.337 (0.326 to 0.376) | 0.304 (0.295 to 0.352) | 0.903 |
| Dependencies, nine blocks | 1 | 0.355 (0.343 to 0.363) | 0.338 (0.334 to 0.379) | 0.952 |
| Dependencies, nine blocks | 4 | 0.346 (0.338 to 0.348) | 0.310 (0.291 to 0.318) | 0.895 |
| `silesia.7z` | 1 | 1.155 (1.144 to 1.160) | 1.144 (1.141 to 1.160) | 0.991 |
| `silesia.7z` | 4 | 0.759 (0.749 to 0.768) | 0.746 (0.742 to 0.758) | 0.983 |

CPU time fell with wall time, from 1.43 to 1.23 s on Silesia at preset 9e and one thread. Peak memory rose by about 1 MiB on every `.tar.xz`, for example from 72 to 73 MiB at preset 9e. 7-Zip extracted `silesia.7z` in 1.040 s at one thread and 0.681 s at four.

## Decision

Kept. Silesia is faster in every cell, with ranges apart in five of six. At preset 6 and one thread one slow run in each binary, 2.245 and 2.959 s, makes the ranges overlap. Dependency sources are faster with ranges apart in two cells at four threads, and the other four overlap. `silesia.7z` overlaps in both cells, so the shared reader costs 7z nothing measurable.

The change also checks SHA-256 through the `sha2` crate, refuses an xz stream whose check Zipper cannot verify, and refuses a dictionary over the decoder budget before allocating it. Two adversarial reviews found one failure, a full-size window zero-filled for every block, fixed with a test that failed first. The second review found none.

## Limits

- One Mac and one session. Linux and Windows were not measured.
- Four threads is the script's default, not Zipper's default thread count.
- The nine-block archive decodes its blocks one after another, so four threads gain nothing there. Decoding sized blocks in parallel is not part of this change.
- The 1 MiB rise in peak memory is unexplained. One more tar chunk in flight is the builder's hypothesis, not a measurement.
- `.lzma`, lzip and the ZIP LZMA method still decode through liblzma.

## Reproduce

Build release binaries at `749467d` and `5872e27`. Write the tars with `COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -cf` from `target/bench-corpora`, and compress each with `xz -T1 -6`, `xz -T1 -9e` and `xz -T0 -6`. Then, for each archive, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/xz/silesia-6.tar.xz \
  target/bench-corpora/silesia/silesia --archive-format tar --runs 7 --cooldown 5 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER
```

Add `--reader-threads before 1 --reader-threads after 1` for one thread.
