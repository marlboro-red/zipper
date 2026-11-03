-------------------------- MODULE FolderWorkers --------------------------
(***************************************************************************)
(* The folder workers of `native_7z::read_archive` in src/native_7z.rs.    *)
(* Scoped worker threads take folder positions from one atomic counter     *)
(* (`next.fetch_add`), check the cancel flag (`tracker.check`), reserve    *)
(* decoder memory from a shared budget that waits on a condition variable  *)
(* (`Memory::reserve`), decode the folder, and release the memory when the *)
(* `Reservation` drops. The first error goes into `failure` and           *)
(* `next.store(wanted.len())` stops new folders. The calling thread joins  *)
(* the workers, checks the cancel flag once more, and publishes.           *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Need,           \* folder position -> decoder memory it asks for (`decoder_memory`)
    Total,          \* the budget, `DECODER_MEMORY`
    Threads,        \* worker threads: `--threads`, at most the number of folders
    DecodeCanFail,  \* a folder can be corrupt: CRC, size or method error
    CanCancel       \* another thread can set the cancel flag

N == Len(Need)
Folders == 1..N
W == 1..Threads
Min(a, b) == IF a < b THEN a ELSE b

VARIABLES
    next,       \* the atomic folder counter
    wpc,        \* worker -> step
    folder,     \* worker -> folder position it holds, 0 for none
    reserved,   \* worker -> memory its `Reservation` holds
    available,  \* `Memory::available`
    errorOf,    \* worker -> "none", "data" or "cancel": the error `decode` returns
    failure,    \* the first error stored, or "none"
    decoded,    \* folder -> times it was decoded to the end
    cancel,     \* the cancel flag
    mpc,        \* calling thread step
    cancelAtPublish, \* the cancel flag when publishing began
    corrupted,  \* some folder turned out corrupt
    result      \* "none", "ok", "data" or "cancel"

vars == <<next, wpc, folder, reserved, available, errorOf, failure, decoded,
          cancel, mpc, cancelAtPublish, corrupted, result>>

Init ==
    /\ next = 0
    /\ wpc = [w \in W |-> "fetch"]
    /\ folder = [w \in W |-> 0]
    /\ reserved = [w \in W |-> 0]
    /\ available = Total
    /\ errorOf = [w \in W |-> "none"]
    /\ failure = "none"
    /\ decoded = [f \in Folders |-> 0]
    /\ cancel = FALSE
    /\ mpc = "joining"
    /\ cancelAtPublish = FALSE
    /\ corrupted = FALSE
    /\ result = "none"

-----------------------------------------------------------------------------
(* Workers.                                                                *)

\* `while let Some(&folder) = wanted.get(next.fetch_add(1, ..))`
WorkerFetches(w) ==
    /\ wpc[w] = "fetch"
    /\ next' = next + 1
    /\ IF next < N
       THEN /\ folder' = [folder EXCEPT ![w] = next + 1]
            /\ wpc' = [wpc EXCEPT ![w] = "check"]
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "exited"]
            /\ UNCHANGED folder
    /\ UNCHANGED <<reserved, available, errorOf, failure, decoded, cancel, mpc,
                   cancelAtPublish, corrupted, result>>

\* `tracker.check()?` at the start of `decode`.
WorkerChecksCancel(w) ==
    /\ wpc[w] = "check"
    /\ IF cancel
       THEN /\ errorOf' = [errorOf EXCEPT ![w] = "cancel"]
            /\ wpc' = [wpc EXCEPT ![w] = "store"]
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "reserve"]
            /\ UNCHANGED errorOf
    /\ UNCHANGED <<next, folder, reserved, available, failure, decoded, cancel, mpc,
                   cancelAtPublish, corrupted, result>>

\* `memory.reserve(decoder_memory(folder))` waits while `available` is short
\* of `min(wanted, total)`.
WorkerReserves(w) ==
    /\ wpc[w] = "reserve"
    /\ LET amount == Min(Need[folder[w]], Total) IN
         /\ available >= amount
         /\ available' = available - amount
         /\ reserved' = [reserved EXCEPT ![w] = amount]
    /\ wpc' = [wpc EXCEPT ![w] = "decode"]
    /\ UNCHANGED <<next, folder, errorOf, failure, decoded, cancel, mpc,
                   cancelAtPublish, corrupted, result>>

