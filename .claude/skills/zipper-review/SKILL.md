---
name: zipper-review
description: Independent adversarial review for changes to archive parsing, extraction safety or concurrency. Use before committing any change to src/native_zip.rs, src/zip_reader.rs, src/v2.rs, src/v1.rs, src/pipeline.rs or the extraction publish path, and when evaluating another ZIP implementation.
---

# Zipper review

The author always finds a reason the code is safe. A reviewer who has not seen that reasoning finds the failure. Review proves failures with inputs, not arguments.

## Run a review

1. Spawn a reviewer agent that has not seen your reasoning. Give it the diff (`git diff`), the contract in `docs/reference/format.md` or `docs/reference/zip-reader.md`, and this checklist. Do not give it your conclusion.
2. Tell it not to edit repository files. It writes crafted inputs and scripts under a scratch directory.
3. Require each finding to be CONFIRMED with a crafted archive or a failing run, or marked PLAUSIBLE with the missing proof named.
4. Fix every CONFIRMED finding in the same change, with a test that fails before the fix.

## Checklist for a hostile archive

- **Sizes.** Decoded length is checked against the declared size during decoding, not after. A declared size of zero still gets a CRC check. Allocation sizes never come straight from the archive.
- **Counts.** Entry counts and central-directory size stay under the documented limits before allocation.
- **Paths.** Absolute paths, `..`, drive letters, backslashes, NUL bytes, empty names, duplicate names and names that collide on a case-insensitive filesystem are rejected or handled.
- **Structure.** Overlapping entries, local headers that disagree with the central directory, ambiguous end records, truncation at every boundary, and ZIP64 fields that are present but inconsistent.
- **Output.** Nothing is published on failure. Output goes to a private staging directory. An existing destination is refused. No path follows a symlink out of the destination.
- **Concurrency.** Workers share no mutable output path. A failure in one worker cancels the others and cleans up. Memory budgets stay bounded under contention.

## Evaluate another implementation

Use the same checklist on its source before trusting a benchmark from it. Record what it does and does not check. A faster extractor that skips a check above is not a like-for-like comparison.

## Signs you skipped this

- The author wrote the only tests for a parser change.
- A safety claim has no crafted input behind it.
- A concurrency change shipped without a run under contention.
