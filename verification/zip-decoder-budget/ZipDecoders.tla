--------------------------- MODULE ZipDecoders ---------------------------
(***************************************************************************)
(* The ZIP entry workers of `native_zip::read_archive_with_options` in     *)
(* src/native_zip.rs and the shared decoder budget in src/memory.rs.       *)
(* Rayon workers take entries in order and stop taking new ones once any   *)
(* entry has failed (`try_for_each_init` returns the first error). An XZ,  *)
(* LZMA or PPMd entry takes memory from the shared `Memory` before it      *)
(* starts (`reserve_within`). When the budget is short it records the entry  *)
(* in `for_the_next_pass` and returns, and `run` runs the recorded entries *)
(* in another pass. It releases the memory when the `Reservation` drops. A large entry may hand its    *)
(* output through a bounded channel to a writer thread (`write_behind`), a *)
(* scoped OS thread outside the Rayon pool that writes until the channel   *)
(* closes and can fail on a write error.                                   *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Need,          \* entry -> memory it reserves, 0 for an entry that reserves none
    Behind,        \* entry -> TRUE when it writes through `write_behind`
    Total,         \* `ZIP_DECODER_MEMORY`
    Threads,       \* Rayon workers
    Chunks,        \* chunks a write-behind entry sends
    Depth,         \* `WRITE_BEHIND_DEPTH`, the channel's capacity
    DecodeCanFail,
    WriteCanFail

N == Len(Need)
Entries == 1..N
W == 1..Threads
Min(a, b) == IF a < b THEN a ELSE b

VARIABLES
    order,      \* entries this pass has not handed out, largest first
    recorded,   \* `for_the_next_pass`
    stop,       \* an entry failed, so no new entry starts
    wpc,        \* worker -> step
    entry,      \* worker -> entry it holds, 0 for none
    reserved,   \* worker -> memory its `Reservation` holds
    available,  \* `Memory::available`
    sent,       \* worker -> chunks sent to its writer
    queue,      \* worker -> chunks waiting in its channel
    writer,     \* worker -> "idle", "writing", "failed" or "done"
    failed,     \* worker -> its entry failed
    decoded,    \* entry -> times it decoded to the end
    corrupted,  \* some entry turned out corrupt or unwritable
    mpc,
    result

vars == <<order, recorded, stop, wpc, entry, reserved, available, sent, queue, writer,
          failed, decoded, corrupted, mpc, result>>

Init ==
    /\ order = [i \in 1..N |-> i]
    /\ recorded = {}
    /\ stop = FALSE
    /\ wpc = [w \in W |-> "take"]
    /\ entry = [w \in W |-> 0]
    /\ reserved = [w \in W |-> 0]
    /\ available = Total
    /\ sent = [w \in W |-> 0]
    /\ queue = [w \in W |-> 0]
    /\ writer = [w \in W |-> "idle"]
    /\ failed = [w \in W |-> FALSE]
    /\ decoded = [e \in Entries |-> 0]
    /\ corrupted = FALSE
    /\ mpc = "joining"
    /\ result = "none"

Unchanged(names) == UNCHANGED names

\* Rayon hands the next entry to an idle worker unless a failure stopped it.
Take(w) ==
    /\ wpc[w] = "take"
    /\ IF order /= <<>> /\ ~stop
       THEN /\ entry' = [entry EXCEPT ![w] = Head(order)]
            /\ order' = Tail(order)
            /\ wpc' = [wpc EXCEPT ![w] = IF Need[Head(order)] > 0 THEN "reserve" ELSE "start"]
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "exited"]
            /\ UNCHANGED <<entry, order>>
    /\ UNCHANGED <<recorded, stop, reserved, available, sent, queue, writer, failed,
                   decoded, corrupted, mpc, result>>

