# M29. Overwrite at the speed of a fresh extraction

`--overwrite` into a folder that already holds an archive's files takes about as long as extracting into an empty folder, with every safety rule kept.

The test archive is a ZIP of 28,000 small files in 280 folders. On a second machine, an Apple M2 Max, on 12 November 2026, it took 6.5 s to extract with `--overwrite` into a folder that already held every file. The same archive took 1.2 s into an existing empty folder, so replacing is 5.3 times as slow as creating. A consumer saw the same gap on a 27,874-file archive, 27 s with Zipper against 3 s with tar.

Scope:

- Profile `--overwrite` into a full folder outside timed runs, and name the stage that costs the time. Candidates are the existence check, the swap or two-rename replace, the set-aside copies kept for rollback, and their cleanup.
- Change the stage that costs the most, one experiment at a time, keeping rollback on failure and cancel, the link checks and the concurrent-overwrite guarantee.
- Time each change interleaved on the second machine against the commit before it.

Exit checks:

1. A record times `--overwrite` into a full folder against an empty one on the 28,000-file archive, with the gap at most 1.5 times or the verdict naming what keeps it larger.
2. The exhaustive overwrite and cancel checks, `cargo xtask verify-sync overwrite`, and the concurrent-overwrite tests pass on the final code.
3. A planted break with `scripts/plant.py` shows each new check can fail.
