# Record layout check

Requiring ZIP records to tile the archive, with checked data descriptors and at most one ZIP64 field per record, closed five more hostile-archive gaps. Wall time, CPU time and peak memory showed no measurable change, even with 8,000 data descriptors to read.

## Question

Does requiring each record to end exactly at the next one, and checking every data descriptor, cost measurable extraction time or memory?

Before this change, the native reader accepted these archives, which `tests/hostile_zip.rs` now rejects:

- A complete local record hidden before the first entry, between entries, or before the central directory.
- A data descriptor whose CRC or size disagrees with the central directory, or with bytes after it.
- A local encryption flag that the central directory does not set.
- A local ZIP64 field whose sizes disagree with real 32-bit sizes.
- A record with two ZIP64 fields. The `zip` crate keeps the last one and other readers keep the first, so an independent review built an archive that hides a second file from Zipper but not from a streaming reader.

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds.
- Baseline is `05e57c1`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py` ran both binaries with `native` and `native-fast`, four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are 10,000 flat tiny files zipped by `ditto` (8,000 entries use data descriptors), the same files zipped by Python `zipfile`, and the 3,981-file dependency corpus zipped by Python `zipfile`.
- The load average was 1.8 at the start and 3.2 at the end.

Real archives still extract byte for byte with both readers. The writers were Info-ZIP (default, stored, ZIP64, piped), 7-Zip (Deflate, Copy, levels, timestamps), Python `zipfile` (seekable and unseekable, stored and Deflate, forced ZIP64), `ditto` and bsdtar.

## Results

| Workload | Reader | Wall ms, before | Wall ms, after | CPU ms, before | CPU ms, after | Peak RSS MiB, before | Peak RSS MiB, after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 files, `ditto` | native | 327.00 [321.80–329.19] | 321.19 [310.85–327.97] | 1128.4 | 1119.7 | 9.22 [9.12–9.31] | 9.17 [9.08–9.27] |
| 10,000 files, `ditto` | native-fast | 327.55 [325.49–332.20] | 324.58 [309.97–335.65] | 1135.3 | 1128.1 | 9.05 [9.03–9.09] | 9.08 [9.03–9.11] |
| 10,000 files, Python | native | 326.87 [316.09–333.61] | 328.17 [310.85–338.89] | 1132.5 | 1127.4 | 7.44 [7.39–7.48] | 7.47 [7.39–7.53] |
| 10,000 files, Python | native-fast | 324.79 [313.40–336.37] | 333.30 [323.80–336.91] | 1135.3 | 1131.6 | 7.33 [7.28–7.36] | 7.34 [7.33–7.39] |
| Dependency sources | native | 168.62 [166.08–199.42] | 167.22 [164.73–186.89] | 520.9 | 517.5 | 6.48 [6.20–6.64] | 6.33 [6.08–6.52] |
| Dependency sources | native-fast | 161.46 [156.82–187.79] | 157.58 [156.91–159.49] | 501.8 | 494.6 | 7.67 [7.48–7.86] | 7.78 [7.56–8.25] |

Values are medians with the observed range in brackets. Every before and after range overlaps for wall time and peak memory, and CPU medians differ by under 1.5%.

## Decision

Kept. The check closes five ways to hide data from Zipper at no measured cost. It rejects self-extracting archives from `zip -A`, because their leading executable bytes are the kind of unreferenced data this check refuses. The compatibility backend still reads them.

## Limits

- One machine, warm cache, four threads.
- A streaming reader can still see different content from Zipper in three ways:
  - bytes after a Deflate stream ends inside its declared size
  - a fake data descriptor inside stored data
  - a Unicode path field that renames an entry

  The [roadmap](../../../docs/roadmap.md#m1-correctness-contract-for-zip) tracks them.
- `list` reads only the central directory, so it runs the first-record and ZIP64-field checks but not the per-record layout check.
- Java `jar` output and archives over 4 GiB were not tested.

## Reproduce

```sh
python3 scripts/make_tiny_corpus.py /tmp/perf/flat --files 10000 --layout flat
ditto -c -k --norsrc --keepParent /tmp/perf/flat /tmp/perf/flat-ditto.zip
python3 scripts/compare_zip_readers.py /tmp/perf/flat-ditto.zip /tmp/perf/flat \
  --reader before native zipper-before --reader after native zipper-after \
  --reader before-fast native-fast zipper-before --reader after-fast native-fast zipper-after \
  --runs 7 --threads 4 > flat-ditto.json
```

Build `zipper-before` at `05e57c1` and `zipper-after` at this commit.