\* `decoder_memory.reserve_within(..)` takes the memory, or records the
\* entry for the next pass when `available` stays short. Waiting is the
\* other steps running first, so the patience needs no step of its own.
Reserve(w) ==
    /\ wpc[w] = "reserve"
    /\ LET amount == Min(Need[entry[w]], Total) IN
         IF available >= amount
         THEN /\ available' = available - amount
              /\ reserved' = [reserved EXCEPT ![w] = amount]
              /\ wpc' = [wpc EXCEPT ![w] = "start"]
              /\ UNCHANGED <<recorded, entry>>
         ELSE /\ recorded' = recorded \cup {entry[w]}
              /\ entry' = [entry EXCEPT ![w] = 0]
              /\ wpc' = [wpc EXCEPT ![w] = "take"]
              /\ UNCHANGED <<available, reserved>>
    /\ UNCHANGED <<order, stop, sent, queue, writer, failed, decoded, corrupted,
                   mpc, result>>

\* The entry decodes in place, or starts its writer thread.
Start(w) ==
    /\ wpc[w] = "start"
    /\ IF Behind[entry[w]]
       THEN /\ writer' = [writer EXCEPT ![w] = "writing"]
            /\ wpc' = [wpc EXCEPT ![w] = "send"]
            /\ UNCHANGED <<decoded, failed, corrupted>>
       ELSE \/ /\ decoded' = [decoded EXCEPT ![entry[w]] = @ + 1]
               /\ wpc' = [wpc EXCEPT ![w] = "release"]
               /\ UNCHANGED <<failed, corrupted, writer>>
            \/ /\ DecodeCanFail
               /\ failed' = [failed EXCEPT ![w] = TRUE]
               /\ corrupted' = TRUE
               /\ wpc' = [wpc EXCEPT ![w] = "release"]
               /\ UNCHANGED <<decoded, writer>>
    /\ UNCHANGED <<order, recorded, stop, entry, reserved, available, sent, queue, mpc, result>>

\* The decoder sends a chunk when the channel has room; a send to a writer
\* that failed returns an error.
Send(w) ==
    /\ wpc[w] = "send"
    /\ IF writer[w] = "failed"
       THEN /\ failed' = [failed EXCEPT ![w] = TRUE]
            /\ wpc' = [wpc EXCEPT ![w] = "close"]
            /\ UNCHANGED <<sent, queue, corrupted>>
       ELSE IF sent[w] = Chunks
       THEN /\ wpc' = [wpc EXCEPT ![w] = "close"]
            /\ UNCHANGED <<sent, queue, failed, corrupted>>
       ELSE \/ /\ queue[w] < Depth
               /\ queue' = [queue EXCEPT ![w] = @ + 1]
               /\ sent' = [sent EXCEPT ![w] = @ + 1]
               /\ UNCHANGED <<wpc, failed, corrupted>>
            \/ /\ DecodeCanFail
               /\ failed' = [failed EXCEPT ![w] = TRUE]
               /\ corrupted' = TRUE
               /\ wpc' = [wpc EXCEPT ![w] = "close"]
               /\ UNCHANGED <<sent, queue>>
    /\ UNCHANGED <<order, recorded, stop, entry, reserved, available, writer, decoded, mpc, result>>

\* The writer thread takes chunks until the channel closes and is empty.
WriterWrites(w) ==
    /\ writer[w] = "writing"
    /\ \/ /\ queue[w] > 0
          /\ queue' = [queue EXCEPT ![w] = @ - 1]
          /\ UNCHANGED <<writer, corrupted>>
       \/ /\ queue[w] > 0
          /\ WriteCanFail
          /\ writer' = [writer EXCEPT ![w] = "failed"]
          /\ queue' = [queue EXCEPT ![w] = 0]
          /\ corrupted' = TRUE
       \/ /\ queue[w] = 0
          /\ wpc[w] = "join"
          /\ writer' = [writer EXCEPT ![w] = "done"]
          /\ UNCHANGED <<queue, corrupted>>
    /\ UNCHANGED <<order, recorded, stop, wpc, entry, reserved, available, sent, failed, decoded,
                   mpc, result>>

\* The decoder drops its sender, then joins the writer thread.
Close(w) ==
    /\ wpc[w] = "close"
    /\ wpc' = [wpc EXCEPT ![w] = "join"]
    /\ UNCHANGED <<order, recorded, stop, entry, reserved, available, sent, queue, writer, failed,
                   decoded, corrupted, mpc, result>>

