------------------------- MODULE ProgressCallback -------------------------
(***************************************************************************)
(* Progress reports from ZIP extraction in src/native_zip.rs through       *)
(* `Tracker` in src/progress.rs. Entry jobs run on a Rayon pool             *)
(* (`par_iter().try_for_each_init`). Each finished entry adds to an atomic *)
(* count (`Tracker::file_done`) and then calls the application's callback *)
(* under the report lock if the lock is free (`report_unless_busy`). When  *)
(* the callback runs Rayon work and waits for it, Rayon lets the waiting   *)
(* thread run another queued job on its own stack, so an entry job can     *)
(* start inside a callback on the same thread. After the pool finishes,    *)
(* the calling thread sends the final report with a blocking lock          *)
(* (`Tracker::finish`).                                                     *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Entries,             \* ZIP entries with a payload
    Threads,             \* Rayon pool threads
    CallbackRunsRayon    \* the callback runs a `par_iter` and waits for it

T == 1..Threads

VARIABLES
    queue,        \* entry jobs not yet started
    stack,        \* thread -> frames, top last: "job" or "callback"
    jobStep,      \* thread -> per frame depth, step of each job frame
    done,         \* `files_done`, the atomic count
    lockHolder,   \* thread holding the report lock, 0 for none, Threads + 1 for the caller
    reported,     \* counts each callback saw, in order
    running,      \* callbacks running now
    mpc,          \* calling thread: "waiting", "final", "in_final" or "done"
    finished      \* entry jobs that returned

vars == <<queue, stack, jobStep, done, lockHolder, reported, running, mpc, finished>>

Caller == Threads + 1

Init ==
    /\ queue = Entries
    /\ stack = [t \in T |-> <<>>]
    /\ jobStep = [t \in T |-> <<>>]
    /\ done = 0
    /\ lockHolder = 0
    /\ reported = <<>>
    /\ running = 0
    /\ mpc = "waiting"
    /\ finished = 0

Top(t) == stack[t][Len(stack[t])]
TopStep(t) == jobStep[t][Len(jobStep[t])]
Push(t, frame, step) ==
    /\ stack' = [stack EXCEPT ![t] = Append(@, frame)]
    /\ jobStep' = [jobStep EXCEPT ![t] = Append(@, step)]
Pop(t) ==
    /\ stack' = [stack EXCEPT ![t] = SubSeq(@, 1, Len(@) - 1)]
    /\ jobStep' = [jobStep EXCEPT ![t] = SubSeq(@, 1, Len(@) - 1)]
SetTopStep(t, step) ==
    /\ jobStep' = [jobStep EXCEPT ![t] = [@ EXCEPT ![Len(@)] = step]]
    /\ UNCHANGED stack

-----------------------------------------------------------------------------
(* Pool threads.                                                           *)

\* An idle pool thread starts the next entry job.
ThreadStartsJob(t) ==
    /\ stack[t] = <<>>
    /\ queue > 0
    /\ queue' = queue - 1
    /\ Push(t, "job", "decode")
    /\ UNCHANGED <<done, lockHolder, reported, running, mpc, finished>>

\* The entry decodes and writes, then `files_done.fetch_add(1)`.
JobCountsFile(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = "job" /\ TopStep(t) = "decode"
    /\ done' = done + 1
    /\ SetTopStep(t, "report")
    /\ UNCHANGED <<queue, lockHolder, reported, running, mpc, finished>>

\* `report_unless_busy`: `reporting.try_lock()`. Free, it holds the lock and
\* calls the callback with a snapshot of the counts; held by anyone,
\* including this thread further down its stack, it skips the report.
JobReports(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = "job" /\ TopStep(t) = "report"
    /\ IF lockHolder = 0
       THEN /\ lockHolder' = t
            /\ reported' = Append(reported, done)
            /\ running' = running + 1
            /\ jobStep' = [jobStep EXCEPT ![t] = Append([@ EXCEPT ![Len(@)] = "unlock"], "none")]
            /\ stack' = [stack EXCEPT ![t] = Append(@, "callback")]
       ELSE /\ SetTopStep(t, "return")
            /\ UNCHANGED <<lockHolder, reported, running>>
    /\ UNCHANGED <<queue, done, mpc, finished>>

\* The callback returns; `_turn` drops and the lock frees.
CallbackReturns(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = "callback"
    /\ running' = running - 1
    /\ stack' = [stack EXCEPT ![t] = SubSeq(@, 1, Len(@) - 1)]
    /\ jobStep' = [jobStep EXCEPT ![t] =
                      LET below == SubSeq(@, 1, Len(@) - 1) IN
                      [below EXCEPT ![Len(below)] = "return"]]
    /\ lockHolder' = 0
    /\ UNCHANGED <<queue, done, reported, mpc, finished>>

\* While the callback waits for its own `par_iter`, Rayon runs another
\* queued job on this thread.
CallbackStealsJob(t) ==
    /\ CallbackRunsRayon
    /\ stack[t] /= <<>> /\ Top(t) = "callback"
    /\ queue > 0
    /\ queue' = queue - 1
    /\ Push(t, "job", "decode")
    /\ UNCHANGED <<done, lockHolder, reported, running, mpc, finished>>

\* The entry job returns to Rayon.
JobReturns(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = "job" /\ TopStep(t) = "return"
    /\ Pop(t)
    /\ finished' = finished + 1
    /\ UNCHANGED <<queue, done, lockHolder, reported, running, mpc>>

-----------------------------------------------------------------------------
(* Calling thread.                                                         *)

\* `par_iter` returns once every entry job has returned.
CallerSeesPoolFinish ==
    /\ mpc = "waiting"
    /\ finished = Entries
    /\ mpc' = "final"
    /\ UNCHANGED <<queue, stack, jobStep, done, lockHolder, reported, running, finished>>

\* `tracker.finish()`: `reporting.lock()`, then the callback with the final
\* counts. The calling thread is not a pool thread, so it steals nothing.
CallerReportsFinal ==
    /\ mpc = "final"
    /\ lockHolder = 0
    /\ lockHolder' = Caller
    /\ reported' = Append(reported, done)
    /\ running' = running + 1
    /\ mpc' = "in_final"
    /\ UNCHANGED <<queue, stack, jobStep, done, finished>>

CallerFinalReturns ==
    /\ mpc = "in_final"
    /\ lockHolder' = 0
    /\ running' = running - 1
    /\ mpc' = "done"
    /\ UNCHANGED <<queue, stack, jobStep, done, reported, finished>>

Finished ==
    /\ mpc = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E t \in T : ThreadStartsJob(t) \/ JobCountsFile(t) \/ JobReports(t)
                    \/ CallbackReturns(t) \/ CallbackStealsJob(t) \/ JobReturns(t)
    \/ CallerSeesPoolFinish \/ CallerReportsFinal \/ CallerFinalReturns
    \/ Finished

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

\* One callback runs at a time.
OneCallbackAtATime == running <= 1

\* Each callback sees counts at least as large as the one before.
CountsOnlyGrow ==
    \A i \in 1..Len(reported) : \A j \in i..Len(reported) : reported[i] <= reported[j]

\* The last report the application sees has every file.
FinalReportIsComplete == mpc = "done" => reported[Len(reported)] = Entries
=============================================================================
