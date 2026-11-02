# Run-time CPU detection in zlib-rs

**Kept. Letting zlib-rs detect CPU features at run time makes Silesia and the random entry extract 1.38× to 1.84× faster as `.tar.gz` on Linux and Windows runners, and Silesia ZIP 1.23× to 1.26× faster.** Linux dependency sources gain 1.33× and the compressible `.tar.gz` 1.06× to 1.08×. One synthetic row got slower. The 256 MiB ZIP entry that repeats one 25-byte line extracts at 0.73× on Linux and 0.75× on Windows, though it still beats default bsdtar on both. macOS uses Apple's zlib and is unchanged.

## Question

The [Linux and Windows tar rows](../tar-scoreboard/README.md) lost to default bsdtar on large `.tar.gz` entries, by up to 1.59×. Zipper builds flate2 without default features, which drops `runtime_detection`, so zlib-rs lacked `std`. Without `std`, zlib-rs uses its baseline x86 code, with no AVX2, SSE4.2 or carry-less multiply. Does enabling the feature close the gap? The metric is the wall median of the same archive, before and after, within one runner job.

## Setup

- GitHub Actions run 41451135093 of `.github/workflows/compare.yml`. `before` is `56d6f74`. `after` is `0d2d1a5`, which adds `runtime_detection` to flate2's features.
- `ubuntu-24.04` reports `Linux-7.0.0-1017-azure-x86_64-with-glibc2.39`. `windows-2022` reports `Windows-2022Server-10.0.20348-SP0`. Each runner has 4 logical CPUs.
- `scripts/compare_workloads.py` wrote each scoreboard fixture once. The ZIP is Deflate level 6. The `.tar.gz` is the ustar archive in gzip level 6. It ran `scripts/compare_zip_readers.py` with both binaries at default settings, for 11 shuffled rounds after one warmup. Every round passed the byte and tree check.

## Results

Wall medians in milliseconds with the range and the ratio of `before` to `after`. Raw samples are in `ubuntu-24.04/` and `windows-2022/`.

| Archive | Linux before | Linux after | Ratio | Windows before | Windows after | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Silesia `.tar.gz` | 466.7 [463.2–479.3] | 339.4 [329.9–345.3] | 1.38× | 394.3 [392.7–562.8] | 283.7 [282.4–429.6] | 1.39× |
| 256 MiB random `.tar.gz` | 117.9 [117.4–122.0] | 70.4 [68.7–78.8] | 1.68× | 121.8 [121.3–123.8] | 66.2 [65.6–77.1] | 1.84× |
| 256 MiB compressible `.tar.gz` | 225.5 [225.3–226.6] | 208.3 [207.8–216.9] | 1.08× | 181.0 [180.2–182.1] | 170.7 [164.8–174.7] | 1.06× |
| Dependency sources `.tar.gz` | 286.8 [284.8–294.1] | 215.2 [206.3–217.3] | 1.33× | 3,329.8 [2,846.2–5,085.6] | 3,310.9 [2,848.3–5,577.2] | 1.01× |
| Silesia ZIP | 189.4 [173.8–211.0] | 153.9 [144.9–175.7] | 1.23× | 162.5 [158.3–356.7] | 129.4 [124.4–196.2] | 1.26× |
| 256 MiB random ZIP | 70.9 [70.4–77.6] | 72.3 [68.8–79.3] | 0.98× | 69.7 [68.3–90.0] | 69.9 [68.8–72.7] | 1.00× |
| 256 MiB compressible ZIP | 154.4 [151.6–165.8] | 210.5 [209.0–237.5] | 0.73× | 122.3 [120.7–126.0] | 163.5 [160.1–168.1] | 0.75× |
| Dependency sources ZIP | 162.3 [157.1–179.8] | 158.9 [153.2–163.8] | 1.02× | 1,986.7 [1,597.5–4,112.4] | 2,459.9 [1,413.0–5,226.5] | 0.81× |

The Windows dependency rows ran six to seven times slower than in the scoreboard, with ranges up to 3.7× wide on both sides, so they show nothing.

## Decision

Kept. On Linux, the after range sits below the before range for every `.tar.gz` row, and the same holds for the random `.tar.gz` on Windows. The Windows Silesia rows and the Linux Silesia ZIP overlap only through their slowest after sample. In each, ten of eleven after samples fall below every before sample. These are the rows the change targets.

The compressible entry repeats `record=example; value=42` and a newline, so every Deflate match copies from 25 bytes back. In the ZIP path that row got 1.33× to 1.36× slower. A likely cause is that zlib-rs copies such short-distance matches more slowly in its AVX2 code than in its baseline code. This record does not profile it. The same entry in `.tar.gz` got faster, likely because gzip's checksum now uses carry-less multiply. On the tar scoreboard run, default bsdtar took 232 ms on Linux and 279 ms on Windows for this ZIP, so Zipper still wins there. Realistic files gain, and this one row loses.

## Limits

One run per runner, on shared virtual machines. The compressible entry is synthetic, and real data with matches this short and this regular is rare. Neither the slow copy path nor the checksum gain was profiled. This run did not include bsdtar, so the comparisons with bsdtar come from a different run. macOS was not measured, because its build uses Apple's zlib and does not change.

## Reproduce

```sh
gh workflow run compare.yml --ref <candidate branch> -f base=56d6f74 \
  -f workloads="large-compressible large-random silesia dependencies" -f formats="zip tar.gz" -f runs=11
gh run download <run id>
```
