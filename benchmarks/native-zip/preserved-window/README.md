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

No production change is retained until extraction timings demonstrate benefit.
Independent review and exact byte/tree verification are required.
