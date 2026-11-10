--------------------------- MODULE FolderWriters ---------------------------
(***************************************************************************)
(* The hand-off from 7z folder workers to the shared writer pool, in       *)
(* `native_7z::read_archive` (src/native_7z.rs) and src/writer_pool.rs.    *)
(* Folder workers take folders from one atomic counter. For each small     *)
(* file of a solid folder a worker takes one `Chunk` from `InFlight`,      *)
(* decodes the file into it and checks its CRC, checks the cancel flag,    *)
(* and sends a `Job` to a lane (`Writers::send`). Writer threads           *)
(* (`Writers::work`, `write_job`) create and write each file and drop the  *)
(* chunk. The calling thread joins the workers, closes the lanes           *)
(* (`ClosesOnDrop`), joins the writers, and only then reports a result.    *)
(* One file here is one chunk, since only files of at most `CHUNK` bytes   *)
(* go to the pool. Each action is one step taken while holding no lock     *)
(* the other side waits on, or one blocking call.                          *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    FilesPerFolder, \* sequence: pooled files of each folder, in `wanted` order
    Threads,        \* folder workers
    Writers,        \* writer threads (`FILE_WRITERS`)
    Lanes,          \* 1 on Unix; Writers on Windows (`ONE_WRITER_PER_FOLDER`)
    Budget,         \* chunks allowed in flight (`BYTES_IN_FLIGHT` / `CHUNK`)
    LaneCap,        \* jobs a lane queue holds (`QUEUED_FILES`)
    DecodeCanFail,  \* a file or folder can be corrupt: CRC, size or method error
    WritesCanFail,  \* `write_job` can fail, or it or the progress callback can panic
    WorkersCanPanic,\* a folder worker can panic, as a progress callback on it can
    CanCancel       \* another thread can set the cancel flag

N == Len(FilesPerFolder)
Folders == 1..N
T == 1..Threads
W == 1..Writers
L == 1..Lanes
LaneOfWriter(w) == ((w - 1) % Lanes) + 1   \* `writer_pool::queue`
Files == UNION {{<<f, i>> : i \in 1..FilesPerFolder[f]} : f \in Folders}
FileOf(f, i) == <<f, i>>

VARIABLES
    next,         \* the atomic folder counter
    tpc,          \* worker -> step
    folder,       \* worker -> folder position it holds, 0 for none
    done,         \* worker -> files of its folder sent so far
    holding,      \* worker -> it holds a chunk it has not sent
    errorOf,      \* worker -> "none", "data", "cancel" or "write"
    failure,      \* the first error the workers stored, or "none"
    panicked,     \* some worker panicked
    inflight,     \* live chunks (`InFlight::bytes` in chunk units)
    laneQ,        \* lane -> queued jobs
    lanesOpen,    \* lane senders exist (`Writers::close` clears them)
    poolFailure,  \* `Writers::failure` holds an error
    wpc,          \* writer -> step
    job,          \* writer -> job it holds, or <<0, 0>>
    written,      \* file -> times it was written to the end
    corrupted,    \* some file or folder was corrupt
    writeFailed,  \* some write failed
    cancel,       \* the cancel flag
    mpc,          \* calling thread step
    cancelAtPublish,
    result        \* "none", "ok", "data", "cancel", "write" or "panic"

vars == <<next, tpc, folder, done, holding, errorOf, failure, panicked, inflight,
          laneQ, lanesOpen, poolFailure, wpc, job, written, corrupted, writeFailed,
          cancel, mpc, cancelAtPublish, result>>
workerVars == <<next, tpc, folder, done, holding, errorOf, failure, panicked>>
writerVars == <<wpc, job, written, writeFailed>>

Init ==
    /\ next = 0
    /\ tpc = [t \in T |-> "fetch"]
    /\ folder = [t \in T |-> 0]
    /\ done = [t \in T |-> 0]
    /\ holding = [t \in T |-> FALSE]
    /\ errorOf = [t \in T |-> "none"]
    /\ failure = "none"
    /\ panicked = FALSE
    /\ inflight = 0
    /\ laneQ = [l \in L |-> <<>>]
    /\ lanesOpen = TRUE
    /\ poolFailure = FALSE
    /\ wpc = [w \in W |-> "idle"]
    /\ job = [w \in W |-> <<0, 0>>]
    /\ written = [x \in Files |-> 0]
    /\ corrupted = FALSE
    /\ writeFailed = FALSE
    /\ cancel = FALSE
    /\ mpc = "joining_workers"
    /\ cancelAtPublish = FALSE
    /\ result = "none"

Released(t) == IF holding[t] THEN inflight - 1 ELSE inflight

-----------------------------------------------------------------------------
(* Folder workers: the `decode` closure.                                   *)

\* `while let Some(&folder) = wanted.get(next.fetch_add(1, ..))`
WorkerFetches(t) ==
    /\ tpc[t] = "fetch"
    /\ next' = next + 1
    /\ IF next < N
       THEN /\ folder' = [folder EXCEPT ![t] = next + 1]
            /\ done' = [done EXCEPT ![t] = 0]
            /\ tpc' = [tpc EXCEPT ![t] = "check"]
       ELSE /\ tpc' = [tpc EXCEPT ![t] = "exited"]
            /\ UNCHANGED <<folder, done>>
    /\ UNCHANGED <<holding, errorOf, failure, panicked, inflight, laneQ, lanesOpen,
                   poolFailure, corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* `tracker.check()?` at the start of `decode`.
WorkerChecksCancel(t) ==
    /\ tpc[t] = "check"
    /\ IF cancel
       THEN /\ errorOf' = [errorOf EXCEPT ![t] = "cancel"]
            /\ tpc' = [tpc EXCEPT ![t] = "store"]
       ELSE /\ tpc' = [tpc EXCEPT ![t] = "next_file"]
            /\ UNCHANGED errorOf
    /\ UNCHANGED <<next, folder, done, holding, failure, panicked, inflight, laneQ,
                   lanesOpen, poolFailure, corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* The next item of the folder, or the end checks: no data past the last
\* file, and the folder CRC.
WorkerNextFile(t) ==
    /\ tpc[t] = "next_file"
    /\ IF done[t] < FilesPerFolder[folder[t]]
       THEN /\ tpc' = [tpc EXCEPT ![t] = "take"]
            /\ UNCHANGED <<errorOf, corrupted>>
       ELSE \/ /\ tpc' = [tpc EXCEPT ![t] = "fetch"]
               /\ UNCHANGED <<errorOf, corrupted>>
            \/ /\ DecodeCanFail
               /\ errorOf' = [errorOf EXCEPT ![t] = "data"]
               /\ corrupted' = TRUE
               /\ tpc' = [tpc EXCEPT ![t] = "store"]
    /\ UNCHANGED <<next, folder, done, holding, failure, panicked, inflight, laneQ,
                   lanesOpen, poolFailure, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* `writers.flight.take(item.size)` blocks while other chunks would pass the
\* budget.
WorkerTakesChunk(t) ==
    /\ tpc[t] = "take"
    /\ inflight = 0 \/ inflight + 1 <= Budget
    /\ inflight' = inflight + 1
    /\ holding' = [holding EXCEPT ![t] = TRUE]
    /\ tpc' = [tpc EXCEPT ![t] = "fill"]
    /\ UNCHANGED <<next, folder, done, errorOf, failure, panicked, laneQ, lanesOpen,
                   poolFailure, corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* The file decodes into the chunk and its CRC is checked, or the data is
\* corrupt and `decode` returns, dropping the chunk.
WorkerFillsChunk(t) ==
    /\ tpc[t] = "fill"
    /\ \/ /\ tpc' = [tpc EXCEPT ![t] = "check_send"]
          /\ UNCHANGED <<errorOf, corrupted, inflight, holding>>
       \/ /\ DecodeCanFail
          /\ errorOf' = [errorOf EXCEPT ![t] = "data"]
          /\ corrupted' = TRUE
          /\ inflight' = inflight - 1
          /\ holding' = [holding EXCEPT ![t] = FALSE]
          /\ tpc' = [tpc EXCEPT ![t] = "store"]
    /\ UNCHANGED <<next, folder, done, failure, panicked, laneQ, lanesOpen,
                   poolFailure, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* `tracker.check()?` before the hand-off; on a cancel the chunk drops.
WorkerChecksBeforeSend(t) ==
    /\ tpc[t] = "check_send"
    /\ IF cancel
       THEN /\ errorOf' = [errorOf EXCEPT ![t] = "cancel"]
            /\ inflight' = inflight - 1
            /\ holding' = [holding EXCEPT ![t] = FALSE]
            /\ tpc' = [tpc EXCEPT ![t] = "store"]
       ELSE /\ tpc' = [tpc EXCEPT ![t] = "send"]
            /\ UNCHANGED <<errorOf, inflight, holding>>
    /\ UNCHANGED <<next, folder, done, failure, panicked, laneQ, lanesOpen,
                   poolFailure, corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* `Writers::send`: `lane.send(job)` blocks while the lane is full, and any
\* lane may be the hash; with the lanes closed the job drops. Then
\* `self.check()` takes a stored failure and returns it.
WorkerSendsJob(t) ==
    /\ tpc[t] = "send"
    /\ holding' = [holding EXCEPT ![t] = FALSE]
    /\ \/ /\ lanesOpen
          /\ \E l \in L :
               /\ Len(laneQ[l]) < LaneCap
               /\ laneQ' = [laneQ EXCEPT ![l] = Append(@, FileOf(folder[t], done[t] + 1))]
          /\ done' = [done EXCEPT ![t] = @ + 1]
          /\ UNCHANGED inflight
          /\ IF poolFailure
             THEN /\ poolFailure' = FALSE
                  /\ errorOf' = [errorOf EXCEPT ![t] = "write"]
                  /\ tpc' = [tpc EXCEPT ![t] = "store"]
             ELSE /\ tpc' = [tpc EXCEPT ![t] = "next_file"]
                  /\ UNCHANGED <<poolFailure, errorOf>>
       \/ /\ ~lanesOpen
          /\ inflight' = inflight - 1
          /\ errorOf' = [errorOf EXCEPT ![t] = "write"]
          /\ tpc' = [tpc EXCEPT ![t] = "store"]
          /\ UNCHANGED <<laneQ, done, poolFailure>>
    /\ UNCHANGED <<next, folder, failure, panicked, lanesOpen, corrupted, cancel, mpc,
                   cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* `failure.get_or_insert(error); next.store(wanted.len(), ..)`
WorkerStoresFailure(t) ==
    /\ tpc[t] = "store"
    /\ failure' = IF failure = "none" THEN errorOf[t] ELSE failure
    /\ next' = N
    /\ errorOf' = [errorOf EXCEPT ![t] = "none"]
    /\ tpc' = [tpc EXCEPT ![t] = "fetch"]
    /\ UNCHANGED <<folder, done, holding, panicked, inflight, laneQ, lanesOpen,
                   poolFailure, corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

\* A panic on the worker, such as one from a progress callback it calls,
\* ends its thread; the chunk it holds drops as it unwinds.
WorkerPanics(t) ==
    /\ WorkersCanPanic
    /\ tpc[t] \notin {"exited"}
    /\ tpc' = [tpc EXCEPT ![t] = "exited"]
    /\ panicked' = TRUE
    /\ inflight' = Released(t)
    /\ holding' = [holding EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<next, folder, done, errorOf, failure, laneQ, lanesOpen, poolFailure,
                   corrupted, cancel, mpc, cancelAtPublish, result>>
    /\ UNCHANGED writerVars

-----------------------------------------------------------------------------
(* Writers: `Writers::work` and `write_job`.                               *)

\* `jobs.lock().unwrap().recv()`: the next job, skipped while a failure is
\* stored, or exit once the lane is empty and closed.
WriterReceives(w) ==
    /\ wpc[w] = "idle"
    /\ LET l == LaneOfWriter(w) IN
         \/ /\ laneQ[l] /= <<>>
            /\ job' = [job EXCEPT ![w] = Head(laneQ[l])]
            /\ laneQ' = [laneQ EXCEPT ![l] = Tail(@)]
            /\ wpc' = [wpc EXCEPT ![w] = IF poolFailure THEN "drop" ELSE "write"]
         \/ /\ laneQ[l] = <<>>
            /\ ~lanesOpen
            /\ wpc' = [wpc EXCEPT ![w] = "exited"]
            /\ UNCHANGED <<job, laneQ>>
    /\ UNCHANGED <<written, writeFailed, inflight, lanesOpen, poolFailure>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED <<corrupted, cancel, mpc, cancelAtPublish, result>>

\* `write_job` creates and writes the file and reports it, or fails, or it
\* or the callback panics and `catch_unwind` stores the failure.
WriterWrites(w) ==
    /\ wpc[w] = "write"
    /\ \/ /\ written' = [written EXCEPT ![job[w]] = @ + 1]
          /\ UNCHANGED <<poolFailure, writeFailed>>
       \/ /\ WritesCanFail
          /\ poolFailure' = TRUE
          /\ writeFailed' = TRUE
          /\ UNCHANGED written
    /\ wpc' = [wpc EXCEPT ![w] = "drop"]
    /\ UNCHANGED <<job, inflight, laneQ, lanesOpen>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED <<corrupted, cancel, mpc, cancelAtPublish, result>>

\* `drop(job)`: `Drop for Chunk` frees its bytes and wakes `take`.
WriterDropsJob(w) ==
    /\ wpc[w] = "drop"
    /\ inflight' = inflight - 1
    /\ job' = [job EXCEPT ![w] = <<0, 0>>]
    /\ wpc' = [wpc EXCEPT ![w] = "idle"]
    /\ UNCHANGED <<written, writeFailed, laneQ, lanesOpen, poolFailure>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED <<corrupted, cancel, mpc, cancelAtPublish, result>>

-----------------------------------------------------------------------------
(* Another thread, and the calling thread.                                 *)

\* `cancel.store(true, ..)` from the application, at any time.
ApplicationCancels ==
    /\ CanCancel
    /\ ~cancel
    /\ cancel' = TRUE
    /\ UNCHANGED <<inflight, laneQ, lanesOpen, poolFailure, corrupted, mpc,
                   cancelAtPublish, result>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED writerVars

\* Every folder worker is joined; then `drop(closing)` closes the lanes.
CallerJoinsWorkers ==
    /\ mpc = "joining_workers"
    /\ \A t \in T : tpc[t] = "exited"
    /\ lanesOpen' = FALSE
    /\ mpc' = "joining_writers"
    /\ UNCHANGED <<inflight, laneQ, poolFailure, corrupted, cancel, cancelAtPublish, result>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED writerVars

\* Every writer is joined; then a panic resumes, a stored failure returns,
\* `writers.check()?` returns a write failure, and `tracker.check()?` runs
\* before `staging.publish`.
CallerJoinsWriters ==
    /\ mpc = "joining_writers"
    /\ \A w \in W : wpc[w] = "exited"
    /\ result' = IF panicked THEN "panic"
                  ELSE IF failure /= "none" THEN failure
                  ELSE IF poolFailure THEN "write"
                  ELSE IF cancel THEN "cancel"
                  ELSE "ok"
    /\ cancelAtPublish' = cancel
    /\ mpc' = "done"
    /\ UNCHANGED <<inflight, laneQ, lanesOpen, poolFailure, corrupted, cancel>>
    /\ UNCHANGED workerVars
    /\ UNCHANGED writerVars

Finished ==
    /\ mpc = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E t \in T : WorkerFetches(t) \/ WorkerChecksCancel(t) \/ WorkerNextFile(t)
                    \/ WorkerTakesChunk(t) \/ WorkerFillsChunk(t)
                    \/ WorkerChecksBeforeSend(t) \/ WorkerSendsJob(t)
                    \/ WorkerStoresFailure(t) \/ WorkerPanics(t)
    \/ \E w \in W : WriterReceives(w) \/ WriterWrites(w) \/ WriterDropsJob(w)
    \/ ApplicationCancels
    \/ CallerJoinsWorkers \/ CallerJoinsWriters
    \/ Finished

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

\* File data in flight never passes the budget (`InFlight::take`).
BudgetHolds == inflight <= Budget

\* Every chunk is freed by the time the call returns.
NothingLeaks == mpc = "done" => inflight = 0

\* A published extraction wrote every pooled file.
SuccessIsComplete == result = "ok" => \A x \in Files : written[x] = 1

\* No file is written twice.
WrittenAtMostOnce == \A x \in Files : written[x] <= 1

\* A corrupt file, a failed write or a panic never ends in a publish.
FailureIsReported ==
    (mpc = "done" /\ (corrupted \/ writeFailed \/ panicked)) => result /= "ok"

\* Nothing is published once the cancel flag is set before the last check.
CancelPublishesNothing == result = "ok" => ~cancelAtPublish

\* The call returns only after every worker and writer thread has exited.
NoThreadOutlivesTheCall ==
    mpc = "done" => /\ \A t \in T : tpc[t] = "exited"
                    /\ \A w \in W : wpc[w] = "exited"
=============================================================================
