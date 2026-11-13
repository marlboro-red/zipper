---
name: zipper-review
description: Independent adversarial review for changes to archive parsing, extraction safety or concurrency. Use before committing any change to src/native_zip.rs, src/zip_reader.rs, src/native_tar.rs, src/native_7z.rs, src/staging.rs or the extraction publish path, and when evaluating another ZIP implementation.
---

# Zipper review

The author always finds a reason the code is safe. A reviewer who has not seen that reasoning finds the failure. Review proves failures with inputs, not arguments.

## Run a review

1. Spawn the `zipper-adversary` agent defined in `.claude/agents/zipper-adversary.md`. It has no edit tools and fixes the scratch directory, the binary it drives and the report shape. Its brief names the scope, the diff command and a review name. Do not give it your conclusion.
2. Each finding comes back CONFIRMED with a crafted archive or a failing run, or PLAUSIBLE with the missing proof named.
3. Fix every CONFIRMED finding in the same change, with a test that fails before the fix.
4. Write the test before the fix, and run it to see it fail.
5. Plant breaks with `scripts/plant.py`, never by hand. It applies the edit only if it matches once. It counts a catch only when the output matches `--caught-when`, so a compile error or a timeout reads as missed. It always restores the file, and it rebuilds the release binary after a plant in `src/`.

## Checklist for a hostile archive

- **Memory against the old code.** On archives whose names, extra fields, comments or entry counts fill the documented limits, peak memory with `/usr/bin/time -l` is no higher than the code it replaces. Two parser changes in M22 regressed here, and only a review caught each.
- **Sizes.** Decoded length is checked against the declared size during decoding, not after. A declared size of zero still gets a CRC check. Allocation sizes never come straight from the archive.
- **Counts.** Entry counts and central-directory size stay under the documented limits before allocation.
- **Budgets.** Every allocation the archive controls is charged to a documented limit, including derived ones such as implied parent directories, link targets and work queued for other threads. Ask the reviewer for peak memory at 1 and 64 threads on a slow destination.
- **Paths.** Absolute paths, `..`, drive letters, backslashes, NUL bytes, empty names, duplicate names and names that collide on a case-insensitive filesystem are rejected or handled.
- **Structure.** Overlapping entries, local headers that disagree with the central directory, ambiguous end records, truncation at every boundary, and ZIP64 fields that are present but inconsistent.
- **Output.** Nothing is published on failure. Output goes to a private staging directory. An existing destination is refused. No path follows a symlink out of the destination.
- **Concurrency.** Workers share no mutable output path. A failure in one worker cancels the others and cleans up. Memory budgets stay bounded under contention.

## Protect real archives

A stricter check can reject archives that real tools write. Before committing a parser change, run `tests/zip_corpus.rs` and extract real archives from every writer you have. When a review finds a real archive that a check rejects, add its shape to the corpus source in `scripts/make_zip_corpus.py` and regenerate.

## Evaluate another implementation

Use the same checklist on its source before trusting a benchmark from it. Record what it does and does not check. A faster extractor that skips a check above is not a like-for-like comparison.

## Signs you skipped this

- The author wrote the only tests for a parser change.
- A safety claim has no crafted input behind it.
- A concurrency change shipped without a run under contention.
