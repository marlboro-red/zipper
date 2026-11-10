---
name: zipper-verify-7z-workers
description: Model-check the 7z folder workers, their decoder memory budget and their hand-off of small files to the writer pool in src/native_7z.rs with TLA+. Use before committing any change to Memory, Reservation, decoder_memory, DECODER_MEMORY, the worker threads, the cancel checks, FILE_WRITERS, file_writers or the writer pool calls in native_7z::read_archive, and when a 7z extraction hangs, uses too much memory, decodes a folder twice, loses a write failure or publishes after a failure or cancel.
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

## The LZMA2 segment pipeline

`verification/7z-lzma2-segments/` checks the reader that decodes one solid LZMA2 folder in segments, `ParallelLzma2`, listed under `[7z-lzma2-segments]` in `verification/coverage.txt`. `cargo xtask verify-sync 7z-lzma2-segments` runs three configurations through `scripts/check_model.py`:

- `MCSegments.tla` with `segments.cfg`, the main bound;
- `MCSegments.tla` with `undropped.cfg`, where no result is dropped early, so a deadlock can show;
- `MCHugeFront.tla` with `segments.cfg`, where the front segment's dictionary is larger than the budget.

All nine rules, `NoDeadlock` included, must hold in all three.

| Rule | In plain words |
| --- | --- |
| `InOrder` | Segment bytes leave in segment order, with none skipped. |
| `OkIsComplete` | A reader that reports the end passed on every segment. |
| `FailureIsReported` | A corrupt segment never ends in a complete read. |
| `MemoryBounded` | The most units threads ever held, plus the largest front dictionary, stay within `SEGMENT_BUDGET`, counted as if no freed memory ever left the process. |
| `OnlyTheFrontStreams` | Only the segment at the front of the output decodes on the folder's worker, straight into its buffer. |
| `ThreadsBounded` | At most `window` segment decoders run at once. |
| `NothingLeaks` | After an error or an early drop, every result is freed once the decoders finish. |
| `NoThreadOutlivesReader` | The folder worker returns only after every decoder thread it started has exited, so no thread holds the archive open after the extraction returns. |
| `NoDeadlock` | Some step is always possible until the reader ends. |

| Covered code | Model |
| --- | --- |
| `ParallelLzma2::start_segments`, with its window and budget checks, which leave room for the largest front dictionary | `ReaderStartsSegment` |
| `Front::Streaming` and `SegmentDecoder`, the front decoded on the worker | `ReaderStreamsFront` |
| The spawned thread running `decode_segment`, reading packed bytes from the file, and sending the result | `DecoderFinishes` |
| `Read for ParallelLzma2`, which takes the front segment and frees its share of the budget when it is used up | `ReaderTakesFront` |
| The folder worker dropping the reader, and a thread whose send finds no receiver | `ReaderDropped`, `ResultFreed` |
| `Drop for ParallelLzma2`, which sets the stop flag and joins every started thread, and `SegmentThread::finish`, which joins a thread after its result | `ReaderReturns` |

A thread the OS refuses only stops new starts, and the front still streams. After an error, every read that follows fails (`Front::Failed`). The model leaves out the chunk-header scan, which the hostile tests in `tests/hostile_7z.rs` cover, and the bytes themselves. `tests/sevenz_segment_memory.rs` measures the heap peak with a counting allocator, and `/usr/bin/time -l` the resident peak, because the allocator can keep freed memory resident.

Each change below breaks the named rule:

| Change | Rule that breaks |
| --- | --- |
| `ReaderStartsSegment` skips the budget check, leaves no room for the largest front, counts only fronts streamed so far, or charges the front nothing | `MemoryBounded` |
| The front streams only when its thread size fits | `NoDeadlock`, on `undropped.cfg` |
| A segment behind a queued thread streams | `OnlyTheFrontStreams` |
| The reader takes the segment after the front first | `InOrder` |
| `ReaderStartsSegment` skips the window check, or the window leaves out the streaming front | `ThreadsBounded` |
| `ReaderReturns` does not wait for the decoders | `NoThreadOutlivesReader` |
| A dropped streaming front is never freed | `NoDeadlock` |

## The writer pool hand-off

