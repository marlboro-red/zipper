# Local-header agreement check

Requiring each local header to match its central-directory entry, and local records not to overlap, closed three hostile-archive gaps. Wall time and CPU time showed no measurable change, and peak memory rose by about 0.4 MiB at 10,000 entries.

## Question

Does checking every local header against the central directory, and rejecting overlapping records, cost measurable extraction time or memory?

Before this change, the native reader accepted three archives that `tests/hostile_zip.rs` now rejects:

- A local header name that differs from the central name.
- Local and central sizes that disagree.
- Two central entries that share one local record.

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds.
- Baseline is `ccd99ba`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py` ran both binaries with `native` and `native-fast`, four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are 10,000 flat tiny files from `scripts/make_tiny_corpus.py` and the 3,981-file dependency corpus, each zipped with Python `zipfile` at Deflate level 6.
- The load average was 1.9 at the start and 2.6 at the end.

An independent review attacked the change with crafted archives. Real archives from Info-ZIP (including piped output with data descriptors), 7-Zip, Python `zipfile` and macOS `ditto` extracted byte for byte with both readers.

## Results

| Workload | Reader | Wall ms, before | Wall ms, after | CPU ms, before | CPU ms, after | Peak RSS MiB, before | Peak RSS MiB, after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 flat files | native | 339.53 [324.08–347.69] | 320.28 [316.89–343.42] | 1136.9 | 1124.2 | 7.08 [6.98–7.11] | 7.48 [7.38–7.58] |
| 10,000 flat files | native-fast | 318.91 [314.36–389.68] | 334.47 [316.38–356.23] | 1131.8 | 1138.3 | 6.98 [6.94–7.02] | 7.34 [7.30–7.39] |
| Dependency sources | native | 166.89 [164.19–211.07] | 167.80 [164.56–169.30] | 515.2 | 514.8 | 6.00 [5.91–6.19] | 6.36 [6.22–6.58] |
| Dependency sources | native-fast | 158.01 [155.35–161.81] | 158.00 [155.31–188.87] | 497.3 | 495.8 | 7.52 [7.34–8.02] | 7.88 [7.61–8.17] |

Values are medians with the observed range in brackets.

- **Time.** Every before and after wall-time range overlaps. CPU medians differ by under 1.2%. No time difference is established.
- **Memory.** Peak RSS rose by 0.36 to 0.40 MiB, with separate ranges on both flat-file rows. The new fields take 32 bytes per entry, for two record offsets and an optional stored name, plus allocator overhead.
- **First attempt.** Storing every raw name cost 0.6 to 0.8 MiB. Keeping the raw bytes only when they differ from the decoded name halved that.

## Decision

Kept. The check closes three ways for one archive to mean different things to different readers, at no measured time cost and 32 bytes of new fields per entry.

## Limits

- One machine, warm cache, four threads.
- The review found hostile layouts this change does not close. A streaming reader can still see a record hidden between records, after a Deflate stream ends, or inside stored data with a fake data descriptor. The [roadmap](../../../docs/roadmap/m1-correctness-contract-for-zip.md) tracks them.
- `list` reads only the central directory, so it does not run the local-header check.
- Archives over 4 GiB and Java `jar` output were not tested.

## Reproduce

```sh
python3 scripts/make_tiny_corpus.py /tmp/perf/flat --files 10000 --layout flat
python3 scripts/compare_zip_readers.py /tmp/perf/flat.zip /tmp/perf/flat \
  --reader before native zipper-before --reader after native zipper-after \
  --reader before-fast native-fast zipper-before --reader after-fast native-fast zipper-after \
  --runs 7 --threads 4 > flat.json
```

Build `zipper-before` at `ccd99ba` and `zipper-after` at this commit. Create the ZIP with Python `zipfile` at Deflate level 6.
