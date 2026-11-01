# Permission restore

The native ZIP engines now restore Unix permission bits the way Info-ZIP `unzip` and bsdtar do, at no measured cost once directories that already have the default mode are skipped.

## Question

Can the native reader restore file and directory modes without slowing extraction?

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1, umask `022`. Release builds.
- Baseline is `3af1bea`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py` ran both binaries with `native` and `native-fast`, four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are 10,000 flat tiny files (one directory entry) and the dependency corpus (996 directory entries among 4,977), both zipped by Python `zipfile`.
- The load average was about 2 throughout.

Each file is created with its archive mode through `OpenOptions::mode`, so the operating system applies the umask with no extra call. Directory modes are applied after extraction, deepest first.

On five test archives from Info-ZIP, 7-Zip, bsdtar, `ditto` and Python, both engines matched `unzip` and bsdtar exactly under umask `022`. That covered an executable file (`755`), a read-only file (`444`) and a read-only directory (`555`). `tests/zip_corpus.rs` now compares modes on all 17 corpus archives. Removing the file modes or the directory modes each fails it.

## Results

| Workload | Reader | Wall ms, before | Wall ms, after |
| --- | --- | ---: | ---: |
| Dependency sources, first version | native | 166.64 [163.30–242.88] | 182.88 [180.52–222.39] |
| Dependency sources, first version | native-fast | 160.17 [159.15–185.35] | 178.38 [174.41–289.44] |
| Dependency sources, final | native | 165.47 [165.12–168.55] | 165.04 [162.47–210.09] |
| Dependency sources, final | native-fast | 160.13 [157.99–165.96] | 160.22 [157.90–227.98] |
| 10,000 flat files, first version | native | 355.13 [318.91–388.13] | 349.43 [338.08–373.86] |
| 10,000 flat files, first version | native-fast | 369.97 [342.33–443.55] | 372.29 [334.85–396.53] |

Values are medians with the observed range in brackets.

- **First version.** It called `chmod` on all 996 directories, one after another. That added about 16 to 18 ms on the dependency corpus. The ranges overlap only through outliers.
- **Final version.** A new directory already has the default mode, so the reader skips directories whose masked archive mode equals it. Wall-time ranges overlap and medians differ by under 0.5 ms.
- The flat-file rows have one directory, so they could not show this cost. Their ranges overlap.

An independent review then found three problems, now fixed with tests:

- After a failed final rename, nested read-only directories could stay locked, so the staging cleanup left them behind. The cleanup now unlocks each directory before reading its children, and runs on every failure after modes are applied.
- The first version probed the default mode with a directory named `.zipper-mode-probe`, so an archive entry with that name failed to extract. The staging directory's own mode is now the default.
- The corpus test assumed umask `022`. It now masks recorded modes with the current umask and passes under `077`.

The review also confirmed that Zipper matches bsdtar under umask `022` and `077`. Info-ZIP `unzip` ignores the umask, so it can restore wider modes.

## Decision

Kept in its final form.

## Limits

- The flat-file rows ran only with the first version. They contain one directory, so the change does not affect them.
- One machine, warm cache, umask `022`. A restrictive umask makes more directories differ from their archive mode, which brings back some `chmod` calls.
- Timestamps, symlinks and Windows attributes are not restored. The [roadmap](../../../docs/roadmap.md#m2-metadata-fidelity) tracks them.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py /tmp/perf/deps.zip /tmp/rust-dependencies \
  --reader before native zipper-before --reader after native zipper-after \
  --reader before-fast native-fast zipper-before --reader after-fast native-fast zipper-after \
  --runs 7 --threads 4 > dependencies.json
```

Build `zipper-before` at `3af1bea` and `zipper-after` at this commit.
