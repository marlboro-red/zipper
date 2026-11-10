---
name: zipper-verify-tar-pool
description: Model-check the tar writer pool in src/writer_pool.rs and src/native_tar.rs with TLA+. Use before committing any change to InFlight, Chunk, Writers, write_job, ClosesOnDrop, Extraction::write_file, or the thread scope in native_tar::read_archive, and when a tar extraction hangs, leaks memory or reports success after a failed write.
---

# Verify the tar writer pool

The tar reader decodes on one thread and writes files on several, through the pool in `src/writer_pool.rs`. The 7z reader shares that pool, and the `zipper-verify-7z-workers` skill checks its own hand-off to it. A wrong order of steps hangs an extraction, holds more than 16 MiB of file data, or reports success after a write failed. Tests hit few of the possible orders. `verification/tar-writer-pool/` is a TLA+ model of the pool, and TLC checks every order of its steps within a bound.

## The rules

Each rule is an invariant in `WriterPool.tla`. TLC also checks that no state is stuck, which is the rule `NoDeadlock`.

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | Every run ends, whether it succeeds, a write fails, or the input is corrupt. | `zipper extract` hangs forever. |
| `BudgetHolds` | Chunks in flight never pass the budget. | Memory grows with file size or writer count. |
| `NothingLeaks` | Every chunk is freed by the end. | Budget leaks until extraction stalls. |
| `SuccessIsComplete` | A run that reports success wrote every chunk of every file. | A truncated file is published. |
| `FailureIsReported` | A run in which a write failed never reports success. | A failed extraction looks good. |
| `RemoveAfterDrain` | A file that repeats an earlier name removes the earlier copy only when no writer holds a job. | The removal races a writer, and the wrong bytes survive. |
| `NoOverwrite` | No file gets more chunks than it has. | Corrupt output. |
| `AllocationsBounded` | No more 1 MiB buffers are ever allocated than the budget allows, because a dropped chunk is on the spare list before its bytes count as free. | Memory grows past the budget as buffers pile up on the spare list. |

## What the model covers

| Covered code | Model |
| --- | --- |
| `InFlight::take`, which waits while other chunks would pass `BYTES_IN_FLIGHT` | `DecoderTakesChunk` |
| `Drop for Chunk`: first `spare.push(data)`, then `*bytes -= length` | `ChunkGoesToSpare`, then `ChunkFreesBudget`, two steps other threads can run between; every drop site adds to `dropping` |
| The `spare.pop()` in `InFlight::take`, or a new `vec![0; length]` | the `spare` and `allocated` updates in `DecoderTakesChunk` |
| `Writers::send`, which counts, sends to a lane and calls `check` | `DecoderCountsJob`, `DecoderSendsJob` |
| `Writers::drain`, `Writers::check`, `Writers::stopped` | `DecoderDrains`, the `failure` resets |
| `Writers::close`, called by the `ClosesOnDrop` guard on every return from the scope; `Writers::new` and `writer_pool::queue`, which pick the lanes | `DecoderClosesLanes` |
| `Writers::work` and `write_job` | `WriterReceives`, `WriterWritesFirst`, `WriterWritesRest`, `WriterDropsJob` |
| `Extraction::write_file`, including the local `stream` sender that drops on `?` | `DecoderTakesChunk` through `DecoderFinishesFile`, `DecoderFindsCorruptInput` |
| The `thread::scope` block in `native_tar::read_archive` | `DecoderClosesLanes`, `DecoderJoins` |

One chunk in the model is one `Chunk` of up to `CHUNK` bytes, so the budget is `BYTES_IN_FLIGHT / CHUNK`. A write that fails and a writer that panics are the same step, because `catch_unwind` turns the panic into an error. A panic in `parse` takes the same path as corrupt input. The lane hash is any lane, so the Windows model covers every hash outcome.

The model leaves out file contents, the filesystem, progress reports and `ReadOptions` validation. It treats every chunk as a full 1 MiB buffer, so every drop returns its buffer to the spare list.

- The bound is 4 files of 2, 3, 1 and 2 chunks, the fourth repeating a name, 3 writers, a budget of 3 chunks, 2 jobs per lane queue and 2 chunks per stream. `MCUnix` has one shared lane, and `MCWindows` has one lane per writer.
- Each model runs in a few seconds, with 53,227 and 59,831 distinct states.
- `verification/coverage.txt` lists the covered code under `[tar-writer-pool]`, with its fingerprint when this check last passed.

## When to run it

The gate's `verify` step fails when code listed under `[tar-writer-pool]` in `verification/coverage.txt` has changed since this check last passed. Run the check then, and also before changing that code on purpose.

## Run it

1. Copy `verification/tar-writer-pool/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCUnix.tla` with `unix.cfg`, and again on `MCWindows.tla` with `windows.cfg`. Give each run its own output folder.
3. Both reports must list all seven rules under "Rules that hold".

## Read a result

- A rule that breaks comes with its shortest path. Read each step as the code step in the coverage table. Then replay the path against the real code, as a test or a live run.
- If the path cannot happen in the code, the model is wrong. Find the code step it gets wrong, fix the model, and say so in the commit. The first version of this model wrongly kept a file's stream open after `Writers::send` failed, and TLC reported a deadlock that the code cannot reach, because the local `stream` sender drops when `write_file` returns.
- If the path can happen, it is a bug. Report it with the path in the code's words. Do not weaken a rule or the model to make it pass.

## Change the model with the code

When a change to covered code is intended, change the matching action in the same commit, so that each action still names one indivisible step of the code. Then run both models. After both pass and you have checked the model against the new code, run `cargo xtask verify-sync tar-writer-pool`.

The rules are proven able to fail. Each of these model changes, which match plausible regressions, breaks the named rule:

| Change | Rule that breaks |
| --- | --- |
| `DecoderDrains` no longer waits for `pending = 0`, as if `write_file` skipped `writers.drain()` | `RemoveAfterDrain` |
| `DecoderTakesChunk` no longer waits on the budget, as if `InFlight::take` lost its loop | `BudgetHolds` |
| `WriterDropsJob` keeps `receiving` true, as if a failed writer kept its stream receiver | `NothingLeaks` |

The model also catches the four pool bugs that reviews found before `05233ac`, `93ac28a` and `10d858e` were committed. Each was back-tested by changing only the actions the old code did differently and running the unchanged rules:

| Old code | Rule that breaks | Path in the code's words |
| --- | --- | --- |
| No `InFlight` budget, only the queue sizes, which let 64 writers hold 389 MB | `BudgetHolds` | The decoder sends two files and takes a fourth chunk while three are queued. |
| `Writers::work` without `catch_unwind`, so a panicking writer never decremented `pending` | `NoDeadlock` | Writers panic mid-file, and the next file with a repeated name waits in `drain()` forever. |
| `parse` without `catch_unwind` in the scope, so a panicking progress callback skipped `writers.close()` | `NoDeadlock` | The decoder panics at its first step, and the writers wait on open lanes forever. |
| `Drop for Chunk` that freed the bytes before pushing the buffer to the spare list | `AllocationsBounded` | A writer drops a chunk, the budget frees first, and the decoder takes a chunk while the spare list is still empty, so it allocates a buffer past the budget. |
