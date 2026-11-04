---------------------------- MODULE Segments ----------------------------
(***************************************************************************)
(* Parallel decoding of one solid 7z LZMA2 folder, split where the stream  *)
(* resets its dictionary. The folder's reader is the consumer: when it     *)
(* needs bytes it starts decoder threads for the next segments, at most    *)
(* `Window` at once and within `Budget` bytes of output held in memory,    *)
(* then takes each segment's output in order. A decoder thread decodes     *)
(* one segment into memory and sends it, or an error, on its own channel.  *)
(* The reader can be dropped at any time, after an error elsewhere or a    *)
(* cancel; a decoder that finishes then finds no receiver and exits.       *)
(* After an error or a drop, the reader stops its decoders and joins them, *)
(* so its caller returns only once no decoder thread is left.              *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Sizes,          \* segment -> output size, in budget units
    Window,         \* decoder threads at once
    Budget,         \* output bytes held in memory at once
    DecodeCanFail,  \* a segment can be corrupt
    ReaderCanDrop   \* the consumer can be dropped early

N == Len(Sizes)
Segments == 1..N

VARIABLES
    next,       \* next segment to start
    state,      \* segment -> "waiting", "decoding", "ready", "failed", "taken" or "abandoned"
    held,       \* output units started and not yet taken or freed
    emitted,    \* segments whose bytes the consumer has passed on, in order
    rpc,        \* consumer: "reading", "ok", "error", "dropped" or "returned"
    threads     \* decoder threads alive

vars == <<next, state, held, emitted, rpc, threads>>

Init ==
    /\ next = 1
    /\ state = [s \in Segments |-> "waiting"]
    /\ held = 0
    /\ emitted = <<>>
    /\ rpc = "reading"
    /\ threads = 0

Front == Len(emitted) + 1

\* Before reading, start the next segment while the window and the budget
\* allow.
ReaderStartsSegment ==
    /\ rpc = "reading"
    /\ next <= N
    /\ threads < Window
    /\ held + Sizes[next] <= Budget
    /\ state' = [state EXCEPT ![next] = "decoding"]
    /\ held' = held + Sizes[next]
    /\ threads' = threads + 1
    /\ next' = next + 1
    /\ UNCHANGED <<emitted, rpc>>

\* A decoder thread finishes its segment and sends the result, then exits.
DecoderFinishes(s) ==
    /\ state[s] = "decoding"
    /\ \/ state' = [state EXCEPT ![s] = "ready"]
       \/ DecodeCanFail /\ state' = [state EXCEPT ![s] = "failed"]
    /\ threads' = threads - 1
    /\ UNCHANGED <<next, held, emitted, rpc>>

\* `recv()` on the front segment's channel: its bytes go out in order and
\* its memory is freed; a failed segment becomes the reader's error.
ReaderTakesFront ==
    /\ rpc = "reading"
    /\ Front <= N
    /\ state[Front] \in {"ready", "failed"}
    /\ IF state[Front] = "ready"
       THEN /\ emitted' = Append(emitted, Front)
            /\ state' = [state EXCEPT ![Front] = "taken"]
            /\ held' = held - Sizes[Front]
            /\ rpc' = IF Front = N THEN "ok" ELSE "reading"
       ELSE /\ rpc' = "error"
            /\ UNCHANGED <<emitted, state, held>>
    /\ UNCHANGED <<next, threads>>

\* The folder worker stops reading and drops the reader.
ReaderDropped ==
    /\ ReaderCanDrop
    /\ rpc = "reading"
    /\ rpc' = "dropped"
    /\ UNCHANGED <<next, state, held, emitted, threads>>

\* Once the reader is gone, results it never took are freed.
ResultFreed(s) ==
    /\ rpc \in {"error", "dropped", "returned"}
    /\ state[s] \in {"ready", "failed"}
    /\ state' = [state EXCEPT ![s] = "abandoned"]
    /\ held' = held - Sizes[s]
    /\ UNCHANGED <<next, emitted, rpc, threads>>

\* Dropping the reader joins every decoder thread it started, and only
\* then does the folder worker return.
ReaderReturns ==
    /\ rpc \in {"error", "dropped"}
    /\ threads = 0
    /\ rpc' = "returned"
    /\ UNCHANGED <<next, state, held, emitted, threads>>

Settled ==
    /\ rpc \in {"ok", "returned"}
    /\ threads = 0
    /\ \A s \in Segments : state[s] \notin {"ready", "failed", "decoding"}
    /\ UNCHANGED vars

Next ==
    \/ ReaderStartsSegment \/ ReaderTakesFront \/ ReaderDropped \/ ReaderReturns
    \/ \E s \in Segments : DecoderFinishes(s) \/ ResultFreed(s)
    \/ Settled

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------

\* Bytes leave in segment order, with none skipped.
InOrder == \A i \in 1..Len(emitted) : emitted[i] = i

\* A reader that reports the end passed on every segment.
OkIsComplete == rpc = "ok" => Len(emitted) = N

\* A corrupt segment never ends in a complete read.
FailureIsReported == (rpc = "ok") => \A s \in Segments : state[s] /= "failed"

\* Output held in memory stays within the budget. The reader falls back to
\* sequential decoding when a segment is larger than the budget.
MemoryBounded == held <= Budget

\* At most `Window` decoder threads run at once.
ThreadsBounded == threads <= Window

\* The folder worker never returns while a decoder thread it started runs,
\* so no thread outlives the extraction and holds the archive open.
NoThreadOutlivesReader == rpc \in {"ok", "returned"} => threads = 0

\* When the reader is done or gone and everything settles, nothing is held.
NothingLeaks == (rpc /= "reading" /\ threads = 0
                 /\ \A s \in Segments : state[s] \notin {"ready", "failed", "decoding"})
                => held = 0
=============================================================================
