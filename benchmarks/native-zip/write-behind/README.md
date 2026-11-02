# Write-behind for large entries

**With a spare thread, a 256 MiB entry extracts 1.62× faster when random and 1.34× faster when compressible.** CPU time changes by at most 5.4% and peak memory grows by 5 MiB. One-thread runs and archives with no spare thread are unchanged.

## Question

Native extraction of one large entry ran its reading, decoding, CRC and writing on one thread, one after another. Can a writer thread overlap the writes with the rest when a thread is otherwise idle?

The hypothesis came from an outside-timing split at one thread. `zipper test` (no writes) took 29.3 ms on the random entry and 37.7 ms on the compressible one, while `extract` took about 59 ms for both. A sampled profile of the random entry put 68% of active time in `write`. Write size and page alignment made no difference in a separate probe, so overlapping the writes was the remaining lever.

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. Release builds.
- Baseline is `81e7eb9`. The candidate is that revision plus this change.
- A streamed entry of at least 16 MiB gets a writer thread when the pool has more threads than output files. The worker decodes into 1 MiB chunks from a pool of five recycled buffers, and the writer thread writes them.
- `scripts/compare_zip_readers.py` ran both binaries with `native` at one and four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are the M3 scoreboard ZIPs for the 256 MiB random entry, the 256 MiB compressible entry and Silesia.

## Results

Medians of seven rounds, with the wall range. Raw samples are in this folder, one JSON file per workload and thread count.

| Workload | Threads | Before ms | After ms | CPU ms before / after | Peak MiB before / after |
| --- | ---: | ---: | ---: | ---: | ---: |
| 256 MiB random | 4 | 60.5 [59.6–61.4] | 37.3 [37.1–42.0] | 59.2 / 55.8 | 3.0 / 8.0 |
| 256 MiB compressible | 4 | 60.7 [60.2–63.4] | 45.2 [44.5–45.3] | 59.3 / 62.5 | 3.1 / 8.2 |
| 256 MiB random | 1 | 60.3 [59.1–110.1] | 60.2 [59.2–60.8] | 58.9 / 58.7 | 3.0 / 2.9 |
| 256 MiB compressible | 1 | 60.8 [60.4–62.8] | 60.6 [60.1–61.2] | 59.2 / 59.2 | 3.0 / 3.0 |
| Silesia | 4 | 88.8 [77.0–90.0] | 89.1 [88.0–94.4] | 260.2 / 260.1 | 4.2 / 4.2 |
| Silesia | 1 | 241.7 [240.6–247.8] | 240.8 [239.8–244.9] | 240.0 / 239.3 | 3.1 / 3.1 |

The random entry at four threads now takes about as long as `cat` copying the same 256 MiB into a file, which took 37.8 ms in the same session.

## Decision

Kept. The four-thread ranges do not overlap the baseline. CPU time fell 5.7% on the random entry and rose 5.4% on the compressible one. The extra memory is five 1 MiB buffers per large entry.

Silesia does not change, because its 12 entries already occupy every worker. One-thread rows do not change, because `--threads 1` leaves no spare thread. Their limiter is the decoder, which is the next M5 experiment.

## Limits

One Mac, warm caches, one session. The archives come from Python `zipfile` at level 6. Spare threads are counted once, before extraction starts, so an archive whose small entries finish early does not hand those threads to a large entry. Linux and Windows were not measured.

## Reproduce

```sh
cargo xtask bench --only large-random large-compressible silesia
python3 scripts/compare_zip_readers.py target/bench-work/large-random.zip \
  target/bench-corpora/large/large/random \
  --reader before native zipper-before --reader after native zipper-after \
  --runs 7 --threads 4 > large-random-4.json
```

Build `zipper-before` at `81e7eb9` and `zipper-after` at this commit.
