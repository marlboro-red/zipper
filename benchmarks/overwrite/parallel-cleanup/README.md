# Parallel staging cleanup after an overwrite

**Reverted both changes. Emptying the staging folder on Rayon after a successful merge left `--overwrite` wall time unchanged and raised CPU time by about half. One `lstat` fewer per replaced file showed no difference beyond the spread.**

## Question

The [rollback cost record](../rollback-cost/README.md) found `--overwrite` on dependency sources 20% slower since the undo journal. A syscall loop put about 100 ms per 4,000 files in removing the old files the swap leaves in staging, which `TempDir` does serially. Two changes tried to win the time back:

1. `d3bdbd5` empties the staging folder on Rayon after a successful merge, relative to open folder handles, down to 16 levels.
2. `b39a7fd`, on top of it, reads a swapped-out file's metadata once instead of twice.

Does either make `--overwrite` faster? The metric is the median wall time of `zipper extract --overwrite` into a full copy of the archive's tree, with CPU time beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, APFS. Spotlight indexing off, no agents running.
- Three binaries, with their SHA-256 prefixes:
  - `cee92b3`, main, `075a223f4555218f`;
  - `d3bdbd5`, `75dcb8310fadc721`;
  - `b39a7fd`, `e288ce3bf2d4e753`.
- The fixtures and [`run.py`](run.py) are those of the rollback cost record. `BINARIES="cee92b3 d3bdbd5 b39a7fd"` rotates the three binaries each round. Ten rounds, the first warmup, leave nine samples per cell. `diff -r` checks every file after each run.
- No timed run saw another process at 25% CPU or more.

## Results

Median wall time in seconds, with the range over nine runs, and median CPU time. Raw samples are in [`samples-speedup.json`](samples-speedup.json).

| Fixture | `cee92b3` | `d3bdbd5` | `b39a7fd` | CPU `cee92b3` | CPU `d3bdbd5` | CPU `b39a7fd` |
| --- | --- | --- | --- | ---: | ---: | ---: |
| flat-tiny | 1.094 (1.061 to 1.165) | 1.115 (1.068 to 1.173) | 1.086 (1.073 to 1.194) | 1.782 | 2.517 | 2.483 |
| dependencies | 0.858 (0.840 to 0.911) | 0.854 (0.834 to 0.958) | 0.847 (0.816 to 0.915) | 1.758 | 2.637 | 2.606 |

## Decision

Reverted both. The parallel removal spent 0.7 to 0.9 s more CPU for no change in wall time. Unlinks in one folder most likely contend for that folder on APFS, so more threads add work without finishing sooner. The single `lstat` overlaps main's range, so it does not earn a change to code the overwrite check covers. The commits stay on the branch `worktree-agent-a716c202a2596d340`.

## Limits

One Mac and one session, on APFS. Linux filesystems may delete in parallel differently, and were not measured. The cause of the contention is inferred, not profiled.

## Reproduce

Build release binaries at `cee92b3`, `d3bdbd5` and `b39a7fd`, and copy each beside [`run.py`](run.py) under its commit name, with the two ZIPs of the rollback cost record. Then run `BINARIES="cee92b3 d3bdbd5 b39a7fd" SAMPLES=samples-speedup.json python3 run.py 10`.
