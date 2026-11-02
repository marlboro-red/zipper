# Tar rows on Linux and Windows runners

**Plain tar through the default engine beats default bsdtar on every workload on Linux, by 1.45× to 2.83×, and on five of seven on Windows, by 1.13× to 1.63×.** Windows is level on the two large entries, at 0.97× and 0.98×. The gzip rows lose on the large entries on both runners. Linux loses on the compressible entry (0.85×), and Windows on both entries (0.68× and 0.63×). On those platforms the decoder is zlib-rs, which this record suspects but does not prove is the gap.

## Question

M7 added tar rows to the scoreboard. How does the in-process tar reader with four writers compare with default bsdtar on Linux and Windows, where the gzip decoder is zlib-rs rather than Apple's zlib?

## Setup

- GitHub Actions run 41440755256, workflow `.github/workflows/scoreboard.yml`, at `05233ac`. This revision has the writer pool. The direct-chunk change came after it.
- `ubuntu-24.04` reports `Linux-7.0.0-1017-azure-x86_64-with-glibc2.39`. `windows-2022` reports `Windows-2022Server-10.0.20348-SP0`. Each runner has 4 logical CPUs.
- `cargo xtask bench` ran the seven fixed workloads, seven measured rounds after one warmup, every extraction byte-verified. bsdtar on Linux comes from `libarchive-tools`; on Windows it is `C:/Windows/System32/tar.exe`.
- The tar rows read a ustar archive and its gzip level 6 form, written by `write_tar` and `write_gzip` in `scripts/extraction_matrix.py`. Both bsdtar rows restore default metadata.

## Results

Medians in milliseconds with the ratio of bsdtar's time to Zipper's on the same file. Raw JSON for all rows is in `ubuntu-24.04/` and `windows-2022/`.

| Workload | Linux tar | Linux bsdtar | Linux tar.gz | Linux bsdtar | Windows tar | Windows bsdtar | Windows tar.gz | Windows bsdtar |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Dependency sources | 133 (2.83×) | 377 | 285 (1.79×) | 511 | 421 (1.63×) | 687 | 466 (1.85×) | 861 |
| 10,000 flat tiny files | 312 (1.45×) | 452 | 314 (1.46×) | 457 | 691 (1.20×) | 829 | 684 (1.23×) | 842 |
| 4,000 grouped files | 91 (2.01×) | 183 | 90 (1.99×) | 178 | 285 (1.39×) | 397 | 281 (1.45×) | 408 |
| 256 MiB compressible | 72 (2.46×) | 178 | 236 (0.85×) | 200 | 93 (0.97×) | 91 | 237 (0.68×) | 161 |
| 256 MiB random | 71 (2.47×) | 175 | 128 (1.72×) | 220 | 92 (0.98×) | 90 | 173 (0.63×) | 109 |
| Mixed tree | 102 (2.12×) | 216 | 128 (1.87×) | 240 | 293 (1.42×) | 416 | 315 (1.42×) | 448 |
| Silesia | 62 (2.39×) | 147 | 487 (1.38×) | 673 | 82 (1.13×) | 93 | 438 (1.34×) | 589 |

## Decision

No code change in this record. It sets one experiment. A faster streaming inflate on Linux and Windows, such as zlib-ng, should be compared with zlib-rs inside one run. On macOS, Apple's zlib already put the gzip rows level with or ahead of bsdtar in the [macOS scoreboard](../../../docs/results.md).

## Limits

One run of each hosted runner, on shared virtual machines with Windows Defender enabled. Hosted Windows runs have differed by nearly 2× between runs, so only the ratios within this run carry weight. The record does not profile the decoder. The direct-chunk change, which removed one copy of each decoded byte, came after this revision.

## Reproduce

```sh
gh workflow run scoreboard.yml -f runs=7
gh run download <run id>
```
