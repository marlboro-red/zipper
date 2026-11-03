---
name: zipper-verify-overwrite
description: Check extraction into an existing folder (`Existing::Overwrite`, `--overwrite`) on every pair of small destination and archive trees. Use before committing any change to src/staging.rs, the publish path in src/lib.rs, or a reader's use of crate::staging, and when an overwrite writes outside the destination, changes files after a conflict, or loses files.
---

# Verify extraction into an existing folder

`--overwrite` merges an archive into a folder the user already has, which can hold anything, including symlinks that point elsewhere. A wrong check writes through such a link, changes files and then reports a conflict, or loses a file. `tests/overwrite_exhaustive.rs` runs the real ZIP reader on every pair of small trees and compares the whole filesystem after each run with the documented contract.

## The rules

The test checks all of them on every pair.

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| Nothing escapes | Files and folders outside the destination never change, even when the destination holds symlinks to them. | An archive overwrites files the user never chose. |
| A conflict changes nothing | When an archive folder meets a file or link, or an archive file meets a folder, extraction fails and every byte, link and folder stays as it was. | A half-merged folder after an error. |
| A merge is exact | Otherwise each archive file replaces the file or link at its path, archive folders exist, and everything else stays as it was. | Lost or stale files. |
| No staging is left | The hidden `.zipper-` folder is gone after success and after a conflict. | Clutter in the user's folder. |

## What it covers

- The trees use the names `a` and `b` at two levels. A destination path holds nothing, a file, a folder, a symlink to a folder outside, or a symlink to a file outside, which gives 841 destinations. An archive path holds nothing, a file or a folder, which gives 121 archives. That makes 101,761 pairs.
- The code covered is `crate::staging::stage`, `Staging::publish`, `plan`, `same_filesystem` and `crate::publish` in src/lib.rs, through the native ZIP reader. tar, 7z, `.zipper` and the 7-Zip backend call the same two functions.
- `documented_outcome` in the test is the contract written as code. It does not copy `plan`. It states what must be on disk afterwards, and the test compares the whole tree, outside folder included.
- It runs on Unix, where the test can make symlinks.
- It leaves out other processes that change the destination during extraction. `plan` checks the tree before the first rename, so a folder swapped for a symlink in between is followed. The [usage page](../../../docs/usage.md) asks for a private destination for that reason. It also leaves out mount points inside the destination, which need root to set up, case-insensitive name clashes, and rename failures such as a full disk.
- The gate runs every 37th pair, 2,751 pairs in about 4 seconds. All 101,761 pairs take about 160 seconds. At the last sync, 26,569 pairs merge and 75,192 are refused as conflicts.
- `verification/coverage.txt` lists the covered code under `[overwrite]`, with its fingerprint when this check last passed.

## When to run it

The gate's `verify` step fails when code listed under `[overwrite]` in `verification/coverage.txt` has changed since this check last passed. Run the check then, and also before changing that code on purpose.

## Run it

```sh
ZIPPER_MERGE_STRIDE=1 cargo test --release --test overwrite_exhaustive -- --nocapture
```

It must pass and print the counts of merged and refused pairs.

## Read a result

- A failure prints the case number, both trees and the difference between the expected and the actual filesystem. Rebuild that pair by hand in a scratch folder and run `zipper extract --overwrite` on it to see the same result.
- If the contract in `documented_outcome` is wrong for that pair, the documentation is wrong too. Fix both together, and say why in the commit. Do not loosen `documented_outcome` to make a failure pass.
- If the code is wrong, it is a bug. Report it with the two trees and what changed on disk.

## Change the check with the code

When a change to the merge is intended, update `documented_outcome` and `docs/usage.md` in the same commit. After the full run passes, run `cargo xtask verify-sync overwrite` to record the new fingerprint.

The check is proven able to fail. Each of these changes to the real code fails it:

| Change in `src/staging.rs` | What the check reported, on the gate's sample |
| --- | --- |
| `plan` reads the destination with `fs::metadata`, which follows symlinks | Case 259. The destination's `b` is a symlink to a folder outside, and the extraction succeeds by writing `b/a` and `b/b` into that folder. |
| `plan` no longer refuses an archive folder over a file | Case 148. The OS refuses a rename with "Not a directory", after earlier renames changed the destination. |
| `plan` no longer refuses an archive file over a folder | Case 518. The OS refuses a rename with "Is a directory", after earlier renames changed the destination. |
