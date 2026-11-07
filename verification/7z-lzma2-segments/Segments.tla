---------------------------- MODULE Segments ----------------------------
(***************************************************************************)
(* Parallel decoding of one solid 7z LZMA2 folder, split where the stream  *)
(* resets its dictionary. The folder's reader is the consumer. When its    *)
(* front segment is used up and no decoder thread is queued, the reader    *)
(* decodes the next segment itself, straight into the caller's buffer, and *)
(* holds only that segment's dictionary. Behind the front it starts        *)
(* decoder threads, at most `Window` decoders at once with its own, and    *)
(* within `Budget`. A thread holds its segment's whole output and          *)
(* dictionary until the reader takes it, in order, from the thread's own   *)
(* channel. A segment too large for a thread waits until it is the front,  *)
(* then streams. The reader can be dropped at any time, after an error     *)
(* elsewhere or a cancel; a decoder that finishes then finds no receiver   *)
(* and exits. After an error or a drop, the reader stops its decoders and  *)
(* joins them, so its caller returns only once no decoder thread is left.  *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Sizes,          \* segment -> output and dictionary, in budget units
    Streamed,       \* segment -> dictionary alone, in budget units
    Window,         \* decoders at once, the reader's own included
    Budget,         \* memory the started segments hold at once
    DecodeCanFail,  \* a segment can be corrupt
    ReaderCanDrop   \* the consumer can be dropped early

N == Len(Sizes)
Segments == 1..N

ASSUME \A s \in Segments : Streamed[s] <= Budget

VARIABLES
    next,       \* next segment to start
    state,      \* segment -> "waiting", "streaming", "decoding", "ready", "failed", "taken" or "abandoned"
    charge,     \* segment -> units it holds once started
    held,       \* units started and not yet taken or freed
    emitted,    \* segments whose bytes the consumer has passed on, in order
    rpc,        \* consumer: "reading", "ok", "error", "dropped" or "returned"
    threads     \* decoder threads alive

vars == <<next, state, charge, held, emitted, rpc, threads>>

Init ==
    /\ next = 1
    /\ state = [s \in Segments |-> "waiting"]
    /\ charge = [s \in Segments |-> 0]
    /\ held = 0
    /\ emitted = <<>>
    /\ rpc = "reading"
    /\ threads = 0

Front == Len(emitted) + 1

Streaming == IF Front <= N /\ state[Front] = "streaming" THEN 1 ELSE 0

\* `stream_front`: no thread is queued, so the next segment is the front,
\* and the reader decodes it itself.
ReaderStreamsFront ==
    /\ rpc = "reading"
    /\ next <= N
    /\ next = Front
    /\ state' = [state EXCEPT ![next] = "streaming"]
    /\ charge' = [charge EXCEPT ![next] = Streamed[next]]
    /\ held' = held + Streamed[next]
    /\ next' = next + 1
    /\ UNCHANGED <<emitted, rpc, threads>>

\* `start_segments`: behind the front, start a thread for the next segment
\* while the window and the budget allow.
ReaderStartsSegment ==
    /\ rpc = "reading"
    /\ next <= N
    /\ next > Front
    /\ threads + Streaming < Window
    /\ held + Sizes[next] <= Budget
    /\ state' = [state EXCEPT ![next] = "decoding"]
    /\ charge' = [charge EXCEPT ![next] = Sizes[next]]
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
    /\ UNCHANGED <<next, charge, held, emitted, rpc>>

\* `SegmentDecoder::read` on the streaming front, to its end: its bytes go
\* out in order and its dictionary is freed, or the decode fails.
ReaderDecodesFront ==
    /\ rpc = "reading"
    /\ Front <= N
    /\ state[Front] = "streaming"
    /\ \/ /\ emitted' = Append(emitted, Front)
          /\ state' = [state EXCEPT ![Front] = "taken"]
          /\ held' = held - charge[Front]
          /\ rpc' = IF Front = N THEN "ok" ELSE "reading"
       \/ /\ DecodeCanFail
          /\ state' = [state EXCEPT ![Front] = "failed"]
          /\ rpc' = "error"
          /\ UNCHANGED <<emitted, held>>
    /\ UNCHANGED <<next, charge, threads>>

\* `recv()` on the front thread's channel: its bytes go out in order and
\* its memory is freed; a failed segment becomes the reader's error.
ReaderTakesFront ==
    /\ rpc = "reading"
    /\ Front <= N
    /\ state[Front] \in {"ready", "failed"}
    /\ IF state[Front] = "ready"
       THEN /\ emitted' = Append(emitted, Front)
            /\ state' = [state EXCEPT ![Front] = "taken"]
            /\ held' = held - charge[Front]
            /\ rpc' = IF Front = N THEN "ok" ELSE "reading"
       ELSE /\ rpc' = "error"
            /\ UNCHANGED <<emitted, state, held>>
    /\ UNCHANGED <<next, charge, threads>>

\* The folder worker stops reading and drops the reader.
ReaderDropped ==
    /\ ReaderCanDrop
    /\ rpc = "reading"
    /\ rpc' = "dropped"
    /\ UNCHANGED <<next, state, charge, held, emitted, threads>>

\* Once the reader is gone, results it never took and the front it was
\* streaming are freed.
ResultFreed(s) ==
    /\ rpc \in {"error", "dropped", "returned"}
    /\ state[s] \in {"ready", "failed", "streaming"}
    /\ state' = [state EXCEPT ![s] = "abandoned"]
    /\ held' = held - charge[s]
    /\ UNCHANGED <<next, charge, emitted, rpc, threads>>

\* Dropping the reader joins every decoder thread it started, and only
\* then does the folder worker return.
ReaderReturns ==
    /\ rpc \in {"error", "dropped"}
    /\ threads = 0
    /\ rpc' = "returned"
    /\ UNCHANGED <<next, state, charge, held, emitted, threads>>

Holding == {"streaming", "decoding", "ready", "failed"}

Settled ==
    /\ rpc \in {"ok", "returned"}
    /\ threads = 0
    /\ \A s \in Segments : state[s] \notin Holding
    /\ UNCHANGED vars

Next ==
    \/ ReaderStreamsFront \/ ReaderStartsSegment \/ ReaderDecodesFront
    \/ ReaderTakesFront \/ ReaderDropped \/ ReaderReturns
    \/ \E s \in Segments : DecoderFinishes(s) \/ ResultFreed(s)
    \/ Settled

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------

RECURSIVE SumOf(_)
SumOf(S) == IF S = {} THEN 0
            ELSE LET s == CHOOSE x \in S : TRUE IN charge[s] + SumOf(S \ {s})

-----------------------------------------------------------------------------

\* Bytes leave in segment order, with none skipped.
InOrder == \A i \in 1..Len(emitted) : emitted[i] = i

\* A reader that reports the end passed on every segment.
OkIsComplete == rpc = "ok" => Len(emitted) = N

\* A corrupt segment never ends in a complete read.
FailureIsReported == (rpc = "ok") => \A s \in Segments : state[s] /= "failed"

\* What the started segments hold stays within the budget, and the reader's
\* count matches it. A thread's segment holds its whole output and its
\* dictionary, and the streaming front only its dictionary.
MemoryBounded ==
    /\ held <= Budget
    /\ held = SumOf({s \in Segments : state[s] \in Holding})
    /\ \A s \in Segments :
         /\ state[s] = "streaming" => charge[s] = Streamed[s]
         /\ state[s] \in {"decoding", "ready"} => charge[s] = Sizes[s]

\* Only the front segment streams.
OnlyTheFrontStreams == \A s \in Segments : state[s] = "streaming" => s = Front

\* At most `Window` decoders run at once, the reader's own included.
ThreadsBounded == threads + Streaming <= Window

\* The folder worker never returns while a decoder thread it started runs,
\* so no thread outlives the extraction and holds the archive open.
NoThreadOutlivesReader == rpc \in {"ok", "returned"} => threads = 0

\* When the reader is done or gone and everything settles, nothing is held.
NothingLeaks == (rpc /= "reading" /\ threads = 0
                 /\ \A s \in Segments : state[s] \notin Holding)
                => held = 0
=============================================================================
