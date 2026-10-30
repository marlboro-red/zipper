# ZIP decoder selection experiment

Hypothesis: whole-entry allocation, decoding, checksum and output can be slower
than bounded streaming for large, very compressible entries. Avoid choosing the
whole-entry path purely because it fits memory. On the local M5, five repeat
patterns at 16 MiB favor streaming, while two incompressible patterns favor
whole-entry decoding. The selection candidate streams extraction entries of at
least 16 MiB with compressed size at most 1/128 of expanded size. This is a
conservative measured region, not a claimed universal optimal threshold.

Only extraction policy changes. Integrity-only reads, metadata/CRC validation,
shared budget and incompressible entry selection stay unchanged. The patch is
isolated from scratch allocation and output-wrapper experiments.

The shape generator varies zeros, text, long runs and random repeat distances,
plus incompressible input. Seven interleaved release runs after warmup, four
workers and default budgets, identical input ZIP for each before/after pair;
direct original/extracted bytes and full tree checks after every run. Timings
include process startup and output; caches are warm and durability is not
requested. Cross-platform results must justify any retained platform policy.

## Decision: retain the selection rule

[Portable run 40998175627](https://github.com/marlboro-red/zipper/actions/runs/40998175627)
builds `11d4d66` plus `candidate.patch`. Reproduce the historical workflow at that
baseline. Windows uses static CRT, Rust 1.96.0 and windows-2022; Linux uses
ubuntu-24.04. Mac uses Apple M5/macOS 26.7.1 and the same runtime baseline
`0169f9b`; intervening commits contain benchmarks only. Do not compare absolute
times across these machines or multiply these gains by older experiments.

Linux improves all five eligible repeat patterns with disjoint ranges, from
15–20 ms to 7–14 ms. Windows improves zero, runs and the two short-period patterns
with disjoint ranges; text overlaps. Mac improves runs, text and the 8 KiB repeat
pattern with disjoint ranges; zero and 256-byte repeat results overlap.
Peak process memory for these entries drops from about 19.5 to 3.7 MiB on Linux,
19.7 to 3.9 MiB on Windows and 18.7 to 2.8 MiB on Mac.

Incompressible entries do not change decoder paths; their observed ranges
overlap. The Windows after/random sample includes a 206.79 ms outlier, retained
in the table and raw data. No incompressible or mixed-tree speed gain is claimed.
The Mac mixed-tree test also overlaps (38.98→38.03 ms), with peak memory
35.55→35.84 MiB; its random file still needs whole-entry buffers.

This is a synthetic workload result and a conservative selection policy, not
proof of the optimal ratio/size threshold or world-leading general ZIP speed.
The existing shared budget still decides which other entries can use whole-entry
decoding; scheduling can affect that choice on mixed trees. `--zip-buffer-mib`
remains a maximum budget, not a requirement to buffer every qualifying entry.
Default compatibility-backend ZIP behavior is unchanged.

Each table cell is median [minimum, maximum] milliseconds. Memory is the maximum
observed process peak in MiB. All original samples, hashes, archive byte sizes,
run ordering and direct-byte recovery counts are retained.

### macos

| Workload (ZIP bytes) | Before ms | After ms | Peak MiB before / after |
|---|---|---|---|
| combined (17,001,723) | 38.98 [36.20, 41.32] | 38.03 [37.28, 40.54] | 35.55 / 35.84 |
| period-256 (65,551) | 6.45 [6.31, 6.65] | 5.74 [5.22, 8.74] | 18.67 / 2.80 |
| period-32768 (16,782,476) | 8.43 [8.15, 8.58] | 8.56 [8.27, 8.91] | 34.61 / 34.62 |
| period-8192 (115,072) | 6.54 [6.45, 7.01] | 5.70 [5.57, 5.84] | 18.73 / 2.80 |
| random (16,782,464) | 8.88 [8.46, 9.67] | 8.95 [8.53, 9.04] | 34.61 / 34.64 |
| runs (29,392) | 6.41 [6.22, 6.48] | 5.38 [5.09, 5.52] | 18.58 / 2.83 |
| text (40,855) | 7.55 [7.22, 7.76] | 7.11 [6.97, 7.17] | 18.66 / 2.80 |
| zero (16,434) | 7.57 [7.05, 10.51] | 6.30 [5.76, 9.75] | 18.59 / 2.83 |

### linux

| Workload (ZIP bytes) | Before ms | After ms | Peak MiB before / after |
|---|---|---|---|
| period-256 (65,551) | 17.57 [16.32, 17.84] | 7.64 [7.53, 8.00] | 19.68 / 3.79 |
| period-32768 (16,782,476) | 26.53 [23.30, 28.10] | 27.53 [23.21, 28.21] | 35.55 / 35.46 |
| period-8192 (115,072) | 16.91 [14.79, 17.73] | 7.56 [7.39, 7.70] | 19.68 / 3.78 |
| random (16,782,464) | 28.30 [24.24, 28.76] | 28.14 [25.76, 28.45] | 35.49 / 35.46 |
| runs (29,392) | 15.32 [13.94, 17.03] | 7.11 [6.89, 7.21] | 19.54 / 3.72 |
| text (40,855) | 20.29 [18.82, 20.45] | 13.74 [13.20, 14.14] | 19.50 / 3.68 |
| zero (16,434) | 16.10 [14.03, 17.06] | 6.90 [6.59, 7.10] | 19.43 / 3.62 |

### windows

| Workload (ZIP bytes) | Before ms | After ms | Peak MiB before / after |
|---|---|---|---|
| period-256 (65,551) | 16.19 [15.94, 16.52] | 12.93 [12.62, 13.18] | 19.68 / 3.74 |
| period-32768 (16,782,476) | 22.32 [22.07, 22.81] | 22.45 [21.98, 22.66] | 35.61 / 35.64 |
| period-8192 (115,072) | 16.17 [16.07, 17.18] | 13.17 [12.91, 13.56] | 19.72 / 3.95 |
| random (16,782,464) | 22.44 [22.13, 22.92] | 22.72 [22.53, 206.79] | 35.64 / 35.64 |
| runs (29,392) | 16.99 [16.41, 17.19] | 12.11 [11.95, 12.41] | 19.67 / 3.87 |
| text (40,855) | 20.64 [19.22, 20.87] | 19.15 [18.99, 19.84] | 19.69 / 3.90 |
| zero (16,434) | 17.11 [16.21, 74.61] | 12.32 [11.99, 14.57] | 19.63 / 3.89 |

## Validation and reproduction

The local full Rust suite, fmt and Clippy pass. Native Windows and Linux release
suites pass in the workflow. All timed extractions recover original bytes and
tree membership. Independent review additionally checked valid and malformed
entries at 16 MiB−1, 16 MiB and 16 MiB+1, including bad CRCs and sizes, with no
published output or staging residue after failures. Integrity-only selection is
unchanged. A separate original/extracted `.zipper` CLI comparison is retained in
`native-roundtrip.json`; v1/v2 compatibility and selected-file tests also pass.

```sh
python3 scripts/make_zip_decoder_corpus.py /tmp/decoder-shapes
python3 scripts/compare_zip_readers.py /tmp/decoder-shapes/text.zip /tmp/decoder-shapes/text \
  --reader before native-fast /path/to/before \
  --reader after native-fast /path/to/after --runs 7 --threads 4
```

The Mac mixed-tree ZIP combines the generated 16 MiB `text` and `random` entries
with 1,000 files named `group-{i//20}/{i}.txt`, each containing `record {i}\n`
repeated 16 times. It is made with Python ZIP_DEFLATED, preserving the top-level
source directory; source bytes total 33,728,672. This auxiliary test is not part
of the portable shape workflow.