Join(w) ==
    /\ wpc[w] = "join"
    /\ writer[w] \in {"done", "failed"}
    /\ IF writer[w] = "failed" \/ failed[w]
       THEN /\ failed' = [failed EXCEPT ![w] = TRUE]
            /\ UNCHANGED decoded
       ELSE /\ decoded' = [decoded EXCEPT ![entry[w]] = @ + 1]
            /\ UNCHANGED failed
    /\ writer' = [writer EXCEPT ![w] = "idle"]
    /\ sent' = [sent EXCEPT ![w] = 0]
    /\ wpc' = [wpc EXCEPT ![w] = "release"]
    /\ UNCHANGED <<order, recorded, stop, entry, reserved, available, queue, corrupted, mpc, result>>

\* The `Reservation` drops; a failure stops Rayon from starting new entries.
Release(w) ==
    /\ wpc[w] = "release"
    /\ available' = available + reserved[w]
    /\ reserved' = [reserved EXCEPT ![w] = 0]
    /\ stop' = (stop \/ failed[w])
    /\ entry' = [entry EXCEPT ![w] = 0]
    /\ wpc' = [wpc EXCEPT ![w] = IF failed[w] THEN "exited" ELSE "take"]
    /\ UNCHANGED <<order, recorded, sent, queue, writer, failed, decoded, corrupted, mpc, result>>

\* Every order of a set of entries.
Orders(set) == {seq \in [1..Cardinality(set) -> set] : \A i, j \in DOMAIN seq : seq[i] = seq[j] => i = j}

\* `run_pass` returns with entries recorded and no failure, so `run`
\* starts another pass.
CallerStartsNextPass ==
    /\ mpc = "joining"
    /\ \A w \in W : wpc[w] = "exited"
    /\ ~\E w \in W : failed[w]
    /\ recorded /= {}
    /\ order' \in Orders(recorded)
    /\ recorded' = {}
    /\ wpc' = [w \in W |-> "take"]
    /\ UNCHANGED <<stop, entry, reserved, available, sent, queue, writer, failed,
                   decoded, corrupted, mpc, result>>

CallerFinishes ==
    /\ mpc = "joining"
    /\ \A w \in W : wpc[w] = "exited"
    /\ (\E w \in W : failed[w]) \/ recorded = {}
    /\ result' = IF \E w \in W : failed[w] THEN "failed" ELSE "ok"
    /\ mpc' = "done"
    /\ UNCHANGED <<order, recorded, order, recorded, stop, wpc, entry, reserved, available, sent, queue, writer,
                   failed, decoded, corrupted>>

Finished == mpc = "done" /\ UNCHANGED vars

Next ==
    \/ \E w \in W : Take(w) \/ Reserve(w) \/ Start(w) \/ Send(w) \/ WriterWrites(w)
                    \/ Close(w) \/ Join(w) \/ Release(w)
    \/ CallerStartsNextPass \/ CallerFinishes
    \/ Finished

Spec == Init /\ [][Next]_vars

\* Every step that can happen eventually happens, as threads keep running.
FairSpec == Spec /\ WF_vars(Next)

RECURSIVE ReservedBy(_)
ReservedBy(n) == IF n = 0 THEN 0 ELSE reserved[n] + ReservedBy(n - 1)

\* All decoders together never hold more than the budget.
MemoryBounded == ReservedBy(Threads) <= Total /\ available + ReservedBy(Threads) = Total

\* Every reservation is back when the extraction ends.
AllReleased == mpc = "done" => available = Total

\* No entry decodes twice.
DecodedAtMostOnce == \A e \in Entries : decoded[e] <= 1

\* A published extraction decoded every entry.
SuccessIsComplete == result = "ok" => \A e \in Entries : decoded[e] = 1

\* A corrupt or unwritable entry never ends in a published extraction.
FailureIsReported == (mpc = "done" /\ corrupted) => result /= "ok"

\* The extraction ends: passes never repeat forever.
EventuallyDone == <>(mpc = "done")
=============================================================================
