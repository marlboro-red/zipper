# Buffered ZIP preflight experiment

Hypothesis: central-directory validation currently performs one small file read
and seek per entry. A 64 KiB BufReader with relative skips can retain validation
bytes between records and reduce overhead without relaxing validation.

Apple M5, macOS, release binary; four workers, one warmup and seven interleaved
runs. Baseline production source is aba6091 (same reader as Windows alpha
55bc598); candidate changes only preflight buffering. Binary hashes and all
samples are retained in the linked JSON reports. Every sample is accompanied by
a complete extraction and direct byte, size, and tree comparison; for listing,
that extraction is outside the timed interval. The complete before/after listing
also matches byte for byte (4,041 entries including directories).

| Workload / operation | Before ms: median [min, max] | After ms: median [min, max] | Peak RSS MiB before / after |
|---|---:|---:|---:|
| 4,000 files: listing | 10.28 [10.11, 12.62] | 8.42 [8.24, 8.54] | 5.4 / 5.4 |
| 4,000 files: extraction | 127.06 [125.82, 130.79] | 124.67 [122.12, 134.07] | 7.7 / 7.3 |
| Silesia: extraction | 117.29 [116.52, 118.81] | 117.93 [116.36, 118.63] | 5.0 / 4.7 |

Keep: listing latency falls about 18%, with disjoint observed ranges. Extraction
ranges overlap, so no extraction-speed improvement is claimed. Archives and
metadata limits are unchanged; the extra preflight buffer is dropped before
parallel decoding. Independent review found no parser/cursor blocker and tested
large comment/extra fields and malformed directory records. Regression tests
cover valid and damaged records across 64 KiB boundaries.

Raw reports: [listing](preflight-list-small-files.json),
[small-file extraction](preflight-extract-small-files.json),
[Silesia extraction](preflight-extract-silesia.json).
Reproduce with `scripts/compare_zip_readers.py`, two `--reader LABEL native BINARY`
arguments, `--runs 7`, and `--operation list` for metadata-only timing.

## Rejected experiments

- Preserve the shared ZIP reader's buffer on seeks: small-file and Silesia
  extraction ranges overlap. Reverted implementation; retained a cursor boundary
  regression test. Independent review and a 10,000-operation cursor comparison
  found no correctness blocker. [Small files](buffered-seek-small-files.json),
  [Silesia](buffered-seek-silesia.json).
- Replace v2's CRC32 implementation with the already-linked libdeflate CRC:
  four-corpus creation/extraction measurements were noisy and did not establish a
  reliable improvement. Reverted. Independent equivalence checks passed, but
  correctness alone is insufficient reason to keep a performance change.
  [Raw results](../v2/libdeflate-crc.json).

These warm-cache local measurements do not establish Windows performance.
