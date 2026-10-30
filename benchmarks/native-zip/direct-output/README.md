# Direct ZIP output experiment

Hypothesis: ZIP decoding already uses whole-entry or 256 KiB streaming output
buffers. Removing the additional 8 KiB BufWriter per file may avoid allocations
and copies without increasing the usual write-call count. A decoder returning
many short chunks could invalidate that assumption, so test both tiny-file and
large incompressible/compressible workloads, native and native-fast.

`candidate.patch` changes only the output wrapper. All CRC, decoded-size,
create-new, staging and error propagation remain required. The manual workflow
builds baseline and candidate from the same checkout, tests the candidate and
runs seven interleaved release rounds after warmup, with exact byte/tree checks.
Four workers, unchanged default budgets, identical ZIPs. It also captures Linux
syscall summaries separately from timed runs. The change is experimental until
end-to-end ranges establish a benefit without workload regressions.

## Decision: reject

[Run 40997540680](https://github.com/marlboro-red/zipper/actions/runs/40997540680)
uses baseline `1174f08` plus the recorded patch. The historical workflow must
run at that baseline. Windows has 1,000 tiny files per shape; Linux has 10,000.
Mac timing ranges also overlap (flat 354.98→368.34 ms, Silesia 95.47→92.85 ms).
No general extraction benefit is established. One Linux mixed native-fast pair
has a small disjoint 81.17→80.18 ms difference, while the native pair through
the same streaming output path overlaps. This is insufficient justification for
changing the output policy across workloads. Production keeps BufWriter.

All samples, bytes, archive/binary hashes and peak memory are retained. These
are warm-cache process timings, not durable output completion. The syscall
summaries are separate untimed runs and include process startup and all threads.

### Linux

| Workload (ZIP bytes) | Engine | Before median [range] ms | After median [range] ms | Peak MiB before / after |
|---|---|---|---|---|
| flat (2,915,393) | native | 119.82 [118.90, 121.49] | 118.60 [117.14, 123.28] | 7.48 / 7.46 |
| flat (2,915,393) | fast | 121.74 [118.94, 234.69] | 120.93 [118.74, 123.87] | 10.69 / 10.76 |
| grouped (3,206,793) | native | 52.98 [49.51, 57.27] | 52.55 [50.91, 54.01] | 7.97 / 7.98 |
| grouped (3,206,793) | fast | 50.92 [48.77, 53.92] | 49.52 [48.01, 51.86] | 11.46 / 11.41 |
| large-compressible (81,673) | native | 20.95 [20.83, 21.37] | 21.32 [20.93, 21.99] | 3.86 / 3.86 |
| large-compressible (81,673) | fast | 36.89 [35.83, 37.34] | 36.55 [35.58, 37.69] | 35.61 / 35.62 |
| large-mixed (33,821,383) | native | 81.03 [80.33, 82.09] | 80.47 [79.96, 81.38] | 3.68 / 3.57 |
| large-mixed (33,821,383) | fast | 81.17 [80.96, 85.19] | 80.18 [79.69, 80.84] | 3.68 / 3.68 |
| large-random (33,564,802) | native | 21.90 [21.39, 23.32] | 22.09 [21.32, 22.84] | 3.55 / 3.49 |
| large-random (33,564,802) | fast | 22.22 [21.51, 22.60] | 21.96 [21.22, 23.62] | 3.55 / 3.50 |

### Windows

| Workload (ZIP bytes) | Engine | Before median [range] ms | After median [range] ms | Peak MiB before / after |
|---|---|---|---|---|
| flat (292,411) | native | 490.43 [432.93, 829.82] | 509.33 [472.72, 624.56] | 5.04 / 5.05 |
| flat (292,411) | fast | 513.08 [498.74, 728.63] | 510.67 [487.77, 1003.62] | 5.11 / 5.13 |
| grouped (321,551) | native | 362.41 [272.49, 701.59] | 293.11 [274.30, 668.38] | 5.30 / 5.12 |
| grouped (321,551) | fast | 304.23 [278.95, 625.68] | 299.91 [271.40, 652.20] | 5.13 / 5.12 |
| large-compressible (81,673) | native | 32.92 [32.16, 33.33] | 32.89 [32.37, 33.84] | 3.86 / 3.86 |
| large-compressible (81,673) | fast | 45.55 [45.14, 46.61] | 45.30 [44.40, 47.09] | 35.71 / 35.71 |
| large-mixed (33,821,383) | native | 121.79 [119.65, 122.94] | 122.26 [121.43, 125.14] | 4.15 / 4.11 |
| large-mixed (33,821,383) | fast | 122.78 [119.28, 123.99] | 122.56 [120.91, 144.51] | 4.12 / 3.96 |
| large-random (33,564,802) | native | 42.62 [41.78, 43.53] | 43.03 [42.10, 59.54] | 4.10 / 4.09 |
| large-random (33,564,802) | fast | 42.66 [41.94, 45.49] | 42.59 [41.34, 44.33] | 4.11 / 4.09 |


Linux grouped native-fast syscall counts are unchanged: 10,006 openat,
8,000 write and 10,006 close calls in both variants (10,000 files, 2,000 empty).
Removing the wrapper does not remove filesystem operations. Summed strace times
are instrumented and overlap across threads; they are not extraction wall time.
This reinforces namespace/output operations as the small-file research target.
