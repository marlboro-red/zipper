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
