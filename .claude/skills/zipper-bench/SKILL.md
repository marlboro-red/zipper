---
name: zipper-bench
description: Run, vet and record a Zipper performance experiment. Use before measuring a speedup, comparing Zipper with another tool, keeping or reverting an optimization, or writing any number into a record or docs/results.md.
---

# Zipper bench

A number is a claim. Every claim names its workload, its configuration, its run count, its spread and what limits it. The scripts enforce the mechanical parts. This skill covers the judgment.

## Before measuring

1. Write the sentence you expect to publish, with the number left blank. For example, "native-fast extracts dependency sources ___× faster than bsdtar at one thread".
2. Name the baseline a user gets today. Compare with the default path, not with an old or slower path. If a change fixes a regression, say "fixes a regression" and also compare it with the best existing path.
3. Pick the script:
   - `scripts/extraction_matrix.py` for ZIP extraction against bsdtar and 7-Zip.
   - `scripts/compare_formats.py` for classic formats through Zipper.
   - `scripts/compare_zstd.py` for tar+zstd and tar+pzstd round trips.
   - `scripts/sweep.py` for native level and block-size settings.
   - `scripts/compare_revisions.py` for one change between two Zipper binaries.
4. Check `uptime`. If the load is high, find out why, and say so in the record. Never run two experiments at once.

## While measuring

- Use release binaries. Record the revision and the binary hash, which the scripts write.
- Use at least seven measured rounds after one warmup. The scripts interleave the order.
- For a comparison with another tool, keep the 1-thread Zipper rows that the scripts add. Report wall time and CPU time side by side. The scripts refuse to compare with an external reader without a 1-thread row.
- Profile only in runs you do not report.

## Vet the result

Answer each question from the JSON, not from memory.

1. **What limits it?** Name the limiter, such as decode CPU, output syscalls, input reads or memory. Cite a profile or trace. If you cannot name it, the verdict is inconclusive.
2. **Is it like for like?** Same archive bytes, thread count, metadata work, sync and session. The JSON `parity` section lists what remains unmatched. Put every remaining difference in Limits.
3. **Does it beat the spread?** Overlapping ranges mean no established difference. A median gap inside the session-to-session drift is not a result. The same archive has differed by 11% between sessions.
4. **Did anything fail?** The scripts stop on a failed command or a byte mismatch. Check that the run finished every round.
5. **Does it hold by default?** If a gain needs a non-default flag, budget or platform, the verdict says so.
6. **Is it the user's path?** Report end-to-end extraction next to any component number.

## Record it

1. Write the record with the `zipper-docs` skill and its six fixed headings.
2. Put the verdict sentence under the title. It carries the caveat that matters most, not only the best ratio.
3. Add a row to `benchmarks/README.md`. Never edit `docs/results.md`. Run `cargo xtask bench` to remeasure the scoreboard and regenerate it.
4. Keep or revert the change. Log a reverted experiment as carefully as a kept one.

## Signs you skipped this

- A ratio without a thread count, or a speedup without CPU time.
- "Faster" over overlapping ranges.
- A best fixture or best reader quoted without the others.
- A comparison against a path the default no longer uses.
