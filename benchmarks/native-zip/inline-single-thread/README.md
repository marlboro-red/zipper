# Single-thread extraction on the calling thread

**Rejected.** Running a one-thread extraction on the calling thread instead of handing it to the rayon pool changed no workload beyond its range.

## Question

A sampled profile of Silesia at one thread put 4% of active samples in `pthread_cond_signal`, called from rayon's `in_worker_cold`. The calling thread is outside the pool, so every extraction wakes a worker and waits. Does skipping the pool when it has exactly one thread save that time?

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. Release builds.
- Baseline is `7e70e2b`. The candidate ran the same per-entry closure with `iter().try_for_each` on the calling thread whenever the pool had one thread.
- `scripts/compare_zip_readers.py` ran both binaries with `native` at one thread, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.

## Results

Medians of seven rounds with the wall range. Raw samples are in this folder.

| Workload | Before ms | After ms |
| --- | ---: | ---: |
| Silesia | 193.9 [193.1–194.3] | 194.3 [193.5–195.1] |
| Dependency sources | 324.7 [317.1–329.1] | 319.2 [317.5–338.7] |
| 10,000 flat tiny files | 505.7 [499.0–516.1] | 513.7 [501.2–553.6] |
| 4,000 grouped files | 211.5 [208.7–266.7] | 213.2 [206.3–251.3] |
| 256 MiB random | 58.6 [58.3–62.0] | 58.4 [58.0–59.0] |

## Decision

Rejected and reverted. Every range overlaps its baseline. The profiled signal time was spent while the calling thread waited anyway, so it was not on the critical path.

## Limits

One Mac, warm caches, one session. The profile came from `examples/profile_zip`, which runs repeated extractions in one process, not from the CLI.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py target/bench-work/silesia.zip \
  target/bench-corpora/silesia/silesia \
  --reader before native zipper-before --reader after native zipper-after \
  --runs 7 --threads 1 > silesia-1.json
```

Build `zipper-before` at `7e70e2b` and `zipper-after` with the per-entry closure run by `iter().try_for_each` when the pool has one thread.
