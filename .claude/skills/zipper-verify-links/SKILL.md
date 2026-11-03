---
name: zipper-verify-links
description: Check that archive links never resolve outside the destination, on every pair of small destination and archive trees in every merging `Existing` mode. Use before committing any change to the link check in src/staging.rs (`ensure_links_stay_in_archive_paths`, `owner_after_publish`, `join_rest`), to `create_symlink` or `ensure_link_stays_inside` in src/lib.rs, and when a published link escapes the destination.
---

# Verify that archive links stay inside

An archive link can escape the destination without any `..` in its target, by passing through a link the destination already holds, or through another archive link that leads there. `tests/link_escape_exhaustive.rs` runs the real ZIP reader on every small case and follows each published link on the real filesystem, so it sees what the operating system resolves, not what the code believes.

## The rules

| Rule | Harm if it breaks |
| --- | --- |
| Every link the archive placed lands inside the destination, followed step by step on disk | Any write or read through the link reaches files the user never chose |
| A refused run leaves the destination unchanged and no staging behind | A half-merged folder after an error |
| Nothing outside the destination changes | An archive writes outside |

## What it covers

- The destination's `a` and `b` each hold nothing, a file, a folder, a link to a folder outside, or a folder holding such a link `k`. That gives 25 destinations.
- The archive's `a` and `b` each hold nothing, a file, a folder, or a link to `.`, `a` or `b`. A link `l` targets `a/k/s`, `b/k/s`, `a/s`, `k/s`, `a` or `.`, or is absent. That gives 252 archives.
- Each pair runs in `Overwrite`, `Skip`, `RenameNew` and `RenameExisting`, 25,200 runs in all. All of them take about 26 seconds on a Mac.
- It runs only when invoked, never on a push or in CI. Run it before a release, and when the gate's `verify` step reports that its covered code changed. Without `ZIPPER_LINK_ESCAPE=1` the test prints that it was skipped.
- The oracle is a property, not a copy of the code. Links the destination already had are told apart by their targets, which no archive can hold.
- It leaves out other processes that change the destination during extraction.
- On Windows, run it on a machine with Developer Mode, or as administrator, before a release that changes how links are created there.

## Run it

```sh
ZIPPER_LINK_ESCAPE=1 ZIPPER_LINK_STRIDE=1 cargo test --release --test link_escape_exhaustive -- --nocapture
```

The same command runs it on Windows, where the full run takes several minutes.

Then run `cargo xtask verify-sync link-escape`, which runs it again and records the fingerprint.

## Proof that the check can fail

| Change in `src/staging.rs` | Result |
| --- | --- |
| `publish` skips `ensure_links_stay_in_archive_paths` | Fails: a link lands outside |
| `join_rest` always joins, adding a trailing slash that makes `lstat` follow the link, the bug the second M12 review found | Fails |
| The resolver stops at an archive link instead of following it, the bug of the first fix | Fails |
