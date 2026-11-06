# 7z extraction with Zipper's own LZMA decoder

**Held. With its own decoder in place of liblzma, the 7z reader extracts Silesia 5 to 8% slower, with ranges apart, and non-solid dependency sources 11 to 13% faster.** The change stays on its branch until the decoder beats liblzma on long streams. It also reads the `lc=8` archive natively and refuses five hostile archives that liblzma's path accepts.

## Question

M21 replaces liblzma with the decoder in `src/lzma.rs`. The 7z wiring was rebased onto main and reviewed, with no confirmed finding. Does 7z extraction stay as fast as on main? The metric is the median wall time of `zipper extract` with the default engine, at one thread and at default threads, with CPU time beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off, no agents running, Teams closed.
- `before` is main at `efecf3f`, binary SHA-256 prefix `b0a234d47e72`. `after` is the M21 wiring branch, prefix `89ee1042f4ff`. 7-Zip 26.05 is the reference.
- Four archives in `target/bench-work/7z-probe/`:
  - `silesia.7z`, solid, 49,597,394 bytes;
  - `silesia-nonsolid.7z`, 49,608,000 bytes;
  - `dependencies.7z`, solid, 6,156,722 bytes;
  - `dependencies-nonsolid.7z`, 12,169,886 bytes.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 7` ran each archive once at one thread and once at default threads, after one warmup round, in interleaved order. It checks every extracted byte against the source tree.
- The script refuses to start while another process uses 25% CPU or more. Three dependency runs were refused at first, because `fseventsd` was busy with the files the run before had written. They ran on the first retry with `--cooldown 5`. No timed run lists a busy process.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | 7-Zip |
| --- | --- | --- | --- | --- |
| Silesia, solid | 1 | 1.305 (1.298 to 1.308) | 1.398 (1.397 to 1.405) | 1.043 |
| Silesia, solid | default | 0.827 (0.827 to 0.829) | 0.889 (0.886 to 0.893) | 0.669 |
| Silesia, non-solid | 1 | 1.305 (1.301 to 1.310) | 1.401 (1.396 to 1.410) | 1.042 |
| Silesia, non-solid | default | 0.415 (0.410 to 0.419) | 0.435 (0.435 to 0.447) | 1.043 |
| Dependencies, solid | 1 | 0.446 (0.436 to 0.467) | 0.463 (0.454 to 0.499) | 0.453 |
| Dependencies, solid | default | 0.458 (0.456 to 0.476) | 0.474 (0.470 to 0.477) | 0.469 |
| Dependencies, non-solid | 1 | 0.858 (0.850 to 0.874) | 0.743 (0.740 to 0.783) | 0.616 |
| Dependencies, non-solid | default | 0.361 (0.353 to 0.377) | 0.322 (0.316 to 0.335) | 0.614 |

CPU time follows wall time at one thread. At default threads on solid Silesia, CPU time rose from 1.330 to 1.428 s. Peak memory fell or held on every archive. On non-solid dependency sources at default threads it fell from 34 to 18 MiB, and on solid Silesia at default threads it held at 347 MiB, against 256 MiB for 7-Zip.

## Decision

Held. The two Silesia archives decode long LZMA and LZMA2 streams, and there the decoder's speed decides. It still takes 1.09 times liblzma's time on the probe stream, as the [kernel-state record](../../lzma/kernel-state/README.md) shows, and the extraction lost about the same share. Non-solid dependency sources hold many small folders. There the new path wins, most likely because it sets up a decoder for each folder more cheaply than liblzma does. That cause is not profiled. Solid dependency sources show overlapping ranges, so no difference is established.

A swap that slows the largest archives does not ship. The branch keeps the wiring for when the decoder is faster. The five hostile archives it refuses run past their declared size or end unclean, and those checks can move to main's liblzma path on their own.

## Limits

One Mac and one session. Seven runs per cell. The dependency runs needed a cooldown, so their order differs from the Silesia runs. Linux and Windows were not measured. The cause of the gain on many small folders is not profiled.

## Reproduce

Build release binaries at main and at the wiring branch. Then, for each archive and for one thread and default threads, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/silesia.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER \
  --reader 7zip 7zip "$(command -v 7zz)"
```

Add `--reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1` for one thread, and `--cooldown 5` when `fseventsd` is busy.
