# M28. Clean failure everywhere

Every failed or cancelled extraction leaves the destination as it was, with the error that names why, on every platform and in every output mode.

Cancel during publication rolls back on Linux and macOS since 12 November 2026. Its builder found three gaps outside that work, each older than it.

Scope:

- A failed `--flat` extraction with a read-only folder in the archive leaves `.tmp*/archive/...` entries in the output, or keeps a staging folder. `flatten` in `src/extract.rs` publishes nothing on failure and removes its staging folder in every case.
- The Windows CLI catches no signal, so Ctrl-C during publication ends the run with some files moved. It handles Ctrl-C and Ctrl-Break as the Unix CLI handles SIGINT and SIGTERM, by cancelling and rolling back.
- A cancel that arrives as a rename fails returns the rename's error, not `Cancelled`. Both roll back. The call returns `Cancelled` whenever the cancel flag was set before the rollback began.

Exit checks:

1. A test makes every folder of a `--flat` extraction read-only in turn and shows that the output folder and its parent hold nothing new after each failure.
2. A test on the Windows CI job sends Ctrl-C to the CLI during publication and shows the destination unchanged, and the Windows workflow passes.
3. A test sets the cancel flag during a failing rename and checks that the error is `Cancelled`.
4. A planted break with `scripts/plant.py` shows each new check can fail.

Done so far, by 14 November 2026:

- Since `7b55134`, the publish rename, every move of a merge and the flat-mode renames check the cancel flag after a failed rename and return `Cancelled`. On Windows they also stop waiting for a held file as soon as the cancel is set. Its first test, `a_cancel_stops_the_wait_for_a_held_folder_and_publishes_nothing` in `src/lib.rs`, runs only on Windows.
- Exit check 3 is met on Linux and macOS. `a_cancel_during_a_failing_publish_rename_returns_cancelled_and_publishes_nothing` in `src/lib.rs` makes the publish rename fail with a read-only parent and sets the cancel flag at the check that follows. `a_cancel_during_a_failing_flat_rename_returns_cancelled_and_moves_nothing` in `src/extract.rs` does the same for a flat-mode rename out of a read-only folder. Each also shows that without the cancel the rename's own error comes back, and that nothing reaches the destination. Both passed before any change, since `7b55134` had already added the checks.
- Exit check 1 is met. `a_read_only_folder_in_any_place_leaves_the_output_and_its_parent_as_they_were` in `tests/flat.rs` makes each of five archive folders read-only in turn, into a new and into an existing destination. After each failure it compares every path under the parent with the tree before. It failed first, with `.zipper-*/.tmp*/archive/...` left in the parent. `flatten`'s caller now unlocks the staging tree's folders and removes the tree whether the flattening failed or not, and publishes nothing when the removal fails.
- Exit check 4 is met for checks 1 and 3. With `scripts/plant.py`, each test fails when its check is broken. The planted breaks drop the cancel check after a failed publish rename, the cancel check after a failed flat rename, and the unlock before the staging tree is removed.
- The review of this work found that `--flat` lost a file without an error where the filesystem ignores case or Unicode normalization. Two names such as `X.txt` and `x.txt` became one file, even under the default that refuses shared names. `flatten` now treats a name that finds a file already moved as shared, and numbers a renamed file past any name the filesystem takes as taken. `names_that_differ_only_in_case_or_normalization_are_one_name_where_the_filesystem_ignores_it` in `tests/flat.rs` failed first, and planted breaks of both checks fail it.

## Closed on 14 November 2026

- Exit check 2 is met. Since `0253639`, the Windows CLI adds a console handler for Ctrl-C and Ctrl-Break while `extract` runs. It sets the same cancel flag that SIGINT sets on Unix. A cancelled run exits with `STATUS_CONTROL_C_EXIT`, the code the default handler gives, and only after it rolls back. Each test in `tests/console_cancel.rs` checks that exit code and compares every path under the destination's parent with the tree before.
- `ctrl_break_during_publication_into_a_folder_rolls_every_move_back` sends Ctrl-Break once the first of 20,000 files reaches an existing folder. `ctrl_break_during_extraction_into_a_new_folder_publishes_nothing` sends it once the staging folder appears. Windows cannot send Ctrl-C to one process group, so `ctrl_c_during_publication_into_a_folder_rolls_every_move_back` starts a helper in a console of its own. The helper ignores Ctrl-C itself and sends it to every process on that console.
- Both Ctrl-Break tests failed first on Windows, without the handler, at `35dc64d` in run 43498333686. The default handler ended the run with files moved or the staging folder left. The Windows check passed on `0253639` in run 43498338533 and on `e198ee2`, with the Ctrl-C test, in run 43499091745.
- Exit check 4 is met for check 2. Each plant below ran the Windows check on the planted tree, and the named tests failed there. Dropping the Ctrl-C arm of the handler failed the Ctrl-C test in run 43502099167. A handler that no longer sets the cancel flag failed all three tests in run 43504085503. A test that expects the destination to change failed all three tests in run 43506897592.
- The adversarial review of this work proved that the first tests sent only Ctrl-Break, so a handler without its Ctrl-C arm passed them. The Ctrl-C test closed that gap. It also found that closing the console window, logging off and shutting down still end a run partway, where the Unix build catches SIGHUP and SIGTERM and rolls back. These events lie outside this milestone's scope, which names Ctrl-C and Ctrl-Break, and [usage](../usage.md#extract-an-encrypted-zip) records that they are not caught.
