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
- Exit check 4 is met for checks 1 and 3. With `scripts/plant.py`, each test fails when its check is broken. The planted breaks drop the cancel check after a failed publish rename, the cancel check after a failed flat rename, and the unlock before the staging tree is removed. Check 2 still needs the Windows work.
