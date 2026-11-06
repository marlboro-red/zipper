# 7z LZMA and LZMA2 streams ended where 7-Zip ends them

**Kept. Ending each LZMA folder at its declared size and refusing an LZMA2 stream cut inside a chunk costs no measurable time on any of five archives, at one thread or at default threads.** Every pair of ranges overlaps, so the record shows no cost, not a proven zero.

## Question

The 7z reader on main decodes LZMA and LZMA2 through liblzma. Before this change, it accepted five hostile archives that 7-Zip reports as a data error. These are streams that run past their declared size, end unclean at it, or stop inside an LZMA2 chunk. The change makes liblzma's .lzma decoder stop at the folder's size and appends one end marker to an LZMA2 stream that runs out of input. Does it slow extraction? The metric is the median wall time of `zipper extract` with the default engine, with CPU time beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off, no agents running.
- `before` is main at `170b319`, binary SHA-256 prefix `04e33ebf9108`. `after` is the change at `e520622`, prefix `3da46fc1ea76`. 7-Zip 26.05 is the reference.
- Five archives in `target/bench-work/7z-probe/`:
  - `silesia.7z`, solid LZMA2, 49,597,394 bytes;
  - `silesia-nonsolid.7z`, LZMA2, 49,608,000 bytes;
  - `silesia-lzma1.7z`, solid LZMA1 from `7zz a -m0=lzma`, 49,563,337 bytes;
  - `dependencies.7z`, solid, 6,156,722 bytes;
  - `dependencies-nonsolid.7z`, 12,169,886 bytes.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 7 --cooldown 5` ran each archive at one thread and at default threads, after one warmup round, in interleaved order. It checks every extracted byte against the source tree. No run listed a busy process, and none had an error.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | 7-Zip |
| --- | --- | --- | --- | --- |
| Silesia, solid | 1 | 1.301 (1.290 to 1.311) | 1.300 (1.286 to 1.318) | 1.046 |
| Silesia, solid | default | 0.843 (0.828 to 0.859) | 0.835 (0.831 to 0.859) | 0.686 |
| Silesia, non-solid | 1 | 1.300 (1.287 to 1.307) | 1.298 (1.284 to 1.306) | 1.048 |
| Silesia, non-solid | default | 0.430 (0.427 to 0.442) | 0.434 (0.422 to 0.439) | 1.047 |
| Silesia, LZMA1 | 1 | 1.299 (1.277 to 1.316) | 1.296 (1.288 to 1.314) | 1.044 |
| Silesia, LZMA1 | default | 1.300 (1.290 to 1.309) | 1.299 (1.289 to 1.320) | 1.046 |
| Dependencies, solid | 1 | 0.478 (0.456 to 0.494) | 0.486 (0.459 to 0.506) | 0.473 |
| Dependencies, solid | default | 0.460 (0.445 to 0.472) | 0.460 (0.454 to 0.479) | 0.477 |
| Dependencies, non-solid | 1 | 0.854 (0.841 to 0.869) | 0.857 (0.848 to 0.873) | 0.625 |
| Dependencies, non-solid | default | 0.366 (0.350 to 0.375) | 0.364 (0.352 to 0.370) | 0.620 |

CPU time follows wall time in every cell. Peak memory held within 3 MiB on every archive.

## Decision

Kept. No cell shows a difference beyond the spread, and the change refuses five archives that 7-Zip also refuses. The solid LZMA1 archive is one folder, so it runs on one thread at default threads too.

## Limits

One Mac and one session. Seven runs per cell. Linux and Windows were not measured. The archives come from 7-Zip only, so other writers' LZMA ends are covered by the 7z corpus tests, not by this timing.

## Reproduce

Build release binaries at `170b319` and `e520622`. Then, for each archive and for one thread and default threads, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/silesia-lzma1.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 5 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER \
  --reader 7zip 7zip "$(command -v 7zz)"
```

Add `--reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1` for one thread. Build `silesia-lzma1.7z` with `7zz a -m0=lzma` from `target/bench-corpora/silesia`.