\* The folder decodes to its end, or fails on corrupt data, or a
\* `tracker.add_bytes` or `tracker.file_done` sees the cancel flag.
WorkerDecodes(w) ==
    /\ wpc[w] = "decode"
    /\ \/ /\ decoded' = [decoded EXCEPT ![folder[w]] = @ + 1]
          /\ UNCHANGED <<errorOf, corrupted>>
       \/ /\ DecodeCanFail
          /\ errorOf' = [errorOf EXCEPT ![w] = "data"]
          /\ corrupted' = TRUE
          /\ UNCHANGED decoded
       \/ /\ cancel
          /\ errorOf' = [errorOf EXCEPT ![w] = "cancel"]
          /\ UNCHANGED <<decoded, corrupted>>
    /\ wpc' = [wpc EXCEPT ![w] = "release"]
    /\ UNCHANGED <<next, folder, reserved, available, failure, cancel, mpc,
                   cancelAtPublish, result>>

\* `decode` returns, so `_reservation` drops: `available += amount` and
\* `notify_all`.
WorkerReleases(w) ==
    /\ wpc[w] = "release"
    /\ available' = available + reserved[w]
    /\ reserved' = [reserved EXCEPT ![w] = 0]
    /\ wpc' = [wpc EXCEPT ![w] = IF errorOf[w] = "none" THEN "fetch" ELSE "store"]
    /\ folder' = [folder EXCEPT ![w] = IF errorOf[w] = "none" THEN 0 ELSE @]
    /\ UNCHANGED <<next, errorOf, failure, decoded, cancel, mpc, cancelAtPublish, corrupted, result>>

\* `failure.get_or_insert(error); next.store(wanted.len(), ..)`
WorkerStoresFailure(w) ==
    /\ wpc[w] = "store"
    /\ failure' = IF failure = "none" THEN errorOf[w] ELSE failure
    /\ next' = N
    /\ errorOf' = [errorOf EXCEPT ![w] = "none"]
    /\ folder' = [folder EXCEPT ![w] = 0]
    /\ wpc' = [wpc EXCEPT ![w] = "fetch"]
    /\ UNCHANGED <<reserved, available, decoded, cancel, mpc, cancelAtPublish, corrupted, result>>

-----------------------------------------------------------------------------
(* Another thread, and the calling thread.                                 *)

\* `cancel.store(true, ..)` from the application, at any time.
ApplicationCancels ==
    /\ CanCancel
    /\ ~cancel
    /\ cancel' = TRUE
    /\ UNCHANGED <<next, wpc, folder, reserved, available, errorOf, failure, decoded,
                   mpc, cancelAtPublish, corrupted, result>>

\* `thread::scope` returns once every worker exits; a stored failure is
\* returned, otherwise `tracker.check()?` runs before `staging.publish`.
CallerJoins ==
    /\ mpc = "joining"
    /\ \A w \in W : wpc[w] = "exited"
    /\ IF failure /= "none"
       THEN /\ result' = failure /\ mpc' = "done" /\ UNCHANGED cancelAtPublish
       ELSE IF cancel
            THEN /\ result' = "cancel" /\ mpc' = "done" /\ UNCHANGED cancelAtPublish
            ELSE /\ result' = "ok" /\ mpc' = "done" /\ cancelAtPublish' = cancel
    /\ UNCHANGED <<next, wpc, folder, reserved, available, errorOf, failure, decoded, cancel, corrupted>>

Finished ==
    /\ mpc = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E w \in W : WorkerFetches(w) \/ WorkerChecksCancel(w) \/ WorkerReserves(w)
                    \/ WorkerDecodes(w) \/ WorkerReleases(w) \/ WorkerStoresFailure(w)
    \/ ApplicationCancels
    \/ CallerJoins
    \/ Finished

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

RECURSIVE ReservedBy(_)
ReservedBy(n) == IF n = 0 THEN 0 ELSE reserved[n] + ReservedBy(n - 1)
ReservedTotal == ReservedBy(Threads)

\* Decoders running at once never reserve more than the budget.
MemoryBounded == ReservedTotal <= Total /\ available + ReservedTotal = Total

\* No folder decodes twice.
DecodedAtMostOnce == \A f \in Folders : decoded[f] <= 1

\* A published extraction decoded every folder.
SuccessIsComplete == result = "ok" => \A f \in Folders : decoded[f] = 1

\* A corrupt folder never ends in a published extraction.
FailureIsReported == (mpc = "done" /\ corrupted) => result /= "ok"

\* Nothing is published once the cancel flag is set before the last check.
CancelPublishesNothing == result = "ok" => ~cancelAtPublish
=============================================================================
