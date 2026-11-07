# 7z extraction switched to Zipper's own LZMA decoder

**Kept. With Zipper's own decoder in place of liblzma, the 7z reader extracts Silesia 7 to 9% faster, with ranges apart on five of six Silesia cells, and non-solid dependency sources 9 to 20% faster. 7-Zip still extracts one long LZMA stream about 8% faster at one thread.** One Mac, one session, in which every reader ran about 15% slower than in the [held record](../own-lzma-decoder/README.md).

## Question

The [held record](../own-lzma-decoder/README.md) kept the swap on its branch because Silesia extracted 5 to 8% slower, while the decoder took 1.09 times liblzma's time. The [tree-preload record](../../lzma/tree-preload/README.md) then brought the decoder to 0.878 times liblzma's time on the probe stream. The swap was rebased onto main, gated and reviewed again. Does 7z extraction now beat main? The metric is the median wall time of `zipper extract` with the default engine, with CPU time and peak memory beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off, no agents running.
- `fseventsd` had stayed near 96% CPU for an hour with nothing writing files, so the script refused every run. It ran normally after a restart with `sudo killall fseventsd`.
- `before` is main at `53ccf74`, binary SHA-256 prefix `075a223f4555218f`. `after` is the swap at `84183c9`, prefix `0336f3eb21fd98f6`. 7-Zip 26.05 for arm64 is the reference.
- Five archives in `target/bench-work/7z-probe/`:
  - `silesia.7z`, solid LZMA2, 49,597,394 bytes;
  - `silesia-lzma1.7z`, solid LZMA, 49,563,337 bytes;
  - `silesia-nonsolid.7z`, 49,608,000 bytes;
  - `dependencies.7z`, solid, 6,156,722 bytes;
  - `dependencies-nonsolid.7z`, 12,169,886 bytes.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 7 --cooldown 5` ran each archive at one thread and at four threads, the script's default, after one warmup round, in interleaved order. It checks every extracted byte against the source tree.
- No timed run lists a busy process.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | 7-Zip | `after` / `before` |
| --- | --- | --- | --- | --- | ---: |
| Silesia, solid LZMA2 | 1 | 1.482 (1.451 to 1.513) | 1.347 (1.320 to 1.365) | 1.242 | 0.909 |
| Silesia, solid LZMA2 | 4 | 1.062 (1.046 to 1.115) | 0.987 (0.952 to 1.024) | 0.928 | 0.930 |
| Silesia, solid LZMA | 1 | 1.482 (1.440 to 1.528) | 1.344 (1.297 to 1.359) | 1.239 | 0.906 |
| Silesia, solid LZMA | 4 | 1.461 (1.453 to 1.517) | 1.341 (1.310 to 1.372) | 1.235 | 0.918 |
| Silesia, non-solid | 1 | 1.449 (1.443 to 1.481) | 1.348 (1.319 to 1.379) | 1.203 | 0.930 |
| Silesia, non-solid | 4 | 0.523 (0.511 to 0.550) | 0.486 (0.453 to 0.512) | 1.258 | 0.929 |
| Dependencies, solid | 1 | 0.553 (0.530 to 0.572) | 0.537 (0.473 to 0.560) | 0.555 | 0.972 |
| Dependencies, solid | 4 | 0.554 (0.524 to 0.564) | 0.545 (0.518 to 0.575) | 0.543 | 0.985 |
| Dependencies, non-solid | 1 | 0.947 (0.930 to 0.981) | 0.754 (0.702 to 0.769) | 0.691 | 0.796 |
| Dependencies, non-solid | 4 | 0.416 (0.410 to 0.440) | 0.378 (0.371 to 0.392) | 0.692 | 0.908 |

CPU time fell with wall time, from 1.39 to 1.25 s on solid Silesia at one thread and from 1.19 to 1.00 s on non-solid dependency sources at four threads. Peak memory fell or held on every archive. On non-solid dependency sources at four threads it fell from 34 to 19 MiB. On solid Silesia at four threads it held at 348 MiB, against 256 MiB for 7-Zip.

## Decision

Kept. Every Silesia cell is faster, with ranges apart in five of six. In non-solid Silesia at four threads the ranges touch, at 0.511 and 0.512 s. Non-solid dependency sources are faster with ranges apart. Solid dependency sources overlap, so no difference is established there.

The swap also extracts the `lc=8` archive that liblzma refuses, and refuses the hostile LZMA ends through Zipper's own decoder. An adversarial review of the rebased swap found no confirmed failure.

## Limits

- One Mac and one session, with seven runs per cell. Linux and Windows were not measured.
- Every reader, 7-Zip included, ran about 15% slower than in the held record's session. Main took 1.482 s on solid Silesia at one thread here, against 1.305 s then. The readers are interleaved in each round, so the ratios hold, but absolute times do not compare across the two records.
- Four threads is the script's default, not Zipper's default thread count.
- 7-Zip is still faster on every long LZMA stream at one thread, by 8 to 12%.

## Reproduce

Build release binaries at `53ccf74` and `84183c9`. Then, for each archive, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/silesia.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 5 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER \
  --reader 7zip 7zip "$(command -v 7zz)"
```

Add `--reader-threads before 1 --reader-threads after 1 --reader-threads 7zip 1` for one thread.
