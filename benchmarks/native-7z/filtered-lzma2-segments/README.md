# Parallel LZMA2 segments behind 7z filters

Kept. An ARM64-filtered 7z of 210 MB extracts 1.29 times faster at four threads, 0.746 s against 0.963 s, with ranges apart, and peak memory rises from 37 to 141 MiB. The BCJ, Delta and four-filter archives hold one LZMA2 block each, so they do not split and are unchanged.

## Question

Can a 7z folder with branch or Delta filters in front of LZMA2 decode its LZMA2 segments in parallel, as a plain LZMA2 folder already does? The reader split only folders whose methods were exactly LZMA2. 7-Zip adds BCJ or ARM64 by default for executables, so the hypothesis was a gain at four or more threads on such archives, with no change at one thread.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `base` is main at `8ba0aa0`, binary SHA-256 prefix `09444d3789d2dad5`. `segments` is `8ba0aa0` with the change main has as `12e7edc`, prefix `26f3a0f66ad0a7e7`. Both are release builds.
- The four solid archives of the [filters record](../own-filters/README.md), written by 7-Zip 26.05:
  - `arm64.7z`, ARM64 then LZMA2, 209,958,008 bytes of arm64 code, 35,676,314 packed;
  - `bcj.7z`, BCJ then LZMA2, 57,372,672 bytes of x86 code and data, 15,664,881 packed;
  - `chain4.7z`, `Delta:2`, BCJ and ARM64 then LZMA2, over the same x86 set, 22,229,382 packed;
  - `delta.7z`, `Delta:2` then LZMA2, 8,474,240 bytes, 4,251,435 packed.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range, and peak memory:

| Archive | Threads | `base` | `segments` | Peak MiB |
| --- | --- | --- | --- | --- |
| arm64 | 1 | 0.955 (0.932 to 0.974) | 0.960 (0.950 to 0.966) | 37 and 37 |
| arm64 | 4 | 0.963 (0.948 to 0.979) | 0.746 (0.724 to 0.751) | 37 and 141 |
| arm64 | default | 0.964 (0.952 to 0.976) | 0.740 (0.722 to 0.755) | 37 and 142 |
| bcj | 4 | 0.428 (0.408 to 0.434) | 0.434 (0.431 to 0.437) | 37 and 37 |
| chain4 | 4 | 0.602 (0.596 to 0.606) | 0.597 (0.596 to 0.608) | 37 and 37 |
| delta | 4 | 0.130 (0.129 to 0.139) | 0.134 (0.127 to 0.140) | 13 and 13 |

The one-thread and default-thread rows of bcj, chain4 and delta overlap in the same way. CPU time on arm64 rose from 0.96 s to 1.00 s at four threads. The raw JSON is beside this page, one file per row.

## Decision

Kept.

- **Change.** The new `lzma2_segments` in `src/native_7z.rs` decides whether a folder splits, and now accepts branch and Delta filters in front of LZMA2. The filter chain wraps the `ParallelLzma2` output on the folder's worker, and `ParallelLzma2` is unchanged. Encrypted folders, folders under 8 MiB and streams with one segment still do not split.
- **Memory.** `decoder_memory` reserves the larger of the sequential need and the segment budget plus the filter buffers. The arm64 peak of 141 MiB stays inside it, and plain solid Silesia already peaks at 143 MiB under the same policy.
- **Tests.** The reservation test and the split-selection test each failed before the change. New tests decode a filtered folder of three segments to 7-Zip's bytes at 1, 2 and 4 threads, and check that a damaged segment and a cancel publish nothing. An encrypted filtered folder still never splits. Three planted breaks were each caught, and the TLA+ model of the folder workers holds all six rules.
- **Review.** An adversarial review found nothing CONFIRMED.

## Limits

- One Mac. The gain depends on where 7-Zip reset the LZMA2 dictionary. The bcj, chain4 and delta archives have no reset, so they gain nothing.
- Memory rises by about 104 MiB on an archive that splits. A caller who needs less can pass `--threads 1`.
- The review found two older parity gaps, the same on main. An LZMA2 coder size that disagrees with the folder size is accepted, and so are bytes after the LZMA2 end marker inside the packed size. 7-Zip refuses both.

## Reproduce

Build `8ba0aa0`, and `8ba0aa0` after `git cherry-pick 12e7edc`, with `cargo build --release`. Then, for each archive and for `1`, `4` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-filters/arm64.7z \
  target/bench-work/7z-filters/src/arm64/arm64 --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader segments native <segments binary> \
  --reader-threads base 4 --reader-threads segments 4
```

The source of bcj and chain4 is `target/bench-work/7z-filters/src/x86/x86`, and of delta `target/bench-work/7z-filters/src/xray/xray`.
