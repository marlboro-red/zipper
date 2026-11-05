------------------------- MODULE ProgressCallback -------------------------
(***************************************************************************)
(* Progress reports from ZIP extraction in src/native_zip.rs through       *)
(* `Tracker` in src/progress.rs, with the decoder memory entries hold.     *)
(* Entry jobs run on a Rayon pool (`par_iter().try_for_each_init` in       *)
(* `run_pass`), in any order. An XZ, LZMA or PPMd entry takes decoder      *)
(* memory before it starts (`Memory::reserve_within`) and counts its written *)
(* bytes while it holds it. Each count calls the application's callback   *)
(* on the worker under the report lock if the lock is free                 *)
(* (`report_unless_busy`). A callback may queue Rayon work and wait for   *)
(* it. Any pool thread may run that work, and a thread that waits inside   *)
(* the callback or inside that work may run a pending entry job on its    *)
(* own stack.                                                              *)
(*                                                                         *)
(* An entry job that starts on a thread already inside an entry of this   *)
(* extraction (`EntryOnThisThread::enter` returns `None`) is recorded in  *)
(* `for_the_next_pass` and returns, and so is one whose memory is short.  *)
(* `run` decodes the recorded entries in another pass. After the last      *)
(* pass, the calling thread sends the final report (`Tracker::finish`).    *)
(*                                                                         *)
(* `BlockingReserve` is the code at `80af2b5`, where an entry waited for   *)
(* memory with `Memory::reserve`. `DeferNested` off lets a nested entry    *)
(* run on top of the one below it.                                         *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Need,                \* entry -> decoder memory it takes, 0 for none
    Total,               \* `ZIP_DECODER_MEMORY`
    Threads,             \* Rayon pool threads
    CallbackRunsRayon,   \* the callback queues Rayon work and waits for it
    DeferNested,         \* `EntryOnThisThread` records a nested entry
    BlockingReserve      \* an entry waits for memory instead of being recorded

N == Len(Need)
Entries == 1..N
T == 1..Threads
Caller == Threads + 1
CallbackFrame == 0
WorkFrame == N + 1

VARIABLES
    pending,      \* entries this pass has not started
    recorded,     \* `for_the_next_pass`
    stack,        \* thread -> frames, top last: an entry, `CallbackFrame` or `WorkFrame`
    jobStep,      \* thread -> per frame, the step of each entry frame
    available,    \* `Memory::available`
    held,         \* entry -> memory its reservation holds
    decoded,      \* entry -> times it decoded
    done,         \* the atomic count
    queued,       \* callback work queued and not yet started
    working,      \* callback work started and not yet returned
    lockHolder,   \* thread holding the report lock, 0 for none
    reported,     \* counts each callback saw, in order
    running,      \* callbacks running now
    mpc           \* calling thread: "waiting", "in_final" or "done"

vars == <<pending, recorded, stack, jobStep, available, held, decoded, done, queued,
          working, lockHolder, reported, running, mpc>>

Init ==
    /\ pending = Entries
    /\ recorded = {}
    /\ stack = [t \in T |-> <<>>]
    /\ jobStep = [t \in T |-> <<>>]
    /\ available = Total
    /\ held = [e \in Entries |-> 0]
    /\ decoded = [e \in Entries |-> 0]
    /\ done = 0
    /\ queued = 0
    /\ working = 0
    /\ lockHolder = 0
    /\ reported = <<>>
    /\ running = 0
    /\ mpc = "waiting"

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
OnEntry(t) == stack[t] /= <<>> /\ Top(t) \in Entries
Waiting(t) == stack[t] /= <<>> /\ Top(t) \in {CallbackFrame, WorkFrame}
InsideEntry(t) == \E i \in 1..Len(stack[t]) : stack[t][i] \in Entries
Min(a, b) == IF a < b THEN a ELSE b
Idle == \A t \in T : stack[t] = <<>>
Amount(e) == Min(Need[e], Total)

-----------------------------------------------------------------------------
(* Pool threads.                                                           *)

\* An idle thread, or one that waits in a callback or in callback work, runs
\* a pending entry job. A job that starts inside an entry of this extraction
\* is recorded and returns at once.
ThreadStartsJob(t) ==
    /\ stack[t] = <<>> \/ Waiting(t)
    /\ \E e \in pending :
         /\ pending' = pending \ {e}
         /\ IF DeferNested /\ InsideEntry(t)
            THEN /\ recorded' = recorded \cup {e}
                 /\ UNCHANGED <<stack, jobStep>>
            ELSE /\ Push(t, e, "reserve")
                 /\ UNCHANGED recorded
    /\ UNCHANGED <<available, held, decoded, done, queued, working, lockHolder,
                   reported, running, mpc>>

\* `decoder_memory.reserve_within(..)`: memory that stays short records the
\* entry for the next pass; waiting is the other steps running first. With `BlockingReserve` the job waits for memory instead.
JobReserves(t) ==
    /\ OnEntry(t) /\ TopStep(t) = "reserve"
    /\ LET e == Top(t) IN
         IF available >= Amount(e)
         THEN /\ available' = available - Amount(e)
              /\ held' = [held EXCEPT ![e] = Amount(e)]
              /\ SetTopStep(t, "count")
              /\ UNCHANGED recorded
         ELSE /\ ~BlockingReserve
              /\ recorded' = recorded \cup {e}
              /\ Pop(t)
              /\ UNCHANGED <<available, held>>
    /\ UNCHANGED <<pending, decoded, done, queued, working, lockHolder, reported,
                   running, mpc>>

\* Written bytes count while the reservation is held.
JobCounts(t) ==
    /\ OnEntry(t) /\ TopStep(t) = "count"
    /\ done' = done + 1
    /\ decoded' = [decoded EXCEPT ![Top(t)] = @ + 1]
    /\ SetTopStep(t, "report")
    /\ UNCHANGED <<pending, recorded, available, held, queued, working, lockHolder,
                   reported, running, mpc>>

\* `report_unless_busy`: a free lock runs the callback on this thread, a held
\* one skips the report. The callback may queue one piece of Rayon work.
JobReports(t) ==
    /\ OnEntry(t) /\ TopStep(t) = "report"
    /\ IF lockHolder = 0
       THEN /\ lockHolder' = t
            /\ reported' = Append(reported, done)
            /\ running' = running + 1
            /\ queued' = IF CallbackRunsRayon THEN 1 ELSE 0
            /\ jobStep' = [jobStep EXCEPT ![t] = Append([@ EXCEPT ![Len(@)] = "unlock"], "none")]
            /\ stack' = [stack EXCEPT ![t] = Append(@, CallbackFrame)]
       ELSE /\ SetTopStep(t, "release")
            /\ UNCHANGED <<lockHolder, reported, running, queued>>
    /\ UNCHANGED <<pending, recorded, available, held, decoded, done, working, mpc>>

\* Any idle or waiting thread starts the callback's queued work.
ThreadStartsWork(t) ==
    /\ queued > 0
    /\ stack[t] = <<>> \/ Waiting(t)
    /\ queued' = queued - 1
    /\ working' = working + 1
    /\ Push(t, WorkFrame, "none")
    /\ UNCHANGED <<pending, recorded, available, held, decoded, done, lockHolder,
                   reported, running, mpc>>

\* The work returns once nothing runs on top of it.
WorkReturns(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = WorkFrame
    /\ working' = working - 1
    /\ Pop(t)
    /\ UNCHANGED <<pending, recorded, available, held, decoded, done, queued, lockHolder,
                   reported, running, mpc>>

\* The callback returns once its work has returned, and the lock frees.
CallbackReturns(t) ==
    /\ stack[t] /= <<>> /\ Top(t) = CallbackFrame
    /\ queued = 0 /\ working = 0
    /\ running' = running - 1
    /\ stack' = [stack EXCEPT ![t] = SubSeq(@, 1, Len(@) - 1)]
    /\ jobStep' = [jobStep EXCEPT ![t] =
                      LET below == SubSeq(@, 1, Len(@) - 1) IN
                      [below EXCEPT ![Len(below)] = "release"]]
    /\ lockHolder' = 0
    /\ UNCHANGED <<pending, recorded, available, held, decoded, done, queued, working,
                   reported, mpc>>

\* The `Reservation` drops and the entry job returns to Rayon.
JobReturns(t) ==
    /\ OnEntry(t) /\ TopStep(t) = "release"
    /\ available' = available + held[Top(t)]
    /\ held' = [held EXCEPT ![Top(t)] = 0]
    /\ Pop(t)
    /\ UNCHANGED <<pending, recorded, decoded, done, queued, working, lockHolder,
                   reported, running, mpc>>

-----------------------------------------------------------------------------
(* Calling thread.                                                         *)

\* `run_pass` returns with entries recorded, so `run` starts another pass.
CallerStartsNextPass ==
    /\ mpc = "waiting"
    /\ pending = {} /\ Idle /\ recorded /= {}
    /\ pending' = recorded
    /\ recorded' = {}
    /\ UNCHANGED <<stack, jobStep, available, held, decoded, done, queued, working,
                   lockHolder, reported, running, mpc>>

\* The last pass returns; `tracker.finish()` locks and sends the final counts.
CallerReportsFinal ==
    /\ mpc = "waiting"
    /\ pending = {} /\ Idle /\ recorded = {}
    /\ lockHolder = 0
    /\ lockHolder' = Caller
    /\ reported' = Append(reported, done)
    /\ running' = running + 1
    /\ mpc' = "in_final"
    /\ UNCHANGED <<pending, recorded, stack, jobStep, available, held, decoded, done,
                   queued, working>>

CallerFinalReturns ==
    /\ mpc = "in_final"
    /\ lockHolder' = 0
    /\ running' = running - 1
    /\ mpc' = "done"
    /\ UNCHANGED <<pending, recorded, stack, jobStep, available, held, decoded, done,
                   queued, working, reported>>

Finished ==
    /\ mpc = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E t \in T : ThreadStartsJob(t) \/ JobReserves(t) \/ JobCounts(t)
                    \/ JobReports(t) \/ ThreadStartsWork(t) \/ WorkReturns(t)
                    \/ CallbackReturns(t) \/ JobReturns(t)
    \/ CallerStartsNextPass \/ CallerReportsFinal \/ CallerFinalReturns
    \/ Finished

Spec == Init /\ [][Next]_vars

\* Every step that can happen eventually happens, as threads keep running.
FairSpec == Spec /\ WF_vars(Next)

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

RECURSIVE HeldBy(_)
HeldBy(e) == IF e = 0 THEN 0 ELSE held[e] + HeldBy(e - 1)

\* Reservations never exceed the budget.
MemoryBounded == available + HeldBy(N) = Total

\* Every entry decodes once, with none lost when it is recorded.
DecodedOnce ==
    /\ \A e \in Entries : decoded[e] <= 1
    /\ mpc = "done" => \A e \in Entries : decoded[e] = 1

\* One callback runs at a time.
OneCallbackAtATime == running <= 1

\* Each callback sees counts at least as large as the one before.
CountsOnlyGrow ==
    \A i \in 1..Len(reported) : \A j \in i..Len(reported) : reported[i] <= reported[j]

\* The last report the application sees has every count.
FinalReportIsComplete == mpc = "done" => Len(reported) > 0 /\ reported[Len(reported)] = N

\* The extraction ends: passes never repeat forever.
EventuallyDone == <>(mpc = "done")
=============================================================================
