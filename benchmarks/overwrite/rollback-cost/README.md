# Cost of undoing a failed overwrite

**The undo journal slows `--overwrite` by about 20% on dependency sources, from 0.710 to 0.851 s with ranges apart, and leaves extraction into a new folder unchanged.** On 10,002 tiny files the median rose 8%, but the ranges overlap, so that rise is not established.

## Question

Since 6b9ca7d, a merge into an existing folder records each rename and undoes them in reverse when the publish fails. Where the filesystem swaps two names, a replaced file is swapped with its staged copy, so the old file survives until the publish succeeds. What does this cost? The metric is the median wall time of `zipper extract --overwrite` into a full copy of the archive's tree, with CPU time beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, APFS. Spotlight indexing off, no agents running.
- `before` is `4b66e85`, the last commit before the undo journal, binary SHA-256 prefix `71fc7b2bf8e65bce`. `after` is `cee92b3`, prefix `075a223f4555218f`.
- Two ZIPs written by Info-ZIP `zip -r` from `target/bench-corpora`:
  - `flat-tiny.zip`, 10,002 files, 3,522,704 bytes;
  - `dependencies.zip`, 3,981 files, 14,521,415 bytes.
- [`run.py`](run.py) clones the source tree into a fresh folder under `target/bench-work` with `cp -cR`. It then waits until no other process uses 25% CPU or more, and times the extraction alone. Afterwards `diff -r` checks every file against the source, outside the timed part. The binaries alternate order every round. Ten rounds, the first warmup, leave nine samples per cell.
- With `MODE=fresh`, the same script extracts into a new folder without `--overwrite`.
- No timed run saw another busy process.

## Results

Median wall time in seconds, with the range over nine runs, and median CPU time. Raw samples are in [`samples.json`](samples.json) and [`samples-fresh.json`](samples-fresh.json).

| Fixture | Mode | `before` | `after` | CPU `before` | CPU `after` |
| --- | --- | --- | --- | ---: | ---: |
| flat-tiny | `--overwrite` | 1.029 (0.994 to 1.123) | 1.110 (1.079 to 1.182) | 1.680 | 1.810 |
| dependencies | `--overwrite` | 0.710 (0.698 to 0.798) | 0.851 (0.821 to 0.927) | 1.648 | 1.770 |
| flat-tiny | new folder | 0.306 (0.286 to 0.337) | 0.304 (0.282 to 0.345) | 1.056 | 1.077 |
| dependencies | new folder | 0.247 (0.236 to 0.330) | 0.248 (0.238 to 0.288) | 1.313 | 1.309 |

A syscall loop over 4,000 files of 2,000 bytes on the same disk, outside the timed runs, splits the cost. A replacing `rename` took 259 to 317 ms. The swap took 214 ms, three `lstat` calls 19 to 23 ms, and deleting the swapped-out old files 97 to 109 ms. The staging folder's removal deletes those old files one by one on the main thread.

## Decision

Kept the undo journal. A failed merge now restores what the destination held, and that guarantee is worth its cost. The next experiment tries to win the time back by removing the staging folder in parallel and by dropping any per-file check that the swap already proves.

## Limits

One Mac and one session, on APFS, where the swap path runs. Filesystems without a swap take the two-move path, which was not timed. Linux and Windows were not measured. The split of the cost comes from a Python syscall loop, not from a profile of the extraction.

## Reproduce

Build release binaries at `4b66e85` and `cee92b3` and copy them beside [`run.py`](run.py) as `before` and `after`. Write `flat-tiny.zip` and `dependencies.zip` beside it with `zip -qr` from `target/bench-corpora/flat-tiny` and `target/bench-corpora/dependencies`. Then run `python3 run.py 10` for `--overwrite` and `MODE=fresh python3 run.py 10` for a new folder.
