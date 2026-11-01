# Adaptive ZIP file read-ahead

Adaptive read-ahead is retained on Windows only, on the strength of one of ten Windows before and after comparisons. Streaming `native` extraction of the large random entry fell from 37.02 to 34.66 ms, about 6%, with a 0.87 ms gap between ranges. That returns it to the alpha's 35.21 ms level, with overlapping ranges, rather than passing it. The final mixed-size validation shows no established gain or regression.

## Question

The fixed 8 KiB buffer reduced tiny-file amplification, but large streaming entries now issue many small physical reads. The ZIP crate wraps its compressed input in a standard BufReader, whose requests can be 8 KiB. Can read-ahead that grows only after sustained sequential reads recover large-entry speed without bringing back the original small-file read amplification?

## Setup

The isolated candidate starts at 8 KiB. It promotes to 256 KiB after 64 KiB of sequential reads when the next request is at least 8 KiB. It resets to 8 KiB on actual seek movement. A clone starts small at the logical position. Cached immutable archives are unchanged.

Buffer resizing preserves the unread logical position. Added tests compare bytes across growth, seeks, clones and invalid seeks. The candidate was a prototype until repeatable extraction improvements justified it.

`.github/workflows/zip-adaptive-input.yml` builds before and after binaries. On Windows it also compares the checksum-pinned actual alpha executable. All readers use normal memory budgets and four workers. Each comparison runs seven interleaved measured rounds, warmup excluded, with direct byte and tree verification after every extraction.

## Results

The before and after samples for each platform are in the raw reports under `linux/` and `windows/` and in the `macos-*.json` files. Raw Linux large-random samples are missing, as Limits explains.

### Decisive Windows comparison

Windows ran five workloads with two engines each, for ten before and after comparisons. Only one has separate ranges. These rows come from `windows/large-random.json`.

| Engine | Before median [range] ms | After median [range] ms | Alpha median [range] ms |
|---|---|---|---|
| native | 37.02 [36.24, 38.40] | 34.66 [33.08, 35.37] | 35.21 [33.16, 44.23] |
| fast | 37.89 [36.48, 42.17] | 34.50 [32.92, 41.31] | 34.27 [32.89, 35.82] |

The native rows differ by about 6%, with 0.87 ms between the before minimum and the after maximum. The native-fast ranges overlap. Neither after median is established as faster than the alpha.

On Linux, large-mixed extraction regressed with separate ranges. Streaming `native` went from 123.41 [122.45, 123.88] to 127.49 [126.41, 131.05] ms, and native-fast went from 123.52 [122.57, 123.82] to 127.28 [126.53, 128.48] ms.

### Actual Windows alpha reference

These rows come from the same Windows runs and archives as the before and after comparison. The alpha timings are not from another machine.

| Workload | Engine | Alpha median [range] ms | Candidate median [range] ms |
|---|---|---|---|
| flat | native | 474.85 [401.60, 495.49] | 592.37 [461.32, 687.43] |
| flat | fast | 464.74 [433.80, 3387.86] | 505.08 [457.88, 843.56] |
| grouped | native | 233.74 [229.49, 247.10] | 231.80 [216.82, 275.20] |
| grouped | fast | 239.36 [216.88, 397.89] | 219.73 [199.73, 246.51] |
| large-compressible | native | 25.94 [25.32, 27.14] | 25.84 [25.55, 26.52] |
| large-compressible | fast | 177.57 [169.99, 188.38] | 36.49 [36.05, 39.08] |
| large-mixed | native | 94.83 [92.48, 109.10] | 94.51 [93.55, 105.57] |
| large-mixed | fast | 93.61 [93.20, 107.33] | 95.93 [94.17, 107.64] |
| large-random | native | 35.21 [33.16, 44.23] | 34.66 [33.08, 35.37] |
| large-random | fast | 34.27 [32.89, 35.82] | 34.50 [32.92, 41.31] |

### Final Windows-only mixed-size validation

[Run 40991500822](https://github.com/marlboro-red/zipper/actions/runs/40991500822) builds `4d30e88` plus `windows-candidate.patch`, the final Windows-only change.

The ZIP contains 2,000 grouped tiny files, one 32 MiB random file and one 32 MiB compressible file. That is 69,016,064 input bytes and 34,290,590 ZIP bytes. The run uses four workers, seven interleaved rounds after warmup, and default entry budgets. It verifies bytes and the tree directly on every extraction. Native Windows release tests pass.

| Engine | Before median [range] ms | After median [range] ms | Before / after peak MiB |
|---|---|---|---|
| native | 472.92 [464.98, 1624.30] | 479.12 [452.49, 768.28] | 5.59 / 5.84 |
| native-fast | 509.28 [461.47, 913.72] | 474.92 [464.97, 1603.39] | 37.54 / 37.53 |

The ranges overlap substantially. There is no established mixed-tree gain or regression.

## Decision

Retain only on Windows, for the disjoint large random streaming improvement. That improvement is the only disjoint Windows result of ten, and it brings native extraction back to the alpha's level rather than beyond it. The additional input buffer is at most 248 KiB per active file reader, outside the compressed and decoded entry budget. The old published alpha asset is unchanged.

## Limits

This is a bounded local result, not a claim of universal or state-of-the-art speed. The mixed-size validation shows neither a gain nor a regression.

The keep rests on one comparison with a 0.87 ms gap between ranges. A rerun could close that gap.

`linux/large-random.json` is not JSON. It holds the experiment's original Markdown notes, so the raw Linux large-random samples are missing from this record. Those notes report a Linux large-random regression, from 39.89 [39.29, 40.43] to 46.57 [46.18, 47.45] ms for `native` and from 39.37 [39.14, 40.59] to 46.44 [46.16, 47.42] ms for native-fast. No retained samples can confirm those summaries.

## Reproduce

Run the historical `zip-adaptive-mixed.yml` workflow at source revision `4d30e88` with `windows-candidate.patch`. `.github/workflows/zip-adaptive-input.yml` builds the original before and after comparison.
