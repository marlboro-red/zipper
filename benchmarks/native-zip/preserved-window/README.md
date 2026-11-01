# Preserved buffered window across ZIP seeks

Preserving buffered bytes across ZIP seeks cut grouped Linux extraction time by 8.7%, with separated timing ranges. The change is retained as an incremental gain, and other workloads show no established change.

## Question

The input read-ahead is now 8 KiB. Can preserving buffered bytes across ZIP header and data seeks reduce redundant reads and shared input-lock work?

The previous 256 KiB experiment was inconclusive on the local Mac. New Linux evidence identifies input processing as a major bottleneck. This is therefore a new baseline and platform investigation, not a reversal without evidence.

## Setup

The candidate patch changes only `Reader::seek` for file-backed cursors. It validates the target offset and uses `seek_relative` when the displacement fits. It falls back to an absolute seek for larger displacements. Memory-backed cursors are unchanged.

The `native` streaming mode is used so Linux's whole-archive cache cannot hide the difference.

[Run 40898089506](https://github.com/marlboro-red/zipper/actions/runs/40898089506) built source `93b1976` and the isolated candidate patch. Both readers have 8 KiB buffers. The run uses seven interleaved measured rounds after warmup and four workers. It compares exact bytes and the tree after each extraction.

Linux has 10,000 files per fixture. Windows has 1,000. Local M5 measurements add Silesia and the real dependency tree.

## Results

| Workload | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| Linux grouped | 76.57 [74.82–78.23] | 69.94 [66.84–71.73] | 7.96 / 7.93 |
| Linux flat | 203.96 [201.43–209.53] | 207.81 [201.69–210.97] | 7.54 / 7.38 |
| Windows grouped | 249.96 [240.87–3326.29] | 260.96 [236.68–865.75] | 5.46 / 5.46 |
| Windows flat | 477.15 [467.04–688.36] | 473.17 [453.35–657.23] | 5.30 / 5.29 |
| M5 flat | 328.94 [317.27–335.60] | 327.08 [311.26–340.20] | 8.67 / 8.75 |
| M5 Silesia | 128.86 [122.66–159.66] | 122.97 [121.09–145.23] | 3.52 / 3.53 |
| M5 dependency sources | 174.71 [170.62–195.22] | 178.50 [168.56–192.59] | 6.61 / 6.52 |

Grouped Linux extraction takes **8.7% less time**, with separated timing ranges. The other ranges overlap. Windows grouped samples have large outliers, which the report retains.

## Decision

Retain, based on a small, repeatable grouped Linux improvement. The candidate stayed outside production until this extraction evidence arrived.

This is an incremental gain, not another multiplier to combine with earlier experiments. The change adds no new cache allocation or dependency, and no format changes occur.

Independent review found no offset, cursor-position or concurrency blocker. It ran 20,000 randomized seek, read and clone operations, including integer boundaries. The existing boundary and concurrent-reader tests and the full Rust suite pass.

## Limits

Where ranges overlap, claim neither a speed gain nor an established regression. The Windows grouped samples include large outliers.

## Reproduce

`.github/workflows/zip-window.yml` builds the baseline, applies the isolated patch, and then tests and measures the candidate. The original candidate patch is retained for historical reproduction against `93b1976`. It is not intended to apply to a production revision that already contains the change.
