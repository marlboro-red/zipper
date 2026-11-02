# Larger streaming steps

**With 512 KiB streaming steps, the 256 MiB random entry extracts 1.30× faster at one thread.** The steps were 64 KiB of input and 192 KiB of output before. Silesia and the other large rows also have lower medians, but their ranges overlap, so this record does not claim them. Each worker's buffer grows from 256 KiB to 1 MiB.

## Question

At one thread, native extraction of the 256 MiB random entry took 59 ms where default bsdtar took 45 ms, and bsdtar here reads 1 MiB at a time. The streaming path read 64 KiB per step, about 4,100 reads and seeks for this entry. Do larger steps close the gap?

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. Release builds.
- Baseline is `7e00e7f`. The candidate changes two constants. The worker buffer grows from 256 KiB to 1 MiB, and its input share from 64 KiB to 512 KiB.
- `scripts/compare_zip_readers.py` ran both binaries with `native`, interleaved, after one warmup. A first pass used seven rounds at one thread. A second pass used eleven rounds for the rows the first pass left inconclusive and for four-thread rows. Every round passed the byte and tree check.

## Results

Medians with the wall range. Raw samples are in this folder. Files ending in `-r11` hold the eleven-round pass.

| Workload | Threads | Rounds | Before ms | After ms | CPU ms before / after | Peak MiB before / after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 256 MiB random | 1 | 7 | 59.0 [58.5–62.5] | 45.3 [45.0–51.8] | 57.4 / 43.9 | 2.9 / 3.7 |
| Silesia | 1 | 11 | 192.5 [190.0–194.5] | 183.8 [182.2–191.5] | 190.2 / 182.3 | not recorded |
| 256 MiB compressible | 1 | 11 | 60.0 [59.4–63.3] | 57.8 [57.1–77.2] | 58.6 / 56.4 | not recorded |
| 10,000 flat tiny files | 1 | 7 | 516.3 [488.8–528.5] | 515.7 [502.4–563.7] | 483.8 / 484.4 | 7.7 / 7.7 |
| 256 MiB random | 4 | 11 | 38.4 [36.2–42.9] | 33.8 [33.1–37.9] | 54.6 / 52.6 | not recorded |
| 256 MiB compressible | 4 | 11 | 46.3 [44.8–49.3] | 45.1 [44.8–46.2] | 61.9 / 61.7 | not recorded |
| Silesia | 4 | 11 | 68.8 [61.2–97.8] | 65.4 [64.3–92.2] | 201.5 / 190.6 | not recorded |
| Dependency sources | 4 | 11 | 181.1 [178.5–235.1] | 179.4 [178.4–195.3] | 581.5 / 580.6 | not recorded |

In the seven-round pass, Silesia at one thread went from 194.1 to 186.5 ms and peak memory from 3.0 to 4.8 MiB. Its after range ran to 214.0 ms there and to 191.5 ms in the eleven-round pass, so both passes overlap the baseline. The middle half of the eleven-round samples does not overlap.

## Decision

Kept. The random entry at one thread beats its range, and no row got slower in median or CPU time. Silesia at one thread is now at parity with bsdtar's 186.9 ms from the last scoreboard session, but this record does not claim that gain.

A 30,000-case run of `tests/zip_fuzz.rs` with seed 3000000 passed, and the fast gate passes.

## Limits

One Mac, warm caches, one session. Peak memory was not printed for the eleven-round pass, though the JSON holds it. With 64 workers, worker buffers can now hold 64 MiB instead of 16 MiB. Linux and Windows were not measured.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py target/bench-work/large-random.zip \
  target/bench-corpora/large/large/random \
  --reader before native zipper-before --reader after native zipper-after \
  --runs 7 --threads 1 > large-random-1.json
```

Build `zipper-before` at `7e00e7f` and `zipper-after` at this commit.
