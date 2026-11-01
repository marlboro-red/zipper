# Portable extraction matrix

The first hosted runs exposed a Linux input-path bottleneck and a Windows whole-buffer regression on a large entry. They do not establish universal leadership or a threefold win on Windows or macOS.

## Question

How do Zipper's native and native-fast ZIP readers compare with direct 7-Zip and libarchive bsdtar on Linux, macOS, and Windows, and where are the bottlenecks?

## Setup

### The comparison tool

`scripts/extraction_matrix.py` compares Zipper's native and native-fast ZIP readers at explicit thread counts with direct 7-Zip and libarchive bsdtar. Every ZIP reader receives the identical archive. An uncompressed TAR contains the same source tree. The tool normalizes ZIP and TAR metadata and entry order. It retains archive hashes, binary hashes, commands, the corpus manifest, the run order, and all samples.

The input source can be a generated tiny corpus or a real regular-file tree. The tool rejects symlinks. The TAR baseline uses USTAR, so paths outside that format's limits fail instead of silently changing formats.

### Timing and verification

Each reader runs once for warmup, then for the requested measured rounds. A seeded shuffle interleaves readers. Wall time includes process startup and extraction. Source generation, archive creation, direct byte verification, and cleanup are outside timing. Verification checks the complete output root, directory and file membership, sizes, and every byte. Any mismatch aborts the report.

Zipper stages and renames the output, while external readers write directly. The tool creates their required empty output root outside timing, which slightly favors them. The bsdtar baseline uses 1 MiB records and disables supported metadata restoration options. 7-Zip uses its default extraction parallelism. No reader is asked to fsync, so the matrix measures completed extraction, not crash-durable restoration.

### Memory

Memory is the high-water mark of the individual process, not a sum over the process tree. The tool reads it from these sources:

- macOS uses `wait4.ru_maxrss`.
- Linux uses the GNU time child peak RSS.
- Windows uses `GetProcessMemoryInfo.PeakWorkingSetSize`.

The Windows query uses the retained child handle after waiting, so it does not miss short peaks the way sampled RSS can. A failed memory query aborts the report rather than reporting zero. The Linux GNU time wrapper avoids an inherited Python-parent high-water mark that direct wait4 reported in the first hosted run. Wall timing includes the wrapper overhead. These OS metrics are not interchangeable measures of committed or private memory. See the [Microsoft API](https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getprocessmemoryinfo) and the [counter definition](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters).

### Hosted workflow and runs

The manually dispatched `Portable extraction matrix` workflow runs Linux, macOS, and Windows. It measures flat and grouped tiny files and one large ZIP entry, plus the existing RAM-materialization diagnostic. It records the filesystem and the Windows antivirus status without changing protection settings. The configurable file count stops at 90,000 to leave room for directories under the native ZIP reader's 100,000-entry limit. Run identifiers and source revisions must accompany results.

This record covers two runs:

