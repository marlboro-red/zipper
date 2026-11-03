---
name: zipper-verify-progress
description: Model-check progress reports from Rayon workers through Tracker in src/progress.rs with TLA+. Use before committing any change to Tracker, Control, report_unless_busy, finish, or where a reader calls them from a Rayon pool, and when a progress callback hangs an extraction or reports counts that shrink or stop short.
---

# Verify progress reports

ZIP extraction runs entry jobs on a Rayon pool, and each finished entry reports progress through `Tracker` to a callback the application wrote. When that callback runs Rayon work and waits for it, Rayon lets the waiting thread run another queued entry job on its own stack. That job then reports too, on a thread already inside a report. A blocking report lock at that point deadlocks, which happened at `10d858e`. `verification/progress-callback/` is a TLA+ model of this, and TLC checks every order of the steps within a bound.

## The rules

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | Every extraction ends, even when the callback runs Rayon work. | `zipper::extract_archive` hangs inside the application's callback. |
| `OneCallbackAtATime` | The callback never runs twice at once. | The application's callback must lock, and may race. |
| `CountsOnlyGrow` | Each report sees counts at least as large as the one before. | A progress bar moves backwards. |
| `FinalReportIsComplete` | The last report has every file. | A progress bar stops short of 100%. |

## What the model covers

| Code at the sync commit | Model |
| --- | --- |
| `par_iter().try_for_each_init` in `native_zip::read_archive_with_options`, run by Rayon threads | `ThreadStartsJob`, `JobReturns` |
| `Tracker::file_done` and `add_bytes`, the atomic `fetch_add` | `JobCountsFile` |
| `report_unless_busy`: `try_lock`, then the callback under the lock, or a skipped report | `JobReports`, `CallbackReturns` |
| Rayon running a queued job on a thread that waits inside the callback | `CallbackStealsJob` |
| `tracker.finish()` on the calling thread after the pool returns | `CallerSeesPoolFinish`, `CallerReportsFinal`, `CallerFinalReturns` |

Bytes and files count the same way, so the model counts files only. The tar decoder reports from one thread and the 7z workers are plain threads, so neither can run a job inside a callback; this model is about the Rayon case.

- The bound is 4 entries on 2 pool threads, with a callback that runs Rayon work. It has 927 distinct states and runs in about a second.
- The sync commit is `d4cf971`.

## When to run it

Run it when a diff touches `src/progress.rs`, or how a reader calls `Tracker` from a Rayon pool. To see what changed since the sync commit, run `git diff d4cf971 -- src/progress.rs src/native_zip.rs`.

## Run it

1. Copy `verification/progress-callback/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCCallback.tla` with `callback.cfg`, with its own output folder.
3. The report must list all four rules under "Rules that hold".

## Read a result

- Read each step of a shortest path as the code step in the coverage table, and replay it against the real code. `tests/control.rs` has `a_callback_that_runs_rayon_work_does_not_deadlock` for the deadlock.
- If the path cannot happen in the code, fix the model and say so in the commit. If it can, report it as a bug with the path in the code's words. Do not weaken a rule.

## Change the model with the code

Change the matching action in the same commit as an intended code change, run the model, and move the sync commit after you have checked the model against the new code.

Each change below breaks the named rule. The first is the code as it was at `10d858e`:

| Change | Rule that breaks |
| --- | --- |
| `JobReports` waits for the lock, as `Tracker::update` did with `lock()` at `10d858e` | `NoDeadlock`: a job takes the lock, its callback runs Rayon work, and a job started on the same thread waits for that lock forever |
| `JobReports` calls the callback without holding the lock | `OneCallbackAtATime` |
| `CallerReportsFinal` reports nothing, as if `finish` were removed | `FinalReportIsComplete` |
