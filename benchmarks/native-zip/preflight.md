# Buffered ZIP preflight

A 64 KiB buffered reader for central-directory validation cut listing latency about 18% and is kept. Extraction ranges overlap, and three related experiments in this record found no reliable gain.

## Question

Central-directory validation performs one small file read and seek per entry. Can a 64 KiB BufReader with relative skips keep validation bytes between records and reduce overhead without relaxing validation?

The record also covers three related questions. Does preserving the shared ZIP reader's buffer on seeks help? Does libdeflate CRC help v2? Does a larger streaming output buffer help?

## Setup

Apple M5, macOS, release binary. Four workers, one warmup and seven interleaved runs.

The baseline production source is aba6091, which has the same reader as Windows alpha 55bc598. The candidate changes only preflight buffering. The linked JSON reports retain binary hashes and all samples.

Every sample comes with a complete extraction and a direct byte, size, and tree comparison. For listing, that extraction is outside the timed interval. The complete before and after listings also match byte for byte (4,041 entries including directories).

## Results

### Preflight buffering

| Workload / operation | Before ms: median [min, max] | After ms: median [min, max] | Peak RSS MiB before / after |
|---|---:|---:|---:|
| 4,000 files: listing | 10.28 [10.11, 12.62] | 8.42 [8.24, 8.54] | 5.4 / 5.4 |
| 4,000 files: extraction | 127.06 [125.82, 130.79] | 124.67 [122.12, 134.07] | 7.7 / 7.3 |
| Silesia: extraction | 117.29 [116.52, 118.81] | 117.93 [116.36, 118.63] | 5.0 / 4.7 |

Listing latency falls about 18%, with disjoint observed ranges. Extraction ranges overlap.

Raw reports are [listing](preflight-list-small-files.json), [small-file extraction](preflight-extract-small-files.json), and [Silesia extraction](preflight-extract-silesia.json).

### Rejected buffer preservation on seeks

Preserving the shared ZIP reader's buffer on seeks gave overlapping small-file and Silesia extraction ranges. Independent review and a 10,000-operation cursor comparison found no correctness blocker. Raw data is in [small files](buffered-seek-small-files.json) and [Silesia](buffered-seek-silesia.json).

### Rejected libdeflate CRC for v2

Replacing v2's CRC32 implementation with the already-linked libdeflate CRC did not establish a reliable improvement. The four-corpus creation and extraction measurements were noisy. Independent equivalence checks passed. The raw results were removed with the native format.

### Streaming output buffer sweep

A separate sweep tested streaming output buffers of 64 KiB, 256 KiB (current), 1 MiB, and 4 MiB on Silesia. Median extraction times were 118.34, 118.19, 117.08, and 119.20 ms respectively, with overlapping ranges. Peak RSS rose from 4.8 MiB at 256 KiB to 16.7 MiB at 4 MiB. See the [raw sweep](stream-buffer-silesia.json).

## Decision

Keep preflight buffering for the listing gain. No extraction-speed improvement is claimed. Archives and metadata limits are unchanged. The extra preflight buffer is dropped before parallel decoding.

Independent review found no parser or cursor blocker. It tested large comment and extra fields and malformed directory records. Regression tests cover valid and damaged records across 64 KiB boundaries.

The seek-buffer implementation was reverted, and a cursor boundary regression test was kept. The libdeflate CRC change was reverted, because correctness alone is insufficient reason to keep a performance change. The streaming output buffer stays at 256 KiB, because no reliable extraction benefit justifies the larger buffers.

## Limits

These warm-cache local measurements do not establish Windows performance.

## Reproduce

Run `scripts/compare_zip_readers.py` with two `--reader LABEL native BINARY` arguments and `--runs 7`. Add `--operation list` for metadata-only timing.