- The [10,000-file, seven-round run](https://github.com/marlboro-red/zipper/actions/runs/40889835980) built source `4226bc4`. Linux and macOS completed. Windows was cancelled after the measurement step ran about 20 minutes without completing its first report. No Windows performance numbers are inferred from that incomplete run.
- The [1,000-file, three-round run](https://github.com/marlboro-red/zipper/actions/runs/40892983670) built source `cc8aced`. All three platforms completed. This source includes the Linux native-fast input cache and the initial shared 128 MiB budget. It predates the reduced read-ahead buffers and the conservative budget default outside ARM64 macOS.

All samples, archive and binary hashes, commands, and environment details are kept in this directory. To avoid repeating large file inventories, this directory represents each corpus manifest by a canonical JSON SHA-256. The full manifests are in the workflow artifacts. Fixtures are deterministic, and the workflow contains their construction.

## Results

### Initial 10,000-file baselines (milliseconds)

| Host / shape | Native 1 worker | Native 4 workers | Native-fast 4 | bsdtar ZIP | bsdtar TAR |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline-linux / flat | 428.64 | 575.78 | 579.34 | 236.50 | 205.76 |
| baseline-linux / grouped | 405.19 | 596.09 | 590.07 | 218.56 | 186.86 |
| baseline-macos / flat | 977.47 | 1072.45 | 1068.29 | 1127.12 | 1058.46 |
| baseline-macos / grouped | 1182.47 | 1031.70 | 870.78 | 1073.72 | 1100.77 |

Linux memory from this first run is invalid for comparisons. Direct Python wait4 reported an inherited parent high-water mark. GNU time collection and a large-parent control test fixed the problem in subsequent runs. Timing and byte checks are unaffected.

These baselines exposed a Linux input-path bottleneck that local M5 timings had hidden.

### Windows preliminary results (milliseconds)

The host is Windows Server 2022 with four logical CPUs. Each tiny-file fixture has 1,000 files, and each reader ran three rounds after warmup. The single-entry fixture contains 61 MiB. The JSON files hold per-run ranges and peak working sets. These measurements are of source builds, not the older downloadable alpha release.

| Shape | Native 1 | Native 4 | Native-fast 4 | 7-Zip | bsdtar ZIP | bsdtar TAR |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| flat | 347.33 | 465.35 | 469.67 | 509.49 | 409.76 | 362.88 |
| grouped | 468.72 | 268.63 | 283.92 | 718.98 | 484.61 | 628.01 |
| single | 122.91 | 122.86 | 715.22 | 171.03 | 149.95 | 38.33 |

Flat files favor one worker, while grouped files can benefit from parallelism. The large-entry result exposes a whole-buffer regression. In the reported build, the Windows peak working set was about 4.93 MiB for native4 versus 97.63 MiB for native-fast4. Linux likewise showed the whole-buffer configuration slower on the large-entry diagnostic.

## Decision

- These noisy three-run measurements do not justify a universal Windows thread cap.
- Production now defaults to a shared 64 MiB entry-buffer budget outside ARM64 macOS, so the large entry streams again. The 128 MiB default is kept only on the locally measured ARM64 macOS target.
- These findings support choosing resources and scheduling per workload.

Matched experiments measure the Linux improvements separately. See the [Linux archive cache](../native-zip/linux-cache/README.md) and the [smaller file-reader buffers](../native-zip/input-buffer/README.md).

This matrix is the first measurement gate in the [extraction design](../../docs/design.md#implementation-proceeds-through-five-ordered-gates). It does not implement the proposed new executor or claim an extraction speedup. The executor redesign remains incremental. This measurement stage introduced no output backend and no new format.

## Limits

- These are hosted-runner measurements, not a controlled cross-platform ranking. Hosted runners help with portability and finding bottlenecks, not with claims of hardware leadership.
- Do not compare absolute times between separate hosted jobs as if hardware and load were fixed.
- Three samples are preliminary diagnostics, especially where ranges are wide.
- The matrix compares bytes and directories, not metadata fidelity. Metadata policies differ, and bsdtar uses tuned buffers and reduced metadata flags. 7-Zip restores timestamps and attributes that Zipper does not, and no 7-Zip flag disables them.
- Zipper ran at explicit worker counts of 1, 2, 4, and 8, and the tables show 1 and 4. bsdtar and 7-Zip each ran as one process, with 7-Zip using its default extraction parallelism. Compare those readers with the one-worker column for equal worker counts. The reports record wall time only, not CPU time. The [matched-thread re-run](../rust-dependencies/README.md) repeats the comparison at equal worker counts.
- Cache state is uncontrolled and warmed by archive creation and earlier runs.
- Earlier Linux reports must not be used for memory comparisons.

## Reproduce

```sh
python3 scripts/make_tiny_corpus.py /tmp/flat --files 10000
python3 scripts/extraction_matrix.py /tmp/flat \
  --zipper target/release/zipper --output results/flat.json \
  --runs 7 --threads 1 2 4 8 --sevenzip 7zz --tar /usr/bin/tar
```

To generate a separate tiny corpus with another shape, use `--layout grouped` or `--layout deep`. To select the output filesystem, use `--work-dir`. To run on hosted runners, dispatch the `Portable extraction matrix` workflow.