`verification/7z-writer-pool/` checks how folder workers hand small files to the writer threads of `src/writer_pool.rs`, the pool the tar reader also uses. It is listed under `[7z-writer-pool]` in `verification/coverage.txt`. `cargo xtask verify-sync 7z-writer-pool` runs `FolderWriters.tla` through `MCUnix.tla` with `unix.cfg`, one lane as on Unix, and `MCWindows.tla` with `windows.cfg`, one lane per writer. All eight rules, `NoDeadlock` included, must hold in both.

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | Every run ends, after a corrupt file, a failed write, a cancel or a panic too. | `zipper extract` hangs. |
| `BudgetHolds` | File data waiting for the writers never passes the budget, across all folder workers. | Memory grows with file count or worker count. |
| `NothingLeaks` | Every chunk is freed by the time the call returns. | The budget leaks until the next file waits forever. |
| `SuccessIsComplete` | A published extraction wrote every pooled file. | A file is missing from the output. |
| `WrittenAtMostOnce` | No file is written twice. | A second create fails or a file is truncated. |
| `FailureIsReported` | A corrupt file, a failed write or a panic never ends in a publish. | A damaged extraction looks good. |
| `CancelPublishesNothing` | A cancel set before the last check publishes nothing. | The destination changes after the user cancelled. |
| `NoThreadOutlivesTheCall` | The call returns only after every folder worker and writer has exited. | A thread holds a file open after the extraction returns. |

| Covered code | Model |
| --- | --- |
| `while let Some(&folder) = wanted.get(next.fetch_add(1, ..))` | `WorkerFetches` |
| `tracker.check()?` at the start of `decode` | `WorkerChecksCancel` |
| The next item of the folder, the check for data past the last file, and the folder CRC | `WorkerNextFile` |
| `writers.flight.take_whole_buffer(item.size)`, which waits while other chunks would pass `BYTES_IN_FLIGHT` | `WorkerTakesChunk` |
| Decoding the file into the chunk and checking its CRC | `WorkerFillsChunk` |
| `tracker.check()?` before `writers.send` | `WorkerChecksBeforeSend` |
| `Writers::send`, its `lane.send` and its `check` | `WorkerSendsJob` |
| `failure.get_or_insert(error)` and `next.store(wanted.len(), ..)` | `WorkerStoresFailure` |
| A panic on a folder worker, such as one from a progress callback | `WorkerPanics` |
| `Writers::work` and `write_job`, with `catch_unwind` around the write and the progress report | `WriterReceives`, `WriterWrites`, `WriterDropsJob` |
| The joins of the workers, `drop(closing)`, the joins of the writers, `resume_callback_panic`, the stored failure, `writers.check()?` and `tracker.check()?` before publishing | `CallerJoinsWorkers`, `CallerJoinsWriters` |

One file here is one chunk, because only a file of at most `CHUNK` bytes in a folder of more than one file goes to the pool. `InFlight::take_whole_buffer` charges each such file a whole `CHUNK` and reuses its buffer, so the budget is exactly `BYTES_IN_FLIGHT / CHUNK` files, and the spare list argument of the tar pool model, `AllocationsBounded`, holds for 7z too. `tests/sevenz_writer_memory.rs` checks with a counting allocator that a solid folder of 96 files of about 1 MiB allocates at most 20 such buffers. Larger files, the files of a one-file folder and every file at one thread take the path the folder model covers. A failed write, a panicking write and a panicking progress callback on a writer are one step, because `catch_unwind` turns each into a stored failure. The model leaves out decoder memory, which the folder model covers. A worker holds no chunk while it waits in `Memory::reserve`, and writers never wait on that memory, so the two waits form no cycle. It also leaves out the spare buffer list, file contents and the filesystem.

- The bound is 2 solid folders of 2 small files each, 2 folder workers, 2 writers, a budget of 2 chunks and 1 job per lane queue. Any file or folder can be corrupt, any write can fail, any worker can panic, and a cancel can come at any moment.
- TLC reaches 999,837 states in `MCUnix` and 1,334,981 in `MCWindows`, and both run within a minute.

`scripts/plant.py` applied each change below to `MCUnix`, and TLC reported the named rule broken:

| Change | Rule that breaks |
| --- | --- |
| `WorkerTakesChunk` no longer waits on the budget | `BudgetHolds` |
| `WorkerChecksBeforeSend` keeps the chunk on a cancel | `NothingLeaks` |
| `CallerJoinsWriters` skips `writers.check()` | `SuccessIsComplete` |
| `WriterReceives` leaves the job in the queue | `WrittenAtMostOnce` |
| `WorkerStoresFailure` drops the error | `SuccessIsComplete`, `FailureIsReported` |
| `CallerJoinsWriters` skips the cancel check | `CancelPublishesNothing` |
| `CallerJoinsWriters` does not wait for the writers | `NoThreadOutlivesTheCall`, `NothingLeaks` |
| `CallerJoinsWorkers` leaves the lanes open, as if `closing` dropped after the writers' joins | `NoDeadlock` |
