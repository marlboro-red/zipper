# Portable extraction matrix: initial evidence

These are hosted-runner measurements, not a controlled cross-platform ranking.
Every reader extracts identical ZIP bytes within a workload; TAR holds the same
files. Bytes, sizes and full tree membership are checked after each extraction.
Metadata policies differ; bsdtar uses tuned buffers and reduced metadata flags.
Zipper stages output while external readers write directly. Cache is uncontrolled.

- [10,000-file, seven-round run](https://github.com/marlboro-red/zipper/actions/runs/40889835980),
  source `4226bc4`: Linux and macOS completed. Windows was cancelled after the
  measurement step ran about 20 minutes without completing its first report.
  No Windows performance numbers are inferred from that incomplete run.
- [1,000-file, three-round run](https://github.com/marlboro-red/zipper/actions/runs/40892983670),
  source `cc8aced`: all three platforms completed. This source includes the Linux
  native-fast input cache and the initial shared 128 MiB budget. It predates the
  reduced read-ahead buffers and conservative non-ARM64-macOS budget default.
  Three samples are preliminary diagnostics, especially where ranges are wide.

All samples, archive/binary hashes, commands and environment are retained here.
To avoid repeating large file inventories, corpus manifests are represented by
a canonical JSON SHA-256; full manifests are in the workflow artifacts. Fixtures
are deterministic and their construction is in the workflow. Do not compare
absolute times between separate hosted jobs as if hardware and load were fixed.

## Initial 10,000-file baselines (milliseconds)

Linux memory from the first run is invalid for comparisons: direct Python wait4
reported an inherited parent high-water mark. GNU time collection and a large-parent
control test fixed this in subsequent runs. Timing and byte checks are unaffected.

| Host / shape | Native 1 worker | Native 4 workers | Native-fast 4 | bsdtar ZIP | bsdtar TAR |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline-linux / flat | 428.64 | 575.78 | 579.34 | 236.50 | 205.76 |
| baseline-linux / grouped | 405.19 | 596.09 | 590.07 | 218.56 | 186.86 |
| baseline-macos / flat | 977.47 | 1072.45 | 1068.29 | 1127.12 | 1058.46 |
| baseline-macos / grouped | 1182.47 | 1031.70 | 870.78 | 1073.72 | 1100.77 |

These baselines exposed a Linux input-path bottleneck that local M5 timings had
obscured. Follow-up matched experiments demonstrate the improvements separately:
[Linux archive cache](../native-zip/linux-cache/README.md) and
[smaller file-reader buffers](../native-zip/input-buffer/README.md).

## Windows preliminary results (milliseconds)

Windows Server 2022, four logical CPUs, 1,000 files per tiny-file fixture,
three rounds after warmup. Per-run ranges and peak working sets are in JSON.
Antivirus/filesystem status was recorded without changing protection settings.
The single-entry fixture contains 61 MiB. These measurements are of source builds,
not the older downloadable alpha release.

| Shape | Native 1 | Native 4 | Native-fast 4 | 7-Zip | bsdtar ZIP | bsdtar TAR |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| flat | 347.33 | 465.35 | 469.67 | 509.49 | 409.76 | 362.88 |
| grouped | 468.72 | 268.63 | 283.92 | 718.98 | 484.61 | 628.01 |
| single | 122.91 | 122.86 | 715.22 | 171.03 | 149.95 | 38.33 |

Flat files favor one worker, while grouped files can benefit from parallelism;
these noisy three-run measurements do not justify a universal Windows thread cap.
The large-entry result exposes a whole-buffer regression. Accordingly, production
now defaults to a shared 64 MiB entry-buffer budget outside ARM64 macOS, making
this entry stream again. The Windows peak working set was about 4.93 MiB for
native4 versus 97.63 MiB for native-fast4 in the reported build. Linux likewise
showed this whole-buffer configuration slower on the large-entry diagnostic.
The 128 MiB default is retained only on the locally measured ARM64 macOS target.

These findings support workload-specific resource/scheduling choices. They do
not establish universal leadership or a threefold win on Windows/macOS. The
proposed executor redesign remains incremental; no output backend or new format
has been introduced by this measurement stage.
