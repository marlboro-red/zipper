# Buffered-window preservation experiment

Hypothesis: after the retained input read-ahead reduction to 8 KiB, preserving
buffered bytes across ZIP header/data seeks can reduce redundant reads and
shared input-lock work. The previous 256 KiB experiment was inconclusive on the
local Mac. New Linux evidence identifies input processing as a major bottleneck,
so this is a new baseline/platform investigation, not a reversal without evidence.

The candidate patch changes only `Reader::seek` for file-backed cursors. It
validates the target offset, uses `seek_relative` when the displacement fits,
and falls back to an absolute seek for larger displacements. Memory-backed
cursors are unchanged. `.github/workflows/zip-window.yml` builds the baseline,
applies this isolated patch, tests and measures the candidate. The `native`
streaming mode is used so Linux's whole-archive cache cannot hide the difference.

The candidate initially remained outside production pending extraction evidence.
The retained decision and independent review are recorded below.

## Result and retention

Retain based on a small, repeatable grouped Linux improvement. This is an
incremental gain, not another multiplier to combine with earlier experiments.
[Run 40898089506](https://github.com/marlboro-red/zipper/actions/runs/40898089506)
built source `93b1976` and the isolated candidate patch. Both readers have 8 KiB
buffers. Seven interleaved measured rounds after warmup, four workers, exact
byte/tree comparison after each extraction. Linux has 10,000 files per fixture;
Windows has 1,000. Local M5 measurements add Silesia and the real dependency tree.

| Workload | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| Linux grouped | 76.57 [74.82–78.23] | 69.94 [66.84–71.73] | 7.96 / 7.93 |
| Linux flat | 203.96 [201.43–209.53] | 207.81 [201.69–210.97] | 7.54 / 7.38 |
| Windows grouped | 249.96 [240.87–3326.29] | 260.96 [236.68–865.75] | 5.46 / 5.46 |
| Windows flat | 477.15 [467.04–688.36] | 473.17 [453.35–657.23] | 5.30 / 5.29 |
| M5 flat | 328.94 [317.27–335.60] | 327.08 [311.26–340.20] | 8.67 / 8.75 |
| M5 Silesia | 128.86 [122.66–159.66] | 122.97 [121.09–145.23] | 3.52 / 3.53 |
| M5 dependency sources | 174.71 [170.62–195.22] | 178.50 [168.56–192.59] | 6.61 / 6.52 |

Grouped Linux extraction takes **8.7% less time**, with separated timing ranges.
Other ranges overlap; claim neither a speed gain nor an established regression
there. Windows grouped samples have large outliers, retained in the report.
No new cache allocation or dependency is introduced, and no format changes occur.

Independent review found no offset, cursor-position or concurrency blocker and
ran 20,000 randomized seek/read/clone operations including integer boundaries.
The existing boundary/concurrent-reader tests and full Rust suite pass. The
original candidate patch is retained for historical reproduction against
`93b1976`; it is not intended to apply to a production revision already containing
the change.
