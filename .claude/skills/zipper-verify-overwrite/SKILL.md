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
| A cancel changes nothing | A cancel seen at any check of publication, before a rename or after the folder modes, undoes every rename made, and the call returns `Cancelled`. | A half-merged folder after the user pressed cancel. |

## What it covers

- `overwrite_matches_the_contract_on_every_small_tree_pair` checks `Existing::Overwrite` against `documented_outcome`. `skip_and_rename_modes_match_their_contracts_on_every_small_tree_pair` checks `Skip`, `RenameNew` and `RenameExisting` against `documented_keep_or_rename`, where nothing is refused and a renamed entry gets the name `name_1`.
- The trees use the names `a` and `b` at two levels. A destination path holds nothing, a file, a folder, a symlink to a folder outside, or a symlink to a file outside, which gives 841 destinations. An archive path holds nothing, a file or a folder, which gives 121 archives. That makes 101,761 pairs.
- The code covered is `crate::staging::stage`, `Staging::publish`, `plan`, `same_filesystem` and `crate::publish` in src/lib.rs, through the native ZIP reader. tar, 7z and the 7-Zip backend call the same two functions.
- `documented_outcome` in the test is the contract written as code. It does not copy `plan`. It states what must be on disk afterwards, and the test compares the whole tree, outside folder included.
- It runs on Unix, where the test can make symlinks.
- `staging::tests::cancel` in `src/staging.rs` cancels at every check of publication. `a_cancel_at_any_check_of_publication_leaves_the_destination_as_it_was` uses one tree with a replaced file and link, a merged folder, new folders with modes and archive links, in every `Existing` mode. `a_cancel_at_any_check_restores_every_small_tree_pair` does the same on 130,321 pairs of trees with files, folders and links over `a` and `b` at two levels, in the four merging modes. Each compares the whole tree, modes included, with the tree before. It calls `Staging` directly, because no public hook can stop a real extraction between two renames. The gate runs every 1,009th pair, and the full check every pair.
- On a filesystem that ignores case, as APFS does on macOS by default, `overwrite_with_names_in_another_case_matches_the_contract` runs every pair again with the archive names in capitals. The outcome must be the same, compared without case. It is skipped where case matters, as on most Linux filesystems.
- `names_in_another_case_meet_the_same_checks_where_case_is_ignored` in `tests/overwrite.rs` covers ZIP and tar on Windows too, with a junction to a folder outside the destination.
- It leaves out other processes that change the destination during extraction. `plan` checks the tree before the first rename, so a folder swapped for a symlink in between is followed. The [usage page](../../../docs/usage.md) asks for a private destination for that reason. It also leaves out mount points inside the destination, which need root to set up, names that differ only by Unicode normalization, and rename failures such as a full disk.
- It runs only when invoked, never on a push or in CI. Run it before a release, through `cargo xtask release-checks`, and when the gate's `verify` step reports that its covered code changed. Without `ZIPPER_OVERWRITE_EXHAUSTIVE=1` each test prints that it was skipped. All 101,761 pairs take about 160 seconds per mode and spelling.
- `verification/coverage.txt` lists the covered code under `[overwrite]`, with its fingerprint when this check last passed.

## When to run it

The gate's `verify` step fails when code listed under `[overwrite]` in `verification/coverage.txt` has changed since this check last passed. Run the check then, and also before changing that code on purpose.

## Run it

```sh
ZIPPER_OVERWRITE_EXHAUSTIVE=1 ZIPPER_MERGE_STRIDE=1 cargo test --release --test overwrite_exhaustive -- --nocapture
```

It must pass and print the counts of merged and refused pairs.

## Read a result

- A failure prints the case number, both trees and the difference between the expected and the actual filesystem. Rebuild that pair by hand in a scratch folder and run `zipper extract --overwrite` on it to see the same result.
- If the contract in `documented_outcome` is wrong for that pair, the documentation is wrong too. Fix both together, and say why in the commit. Do not loosen `documented_outcome` to make a failure pass.
- If the code is wrong, it is a bug. Report it with the two trees and what changed on disk.

## Change the check with the code

When a change to the merge is intended, update `documented_outcome` and `docs/usage.md` in the same commit. After the full run passes, run `cargo xtask verify-sync overwrite` to record the new fingerprint.

The check is proven able to fail. Each of these changes to the real code fails it:

| Change in `src/staging.rs` | What the check reported, on every 37th pair |
| --- | --- |
| `plan` reads the destination with `fs::metadata`, which follows symlinks | Case 259. The destination's `b` is a symlink to a folder outside, and the extraction succeeds by writing `b/a` and `b/b` into that folder. |
| `plan` reads the destination with `fs::metadata`, with the archive name `LINKED` against a link named `linked` | `names_in_another_case_meet_the_same_checks_where_case_is_ignored` fails. |
| `plan` no longer refuses an archive folder over a file | Case 148. The OS refuses a rename with "Not a directory", after earlier renames changed the destination. |
| `plan` no longer refuses an archive file over a folder | Case 518. The OS refuses a rename with "Is a directory", after earlier renames changed the destination. |
| `carry_out` no longer checks the cancel flag before each rename | `staging::tests::cancel` fails. |
| `publish` returns a cancel without undoing the renames | `staging::tests::cancel` fails. |
| The cancel check after the folder modes runs after the created folders are unlocked | A read-only new folder cannot move back, and `staging::tests::cancel` fails. |
| `unlock_tree_for_cleanup` follows a created link | The undo changes the mode of a file the link names, and `staging::tests::cancel` fails. |
