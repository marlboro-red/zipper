--------------------------- MODULE WriteBehind ---------------------------
(***************************************************************************)
(* `write_behind` in src/native_zip.rs. The calling thread decodes one ZIP *)
(* entry and fills buffers of `WRITE_BEHIND_CHUNK` bytes; a scoped writer  *)
(* thread writes them to the output file. Buffers circulate through two    *)
(* bounded channels: `full` (capacity `WRITE_BEHIND_DEPTH`) carries filled *)
(* buffers to the writer, and `empty` (capacity depth + 1) carries cleared *)
(* buffers back. Each action is one channel operation, one write, or one   *)
(* decode step.                                                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Chunks,           \* buffers the entry fills, counting a final partial one
    LastPartial,      \* whether the last buffer is partial (sent after `produce`)
    Depth,            \* WRITE_BEHIND_DEPTH
    DecodeCanFail,    \* `stream_entry` can fail: bad data, CRC or size mismatch
    WriteCanFail,     \* `output.write_all` can fail
    WriterCanPanic    \* the writer thread can panic

VARIABLES
    ppc,            \* producer (calling thread) step
    produced,       \* buffers the decoder has filled so far
    current,        \* chunk number in `current`, 0 when it is empty
    holdingNext,    \* the producer holds `next` and has not sent `current`
    emptyQ,         \* cleared buffers in the `empty` channel
    fullQ,          \* chunk numbers in the `full` channel, oldest first
    fullOpen,       \* the producer still holds the `full` sender
    writerAlive,    \* the writer thread holds `filled` and `empty`
    wpc,            \* writer step
    wchunk,         \* chunk number the writer holds, 0 for none
    written,        \* chunk numbers written to the file, in order
    produceResult,  \* "none", "ok", "decode_error" or "channel_error"
    writerStopped,  \* `writer_stopped`
    writerResult,   \* "none", "ok", "io_error" or "panic"
    result          \* what `write_behind` returns

vars == <<ppc, produced, current, holdingNext, emptyQ, fullQ, fullOpen,
          writerAlive, wpc, wchunk, written, produceResult, writerStopped,
          writerResult, result>>

FullChunks == IF LastPartial THEN Chunks - 1 ELSE Chunks

Init ==
    /\ ppc = "first_recv"
    /\ produced = 0
    /\ current = 0
    /\ holdingNext = FALSE
    /\ emptyQ = Depth + 1
    /\ fullQ = <<>>
    /\ fullOpen = TRUE
    /\ writerAlive = TRUE
    /\ wpc = "recv"
    /\ wchunk = 0
    /\ written = <<>>
    /\ produceResult = "none"
    /\ writerStopped = FALSE
    /\ writerResult = "none"
    /\ result = "none"

-----------------------------------------------------------------------------
(* Producer: the calling thread.                                           *)

\* `let mut current = emptied.recv()?;` The channel holds depth + 1 buffers.
ProducerTakesFirstBuffer ==
    /\ ppc = "first_recv"
    /\ emptyQ > 0
    /\ emptyQ' = emptyQ - 1
    /\ ppc' = "fill"
    /\ UNCHANGED <<produced, current, holdingNext, fullQ, fullOpen, writerAlive, wpc,
                   wchunk, written, produceResult, writerStopped, writerResult, result>>

\* `current.extend_from_slice(now)` until `current` is full, or the last
\* partial buffer of the entry.
ProducerFillsBuffer ==
    /\ ppc = "fill"
    /\ produced < Chunks
    /\ produced' = produced + 1
    /\ current' = produced + 1
    /\ ppc' = IF produced + 1 <= FullChunks THEN "next_recv" ELSE "fill"
    /\ UNCHANGED <<holdingNext, emptyQ, fullQ, fullOpen, writerAlive, wpc, wchunk,
                   written, produceResult, writerStopped, writerResult, result>>

\* `produce` returns Ok: every byte decoded and the CRC matched.
ProducerFinishes ==
    /\ ppc = "fill"
    /\ produced = Chunks
    /\ produceResult' = "ok"
    /\ ppc' = "final_send"
    /\ UNCHANGED <<produced, current, holdingNext, emptyQ, fullQ, fullOpen, writerAlive,
                   wpc, wchunk, written, writerStopped, writerResult, result>>

\* `stream_entry` fails at any point while decoding, including the CRC check
\* after the last byte. The buffer in `current` drops when the scope ends.
ProducerFailsToDecode ==
    /\ DecodeCanFail
    /\ ppc = "fill"
    /\ produceResult' = "decode_error"
    /\ ppc' = "close"
    /\ UNCHANGED <<produced, current, holdingNext, emptyQ, fullQ, fullOpen, writerAlive,
                   wpc, wchunk, written, writerStopped, writerResult, result>>

\* `let next = emptied.recv().inspect_err(..)?;` It fails once the writer has
\* exited, which drops the `empty` sender, and no buffer is queued.
ProducerTakesNextBuffer ==
    /\ ppc = "next_recv"
    /\ \/ /\ emptyQ > 0
          /\ emptyQ' = emptyQ - 1
          /\ holdingNext' = TRUE
          /\ ppc' = "full_send"
          /\ UNCHANGED <<produceResult, writerStopped>>
       \/ /\ emptyQ = 0
          /\ ~writerAlive
          /\ writerStopped' = TRUE
          /\ produceResult' = "channel_error"
          /\ ppc' = "close"
          /\ UNCHANGED <<emptyQ, holdingNext>>
    /\ UNCHANGED <<produced, current, fullQ, fullOpen, writerAlive, wpc, wchunk,
                   written, writerResult, result>>

\* `full.send(mem::replace(&mut current, next)).inspect_err(..)?;` It blocks
\* while `full` is full, and fails once the writer dropped `filled`.
ProducerSendsBuffer ==
    /\ ppc = "full_send"
    /\ \/ /\ writerAlive
          /\ Len(fullQ) < Depth
          /\ fullQ' = Append(fullQ, current)
          /\ current' = 0
          /\ holdingNext' = FALSE
          /\ ppc' = "fill"
          /\ UNCHANGED <<produceResult, writerStopped>>
       \/ /\ ~writerAlive
          /\ writerStopped' = TRUE
          /\ produceResult' = "channel_error"
          /\ ppc' = "close"
          /\ UNCHANGED <<fullQ, current, holdingNext>>
    /\ UNCHANGED <<produced, emptyQ, fullOpen, writerAlive, wpc, wchunk, written,
                   writerResult, result>>

\* `if produced.is_ok() && !current.is_empty() { let _ = full.send(current); }`
FinalSend ==
    /\ ppc = "final_send"
    /\ \/ /\ current = 0
          /\ UNCHANGED <<fullQ, current>>
       \/ /\ current /= 0
          /\ writerAlive
          /\ Len(fullQ) < Depth
          /\ fullQ' = Append(fullQ, current)
          /\ current' = 0
       \/ /\ current /= 0
          /\ ~writerAlive
          /\ UNCHANGED <<fullQ, current>>
    /\ ppc' = "close"
    /\ UNCHANGED <<produced, holdingNext, emptyQ, fullOpen, writerAlive, wpc, wchunk,
                   written, produceResult, writerStopped, writerResult, result>>

\* `drop(full);`
ProducerDropsSender ==
    /\ ppc = "close"
    /\ fullOpen' = FALSE
    /\ ppc' = "join"
    /\ UNCHANGED <<produced, current, holdingNext, emptyQ, fullQ, writerAlive, wpc,
                   wchunk, written, produceResult, writerStopped, writerResult, result>>

\* `writer.join()`, then the `match (produced, written)`.
ProducerJoins ==
    /\ ppc = "join"
    /\ ~writerAlive
    /\ result' =
         CASE writerResult = "panic" -> "writer_panic"
           [] produceResult = "ok" /\ writerResult = "ok" -> "ok"
           [] produceResult /= "ok" /\ writerResult = "io_error" /\ writerStopped -> "io_error"
           [] produceResult /= "ok" -> produceResult
           [] OTHER -> "io_error"
    /\ ppc' = "done"
    /\ UNCHANGED <<produced, current, holdingNext, emptyQ, fullQ, fullOpen, writerAlive,
                   wpc, wchunk, written, produceResult, writerStopped, writerResult>>

-----------------------------------------------------------------------------
(* Writer thread.                                                          *)

\* Thread exit: `filled`, `empty`, any queued buffers and the output drop.
Exit(outcome) ==
    /\ writerAlive' = FALSE
    /\ writerResult' = outcome
    /\ wpc' = "exited"
    /\ wchunk' = 0
    /\ fullQ' = <<>>

\* `for mut chunk in filled`: the next buffer, or the loop ends once the
\* producer dropped `full` and nothing is queued.
WriterReceives ==
    /\ wpc = "recv"
    /\ \/ /\ fullQ /= <<>>
          /\ wchunk' = Head(fullQ)
          /\ fullQ' = Tail(fullQ)
          /\ wpc' = "write"
          /\ UNCHANGED <<writerAlive, writerResult>>
       \/ /\ fullQ = <<>>
          /\ ~fullOpen
          /\ Exit("ok")
    /\ UNCHANGED <<ppc, produced, current, holdingNext, emptyQ, fullOpen, written,
                   produceResult, writerStopped, result>>

\* `output.write_all(&chunk)?`, which can fail; or the thread panics.
WriterWrites ==
    /\ wpc = "write"
    /\ \/ /\ written' = Append(written, wchunk)
          /\ wpc' = "give_back"
          /\ UNCHANGED <<writerAlive, writerResult, wchunk, fullQ>>
       \/ /\ WriteCanFail
          /\ Exit("io_error")
          /\ UNCHANGED written
       \/ /\ WriterCanPanic
          /\ Exit("panic")
          /\ UNCHANGED written
    /\ UNCHANGED <<ppc, produced, current, holdingNext, emptyQ, fullOpen,
                   produceResult, writerStopped, result>>

\* `chunk.clear(); let _ = empty.send(chunk);` The channel has room for every
\* buffer, so the send never blocks.
WriterGivesBufferBack ==
    /\ wpc = "give_back"
    /\ emptyQ' = emptyQ + 1
    /\ wchunk' = 0
    /\ wpc' = "recv"
    /\ UNCHANGED <<ppc, produced, current, holdingNext, fullQ, fullOpen, writerAlive,
                   written, produceResult, writerStopped, writerResult, result>>

Finished ==
    /\ ppc = "done"
    /\ UNCHANGED vars

Next ==
    \/ ProducerTakesFirstBuffer \/ ProducerFillsBuffer \/ ProducerFinishes
    \/ ProducerFailsToDecode \/ ProducerTakesNextBuffer \/ ProducerSendsBuffer
    \/ FinalSend \/ ProducerDropsSender \/ ProducerJoins
    \/ WriterReceives \/ WriterWrites \/ WriterGivesBufferBack
    \/ Finished

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

ChunkNumbers(n) == [i \in 1..n |-> i]

\* Buffers are written in the order they were filled, with none skipped.
WrittenInOrder == written = ChunkNumbers(Len(written))

\* A reported success wrote every byte of the entry.
SuccessIsComplete == result = "ok" => written = ChunkNumbers(Chunks)

\* Any failed decode or failed write ends in an error.
FailureIsReported ==
    (ppc = "done" /\ (produceResult = "decode_error" \/ writerResult \in {"io_error", "panic"}))
        => result /= "ok"

\* When the writer failed, the caller sees the write error, not a closed
\* channel.
WriterIsBlamed == result /= "channel_error"

\* No more than depth + 1 buffers exist, so memory stays bounded.
BuffersBounded ==
    emptyQ + Len(fullQ) + (IF ppc \in {"fill", "next_recv", "full_send", "final_send"} THEN 1 ELSE 0)
        + (IF holdingNext THEN 1 ELSE 0) + (IF wchunk /= 0 THEN 1 ELSE 0) <= Depth + 1
=============================================================================
