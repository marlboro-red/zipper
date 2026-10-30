# Lazy ZIP streaming scratch experiment

Hypothesis: every Rayon task eagerly zeroes 256 KiB of streaming scratch even
when all its entries use whole-entry decoding or contain only tiny payloads.
Allocate scratch only on streaming fallback, growing on demand up to 256 KiB.
Keep at least one byte for empty entries so decoder EOF and CRC checks still run.
The scratch remains reusable within a task; no compressed/decoded budget change.

The patch is isolated from the direct-output experiment. Measure native and
native-fast end-to-end extraction and peak memory on identical immutable ZIPs,
four workers, seven interleaved release runs after warmup, direct original versus
extracted bytes/tree verification after every run. Small and large workloads
check both saved allocation and potential extra growth/copying cost.

## Decision: no speed benefit established; not retained

[Run 40998111124](https://github.com/marlboro-red/zipper/actions/runs/40998111124)
uses source `9e2ecad` plus the recorded patch. Reproduce the historical workflow
at that baseline. All timing ranges overlap on Linux, Windows and Mac. Local
Mac flat medians are 359.62→356.51 ms native and 350.39→364.29 ms native-fast;
Silesia native-fast is 94.06→95.40 ms. Do not claim an extraction improvement.

There is a measured memory benefit for tiny files: approximately 0.7–1 MiB lower
peak process memory on Linux and Windows. This is a viable future memory-focused
tradeoff, but not retained in the present speed work. Growing scratch can also
introduce allocations/copies that the eager fixed buffer avoids on mixed work.
Large streaming RSS does not consistently improve. The patch remains available
for independent evaluation; no production scratch-allocation change is shipped.

Independent review found no correctness issue and tested 38 valid/malformed
Stored/Deflate cases, including zero-length outputs, bad CRCs, wrong decoded
sizes and cleanup. Scratch is per Rayon job state, not necessarily exactly once
per worker. A nonzero read buffer is required to reach EOF-triggered CRC checks.
The corresponding empty-entry CRC regression test is retained with the later
decoder-selection change.

Warm-cache process timings, four workers, seven interleaved rounds after warmup.
Windows tiny workloads have 1,000 files; Linux has 10,000. Direct byte/tree
verification after every extraction. Cells are median [min, max] ms; memory is
maximum observed process peak MiB. Raw samples and hashes remain alongside.

### linux

| Workload (ZIP bytes) | Engine | Before ms | After ms | Peak MiB before / after |
|---|---|---|---|---|
| flat (2,915,393) | native | 296.90 [293.12, 298.66] | 296.27 [293.52, 297.30] | 7.54 / 6.69 |
| flat (2,915,393) | fast | 295.43 [292.83, 297.14] | 295.73 [292.82, 297.27] | 10.63 / 9.70 |
| grouped (3,206,793) | native | 113.42 [108.74, 115.88] | 111.85 [108.97, 117.90] | 7.90 / 7.18 |
| grouped (3,206,793) | fast | 100.77 [100.34, 102.65] | 99.85 [99.27, 102.86] | 11.27 / 10.43 |
| large-compressible (81,673) | native | 30.10 [29.87, 30.32] | 30.04 [29.80, 30.85] | 3.68 / 3.70 |
| large-compressible (81,673) | fast | 43.50 [43.07, 44.23] | 43.55 [43.09, 43.87] | 35.55 / 35.58 |
| large-mixed (33,821,383) | native | 119.89 [119.04, 120.51] | 120.34 [119.78, 121.03] | 3.44 / 3.70 |
| large-mixed (33,821,383) | fast | 120.34 [119.71, 121.24] | 120.56 [119.39, 121.00] | 3.49 / 3.70 |
| large-random (33,564,802) | native | 40.18 [39.59, 44.12] | 40.01 [39.56, 41.24] | 3.43 / 3.66 |
| large-random (33,564,802) | fast | 40.35 [39.69, 41.45] | 40.39 [39.56, 40.84] | 3.43 / 3.70 |

### windows

| Workload (ZIP bytes) | Engine | Before ms | After ms | Peak MiB before / after |
|---|---|---|---|---|
| flat (292,411) | native | 493.09 [472.16, 1141.05] | 523.88 [487.57, 3293.27] | 5.13 / 4.16 |
| flat (292,411) | fast | 501.05 [484.69, 816.20] | 493.58 [485.00, 1215.60] | 5.06 / 4.19 |
| grouped (321,551) | native | 358.47 [344.46, 486.26] | 378.44 [314.82, 387.29] | 5.24 / 4.24 |
| grouped (321,551) | fast | 361.14 [317.31, 557.38] | 499.74 [285.58, 981.86] | 5.12 / 4.21 |
| large-compressible (81,673) | native | 32.18 [31.91, 33.23] | 32.37 [32.18, 32.73] | 3.86 / 3.89 |
| large-compressible (81,673) | fast | 35.89 [35.61, 36.75] | 35.80 [35.10, 37.85] | 35.69 / 35.45 |
| large-mixed (33,821,383) | native | 117.05 [116.23, 118.46] | 118.72 [117.54, 126.16] | 4.11 / 4.14 |
| large-mixed (33,821,383) | fast | 117.59 [117.12, 130.31] | 118.50 [117.57, 130.43] | 4.12 / 3.97 |
| large-random (33,564,802) | native | 40.28 [39.88, 41.88] | 40.29 [39.51, 40.73] | 4.10 / 4.11 |
| large-random (33,564,802) | fast | 40.12 [39.87, 40.28] | 40.31 [39.74, 46.75] | 4.11 / 3.96 |
