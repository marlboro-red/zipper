---
name: zipper-verify-7z-workers
description: Model-check the 7z folder workers and decoder memory budget in src/native_7z.rs with TLA+. Use before committing any change to Memory, Reservation, decoder_memory, DECODER_MEMORY, the worker threads or the cancel checks in native_7z::read_archive, and when a 7z extraction hangs, uses too much memory, decodes a folder twice or publishes after a failure or cancel.
---

# Verify the 7z folder workers

The 7z reader decodes folders on several worker threads. Each worker takes the next folder from one atomic counter, reserves decoder memory from a shared 2 GiB budget, and may wait for other workers to release memory. A wrong step order hangs the extraction, passes the budget, decodes a folder twice, or publishes after a corrupt folder or a cancel. `verification/7z-folder-workers/` is a TLA+ model of the workers, and TLC checks every order of their steps within a bound.

## The rules

Each rule is an invariant in `FolderWorkers.tla`. TLC also checks that no state is stuck, which is the rule `NoDeadlock`.

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | Every run ends, including when one folder asks for more than the whole budget. | `zipper extract` hangs. |
| `MemoryBounded` | Decoders running at once never reserve more than the budget, and released memory returns to it. | Memory exhaustion on many large folders. |
| `DecodedAtMostOnce` | No folder decodes twice. | Two threads create the same files, and one fails. |
| `SuccessIsComplete` | A published extraction decoded every folder. | Files missing from the output. |
| `FailureIsReported` | A corrupt folder never ends in a publish. | A damaged archive looks good. |
| `CancelPublishesNothing` | A cancel set before the last check publishes nothing. | The destination changes after the user cancelled. |

## What the model covers

| Covered code | Model |
| --- | --- |
| `while let Some(&folder) = wanted.get(next.fetch_add(1, ..))` | `WorkerFetches` |
| `tracker.check()?` at the start of `decode` | `WorkerChecksCancel` |
| `Memory::reserve`, with `wanted.min(self.total)` and the `Condvar` wait | `WorkerReserves` |
| Decoding the folder, with `tracker.add_bytes` and `tracker.file_done` seeing the cancel flag | `WorkerDecodes` |
| `Drop for Reservation`, as `decode` returns | `WorkerReleases` |
| `failure.get_or_insert(error)` and `next.store(wanted.len(), ..)` | `WorkerStoresFailure` |
| The application's `cancel.store(true, ..)` | `ApplicationCancels` |
| `thread::scope` returning, the stored failure, and `tracker.check()?` before `staging.publish` | `CallerJoins` |

One step of `WorkerDecodes` stands for the whole folder, because no other worker waits on a folder's progress. Memory is in abstract units, and `Need` stands for `decoder_memory` of each folder, sorted largest first as `read_archive` sorts `wanted`.

The model leaves out file creation, symlink targets, progress reports, the memory a decoder takes beyond its reservation, and the work after publishing begins.

- The bound is 4 folders needing 6, 3, 2 and 1 units against a budget of 4, so the first asks for more than the whole budget. There are 3 workers, any folder can be corrupt, and a cancel can come at any moment.
- The model runs in a few seconds, with 53,095 distinct states.
- `verification/coverage.txt` lists the covered code under `[7z-folder-workers]`, with its fingerprint when this check last passed.

## When to run it

The gate's `verify` step fails when code listed under `[7z-folder-workers]` in `verification/coverage.txt` has changed since this check last passed. Run the check then, and also before changing that code on purpose.

## Run it

1. Copy `verification/7z-folder-workers/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCWorkers.tla` with `workers.cfg`, with its own output folder.
3. The report must list all six rules under "Rules that hold".

## Read a result

- A rule that breaks comes with its shortest path. Read each step as the code step in the coverage table. Then replay the path against the real code, as a test or a live run.
- If the path cannot happen in the code, the model is wrong. Find the code step it gets wrong, fix the model, and say so in the commit.
- If the path can happen, it is a bug. Report it with the path in the code's words. Do not weaken a rule or the model to make it pass.
- If the state count drops after a model edit that only adds detail, an action has stopped firing. A primed variable that is both set and listed as `UNCHANGED` in one action disables that action. The first version of `FailureIsReported` read the code's own failure slot, so it missed a dropped failure; it now reads `corrupted`, which only the model sets.

## Change the model with the code

When a change to covered code is intended, change the matching action in the same commit, so that each action still names one step of the code. Then run the model. After it passes and you have checked the model against the new code, run `cargo xtask verify-sync 7z-folder-workers` to record the new fingerprint.

The rules are proven able to fail. Each of these model changes, which match plausible regressions, breaks the named rule:

| Change | Rule that breaks |
| --- | --- |
| `WorkerReserves` asks for the full need, as if `reserve` lost `.min(self.total)` | `NoDeadlock` |
| `WorkerReleases` keeps the memory of a failed folder, as if the reservation outlived the error | `MemoryBounded`, `NoDeadlock` |
| `WorkerStoresFailure` drops the error | `SuccessIsComplete`, `FailureIsReported` |
| `CallerJoins` skips the cancel check before publishing | `CancelPublishesNothing` |
| `WorkerFetches` reads `next` without incrementing it | `DecodedAtMostOnce` |
