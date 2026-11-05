---
name: zipper-verify-progress
description: Model-check progress reports from Rayon workers through Tracker in src/progress.rs with TLA+, with the ZIP decoder memory they hold. Use before committing any change to Tracker, Control, report_unless_busy, finish, EntryOnThisThread, the ZIP pass loop (run_pass, for_the_next_pass), reserve_within, DECODER_MEMORY_PATIENCE, how write_behind counts bytes, or where a reader calls them from a Rayon pool, and when a progress callback hangs an extraction or reports counts that shrink or stop short.
---

# Verify progress reports

ZIP extraction runs entry jobs on a Rayon pool, and each entry reports progress through `Tracker` to a callback the application wrote. When that callback runs Rayon work and waits for it, Rayon lets the waiting thread, or any thread that runs a piece of that work, start another queued entry job on its own stack. Three hangs came from this:

- At `10d858e` the second job waited for the report lock its own thread held.
- At `80af2b5` it waited in `Memory::reserve` for decoder memory that an entry lower on the same stack held. `a_callback_that_runs_rayon_work_never_waits_on_memory_its_own_thread_holds` in `tests/control.rs` reproduces this.
- Also at `80af2b5`, another thread running a piece of the callback's work started an entry and waited for memory that the callback's thread held.

Now no ZIP worker waits for decoder memory without a limit. `reserve_within` waits at most `DECODER_MEMORY_PATIENCE`, then takes the memory or records the entry in `for_the_next_pass`. `run` decodes the recorded entries in another pass. `EntryOnThisThread::enter` returns `None` for an entry that starts while its thread is already inside an entry of the same extraction, at any depth. That entry is recorded too, so a callback never decodes a whole entry of its own extraction. Write-behind writer threads only count bytes (`Tracker::count_bytes`) and never call the callback; `a_callback_that_runs_rayon_work_finishes_while_extractions_fill_the_pool` in `tests/global_pool_of_two.rs` covers that. `verification/progress-callback/` is a TLA+ model of the workers, the callback's Rayon work and the decoder memory, and TLC checks every order of the steps within a bound.

## The rules

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | No state is stuck, even when the callback runs Rayon work and entries need decoder memory. | `zipper::extract_archive` hangs inside the application's callback. |
| `EventuallyDone`, under weak fairness | The extraction ends; passes never repeat forever. | The extraction spins. |
| `MemoryBounded` | Reservations never exceed the decoder budget. | Memory grows past `ZIP_DECODER_MEMORY`. |
| `DecodedOnce` | Every entry decodes exactly once, so a recorded entry is never lost or doubled. | A file is missing or written twice. |
| `OneCallbackAtATime` | The callback never runs twice at once. | The application's callback must lock, and may race. |
| `CountsOnlyGrow` | Each report sees counts at least as large as the one before. | A progress bar moves backwards. |
| `FinalReportIsComplete` | The last report has every file. | A progress bar stops short of 100%. |

## What the model covers

| Covered code | Model |
| --- | --- |
| `run_pass`: `par_iter().try_for_each_init` in `native_zip::read_archive_with_options`, in any order | `ThreadStartsJob`, `JobReturns` |
| `EntryOnThisThread::enter` recording an entry that starts inside an entry on the same thread | `ThreadStartsJob` with `DeferNested` |
| `decoder_memory.reserve_within` at the start of an entry, waiting while other steps run and recording it when memory stays short, and the `Reservation` drop | `JobReserves`, `JobReturns` |
| `Tracker::add_bytes` and `file_done` while the reservation is held | `JobCounts` |
| `report_unless_busy`: `try_lock`, then the callback under the lock, or a skipped report | `JobReports`, `CallbackReturns` |
| The callback's Rayon work, run by any idle or waiting pool thread, which may start entries while it waits | `ThreadStartsWork`, `WorkReturns`, `ThreadStartsJob` |
| `run` starting another pass with the recorded entries | `CallerStartsNextPass` |
| `tracker.finish()` on the calling thread after the last pass | `CallerReportsFinal`, `CallerFinalReturns` |

Bytes and files count the same way, so the model counts once per entry. The model leaves out:

- Write-behind writer threads, which only count bytes and never call the callback. `tests/global_pool_of_two.rs` checks that instead.
- A second extraction started inside a callback. `an_extraction_started_inside_a_callback_on_the_same_thread_finishes` in `tests/control.rs` checks that case.
- A calling thread that is itself a Rayon worker, and cancel, errors and panics.
- Stack depth. Entries of different extractions can still nest on one thread, one level per extraction, which a 2 MiB thread holds about 30 deep. The tar decoder reports from one thread and the 7z workers are plain threads, so neither starts an entry inside a callback; this model is about the Rayon case.

- The bound is 4 entries on 2 pool threads that need 3, 3, 1 and 0 of a budget of 4, with a callback that queues Rayon work. It has 23,797 distinct states and runs in a few seconds.
- `verification/coverage.txt` lists the covered code under `[progress-callback]`, with its fingerprint when this check last passed.

## When to run it

The gate's `verify` step fails when code listed under `[progress-callback]` in `verification/coverage.txt` has changed since this check last passed. Run the check then, and also before changing that code on purpose.

## Run it

1. Copy `verification/progress-callback/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCCallback.tla` with `callback.cfg`, with its own output folder.
3. The report must list all seven rules under "Rules that hold".

## Read a result

- Read each step of a shortest path as the code step in the coverage table, and replay it against the real code.
- If the path cannot happen in the code, fix the model and say so in the commit. If it can, report it as a bug with the path in the code's words. Do not weaken a rule.

## Change the model with the code

Change the matching action in the same commit as an intended code change, run the model, and after you have checked the model against the new code, run `cargo xtask verify-sync progress-callback`.

Each change below breaks the named rule. The first two are the code as it was at `80af2b5` and the first fix a review broke:

| Change | Rule that breaks |
| --- | --- |
| `BlockingReserve = TRUE` and `DeferNested = FALSE` in `callback.cfg` | `NoDeadlock` |
| `BlockingReserve = TRUE` alone | `NoDeadlock` in 8 steps: thread 1 holds 3 in entry 1 and waits in its callback, and thread 2 runs that callback's work, starts entry 2 and waits for memory |
| `JobReserves` never takes memory | `EventuallyDone`: every pass records every entry |
| `CallerStartsNextPass` drops the recorded entries | `DecodedOnce`, `FinalReportIsComplete` |
| `CallerReportsFinal` ignores the recorded entries, as if `run` made one pass | `DecodedOnce`, `FinalReportIsComplete` |
| `CallerReportsFinal` reports nothing, as if `finish` were removed | `FinalReportIsComplete` |

`DeferNested = FALSE` alone passes. `reserve_within` already prevents the hang; the nesting check keeps a callback from decoding whole entries of its own extraction.
