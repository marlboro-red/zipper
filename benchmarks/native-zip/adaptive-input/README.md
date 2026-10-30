# Adaptive ZIP file read-ahead experiment

Hypothesis: the fixed 8 KiB buffer reduced tiny-file amplification, but large
streaming entries now issue many small physical reads. The ZIP crate wraps its
compressed input in a standard BufReader, whose requests can be 8 KiB. Increasing
read-ahead only after sustained sequential reads may recover large-entry speed
without reintroducing the original small-file read amplification.

The isolated candidate starts at 8 KiB, promotes to 256 KiB after 64 KiB of
sequential reads when the next request is at least 8 KiB, and resets to 8 KiB on
actual seek movement. A clone starts small at the logical position. Cached
immutable archives are unchanged. Buffer resizing preserves the unread logical
position; added tests compare bytes across growth, seeks, clones and invalid
seeks. This is a prototype until repeatable extraction improvements justify it.

`.github/workflows/zip-adaptive-input.yml` builds before/after binaries and on
Windows also compares the checksum-pinned actual alpha executable. All readers
use normal memory budgets and four workers. Seven interleaved measured rounds,
warmup excluded, with direct byte/tree verification after every extraction.

### Actual Windows alpha reference

Same runs and archives as above; release timings are not from another machine.

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

## Final Windows-only mixed-size validation

[Run 40991500822](https://github.com/marlboro-red/zipper/actions/runs/40991500822)
builds `4d30e88` plus `windows-candidate.patch`, the final Windows-only change.
The historical `zip-adaptive-mixed.yml` workflow must run at that source revision.
The ZIP contains 2,000 grouped tiny files, one 32 MiB random file and one 32 MiB
compressible file: 69,016,064 input bytes, 34,290,590 ZIP bytes. Four workers,
seven interleaved rounds after warmup, default entry budgets and direct byte/tree
verification on every extraction. Native Windows release tests pass.

| Engine | Before median [range] ms | After median [range] ms | Before / after peak MiB |
|---|---|---|---|
| native | 472.92 [464.98, 1624.30] | 479.12 [452.49, 768.28] | 5.59 / 5.84 |
| native-fast | 509.28 [461.47, 913.72] | 474.92 [464.97, 1603.39] | 37.54 / 37.53 |

The ranges overlap substantially: no established mixed-tree gain or regression.
Retain only on Windows for the disjoint large random streaming improvement;
this is a bounded local result, not a claim of universal or state-of-the-art speed.
The additional input buffer is at most 248 KiB per active file reader outside
the compressed/decoded entry budget. The old published alpha asset is unchanged.
