----------------------------- MODULE Blocks -----------------------------
(***************************************************************************)
(* Parallel decoding of the blocks of BZip2 streams. A block has no length *)
(* field, so the reader cuts the input at every block or end magic it      *)
(* finds, a "candidate". A candidate is a true block start or a magic that *)
(* lies inside a block's data. The reader starts a decoder thread for the  *)
(* bytes from its cursor to the next candidate, at most `Workers` at once  *)
(* and within `Budget`, and moves its cursor to that candidate before any  *)
(* thread has confirmed it. It takes results in order. A block that ends   *)
(* exactly at its cut is passed on. A block that runs out of bytes at its  *)
(* cut was cut at a false magic: the reader joins every thread behind it,  *)
(* drops their results, takes their input back and decodes the block       *)
(* itself to its true end, then goes on from there. A decoder started at a *)
(* false magic can report anything, a whole block included. The reader can *)
(* be dropped at any time; dropping it joins every thread it started.      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Cuts,           \* candidates in the input; the stream's true end follows the last
    Workers,        \* decoder threads at once
    Budget,         \* memory the queued blocks hold at once
    Size,           \* memory one queued block holds
    DecodeCanFail,  \* a block can be corrupt
    ReaderCanDrop   \* the consumer can be dropped early

Candidates == 1..Cuts
End == Cuts + 1

VARIABLES
    truth,      \* candidates that start a block; fixed at the start
    corrupt,    \* true blocks that are corrupt; fixed at the start
    cursor,     \* candidate where the next block to start begins
    queue,      \* started blocks in order, by their starting candidate
    result,     \* candidate -> "none", "decoding", "ready", "failed" or "truncated"
    held,       \* units the queued blocks hold
    emitted,    \* starting candidates of the blocks passed on, in order
    rpc,        \* consumer: "reading", "ok", "error", "dropped" or "returned"
    threads     \* decoder threads alive

vars == <<truth, corrupt, cursor, queue, result, held, emitted, rpc, threads>>

IsTrue(c) == c \in truth \/ c = End

\* Where the block that starts at `c` really ends.
TrueEnd(c) == CHOOSE t \in (c + 1)..End :
                  IsTrue(t) /\ \A u \in (c + 1)..(t - 1) : ~IsTrue(u)

Init ==
    /\ truth \in {S \in SUBSET Candidates : 1 \in S}
    /\ corrupt \in IF DecodeCanFail THEN SUBSET truth ELSE {{}}
    /\ cursor = 1
    /\ queue = <<>>
    /\ result = [c \in Candidates |-> "none"]
    /\ held = 0
    /\ emitted = <<>>
    /\ rpc = "reading"
    /\ threads = 0

\* `start_block`: cut at the next candidate and start a thread, while the
\* workers and the budget allow. One block may always start.
ReaderStartsBlock ==
    /\ rpc = "reading"
    /\ cursor <= Cuts
    /\ Len(queue) < Workers
    /\ queue = <<>> \/ held + Size <= Budget
    /\ queue' = Append(queue, cursor)
    /\ result' = [result EXCEPT ![cursor] = "decoding"]
    /\ held' = held + Size
    /\ threads' = threads + 1
    /\ cursor' = cursor + 1
    /\ UNCHANGED <<truth, corrupt, emitted, rpc>>

\* What `decode_block` can report for the bytes from `c` to the next candidate.
Outcomes(c) ==
    IF c \notin truth THEN {"ready", "failed", "truncated"}
    ELSE IF c \in corrupt
         THEN IF IsTrue(c + 1) THEN {"failed"} ELSE {"failed", "truncated"}
         ELSE IF IsTrue(c + 1) THEN {"ready"} ELSE {"truncated"}

\* A decoder thread finishes its block, sends the result and exits.
DecoderFinishes(c) ==
    /\ result[c] = "decoding"
    /\ \E outcome \in Outcomes(c) : result' = [result EXCEPT ![c] = outcome]
    /\ threads' = threads - 1
    /\ UNCHANGED <<truth, corrupt, cursor, queue, held, emitted, rpc>>

\* `advance`: the front block ended at its cut and goes out, or it failed.
ReaderTakesFront ==
    /\ rpc = "reading"
    /\ queue /= <<>>
    /\ LET front == Head(queue) IN
       /\ result[front] \in {"ready", "failed"}
       /\ IF result[front] = "ready"
          THEN /\ emitted' = Append(emitted, front)
               /\ queue' = Tail(queue)
               /\ held' = held - Size
               /\ rpc' = rpc
          ELSE /\ rpc' = "error"
               /\ UNCHANGED <<emitted, queue, held>>
    /\ UNCHANGED <<truth, corrupt, cursor, result, threads>>

\* `take_back_the_input` and `decode_past_the_false_cut`: the front ran out
\* of bytes at its cut. Every thread behind it is joined and its result
\* dropped, and the reader decodes the block to its true end.
ReaderDecodesPastAFalseCut ==
    /\ rpc = "reading"
    /\ queue /= <<>>
    /\ LET front == Head(queue) IN
       /\ result[front] = "truncated"
       /\ \A i \in 1..Len(queue) : result[queue[i]] /= "decoding"
       /\ queue' = <<>>
       /\ held' = 0
       /\ IF front \in corrupt \/ front \notin truth
          THEN /\ rpc' = "error"
               /\ UNCHANGED <<emitted, cursor>>
          ELSE /\ emitted' = Append(emitted, front)
               /\ cursor' = TrueEnd(front)
               /\ rpc' = rpc
    /\ UNCHANGED <<truth, corrupt, result, threads>>

\* `end_stream`: every started block is out and the cursor is at the end.
ReaderFinishes ==
    /\ rpc = "reading"
    /\ queue = <<>>
    /\ cursor = End
    /\ rpc' = "ok"
    /\ UNCHANGED <<truth, corrupt, cursor, queue, result, held, emitted, threads>>

\* The caller stops reading and drops the reader.
ReaderDropped ==
    /\ ReaderCanDrop
    /\ rpc = "reading"
    /\ rpc' = "dropped"
    /\ UNCHANGED <<truth, corrupt, cursor, queue, result, held, emitted, threads>>

\* `Drop for ParallelBzip2` joins every thread, and their results are freed.
ReaderReturns ==
    /\ rpc \in {"error", "dropped"}
    /\ threads = 0
    /\ rpc' = "returned"
    /\ queue' = <<>>
    /\ held' = 0
    /\ UNCHANGED <<truth, corrupt, cursor, result, emitted, threads>>

Settled ==
    /\ rpc \in {"ok", "returned"}
    /\ UNCHANGED vars

Next ==
    \/ ReaderStartsBlock \/ ReaderTakesFront \/ ReaderDecodesPastAFalseCut
    \/ ReaderFinishes \/ ReaderDropped \/ ReaderReturns
    \/ \E c \in Candidates : DecoderFinishes(c)
    \/ Settled

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------

Max(a, b) == IF a > b THEN a ELSE b

\* Only true blocks go out, each once, in stream order with none skipped.
InOrder ==
    \A i \in 1..Len(emitted) :
        /\ emitted[i] \in truth
        /\ emitted[i] = IF i = 1 THEN 1 ELSE TrueEnd(emitted[i - 1])

\* A reader that reports the end passed on every block.
OkIsComplete ==
    rpc = "ok" => {emitted[i] : i \in 1..Len(emitted)} = truth

\* A corrupt block never ends in a complete read.
FailureIsReported == rpc = "ok" => corrupt = {}

\* The front of the queue always starts a true block, so the result of a
\* decoder started at a false magic is never the one the reader acts on.
FrontIsTrue == (queue /= <<>> /\ rpc = "reading") => Head(queue) \in truth

\* The queued blocks stay within the budget, or are one block alone.
MemoryBounded ==
    /\ held = Size * Len(queue)
    /\ held <= Max(Budget, Size)

\* At most `Workers` decoder threads run at once.
ThreadsBounded == threads <= Workers

\* The reader's caller never returns while a decoder thread runs.
NoThreadOutlivesReader == rpc \in {"ok", "returned"} => threads = 0

\* Once the reader is done or gone, nothing is held.
NothingLeaks == rpc \in {"ok", "returned"} => (held = 0 /\ queue = <<>>)
=============================================================================
