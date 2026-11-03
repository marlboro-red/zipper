---------------------------- MODULE WriterPool ----------------------------
(***************************************************************************)
(* The tar writer pool in src/native_tar.rs: one decoding thread (`parse`  *)
(* calling `Extraction::write_file`) hands file data to writer threads     *)
(* (`Writers::work` and `write_job`) through lane queues and per-file      *)
(* chunk streams, under the `InFlight` byte budget. One chunk here is one  *)
(* `Chunk` of up to `CHUNK` bytes. Each action is one step the code takes  *)
(* while holding no lock the other side waits on, or one blocking call.    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    ChunksPerFile,  \* sequence: number of chunks of each tar file entry, in archive order
    Replaces,       \* set of file indices whose name repeats an earlier file (`replaces`)
    Writers,        \* writer threads (`ReadOptions::writers`)
    Lanes,          \* 1 on Unix; Writers on Windows (`ONE_WRITER_PER_FOLDER`)
    Budget,         \* chunks allowed in flight (`BYTES_IN_FLIGHT` / `CHUNK`)
    LaneCap,        \* jobs a lane queue holds (`QUEUED_FILES`)
    StreamCap,      \* chunks a file stream holds (`QUEUED_CHUNKS_PER_FILE`)
    WritesCanFail,  \* whether `write_job` can fail or panic
    DecoderCanFail  \* whether the stream can turn out corrupt mid-file

Files == 1..Len(ChunksPerFile)
W == 1..Writers
L == 1..Lanes
LaneOfWriter(w) == ((w - 1) % Lanes) + 1   \* `queues[index % lane_count]`

VARIABLES
    dpc,        \* decoder step
    f,          \* current file
    taken,      \* chunks of file f taken so far
    holding,    \* the decoder holds a filled chunk it has not sent
    inflight,   \* live chunks (`InFlight::bytes` in chunk units)
    lanesOpen,  \* lane senders exist (`Writers::close` clears them)
    laneQ,      \* lane -> queued jobs (file indices)
    stream,     \* file -> chunks queued in its stream
    sending,    \* file -> its stream sender is alive
    receiving,  \* file -> its stream receiver is alive
    pending,    \* `Writers::pending`
    failure,    \* `Writers::failure` holds an error
    everFailed, \* some write failed at some point
    wpc,        \* writer -> step
    job,        \* writer -> file it holds, or 0
    written,    \* file -> chunks written to disk
    removing,   \* the decoder is removing an earlier file of the same name
    result      \* "none", "ok" or "error": what `read_archive` returns

vars == <<dpc, f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
          receiving, pending, failure, everFailed, wpc, job, written, removing, result>>

Init ==
    /\ dpc = "start"
    /\ f = 1
    /\ taken = 0
    /\ holding = FALSE
    /\ inflight = 0
    /\ lanesOpen = TRUE
    /\ laneQ = [l \in L |-> <<>>]
    /\ stream = [x \in Files |-> 0]
    /\ sending = [x \in Files |-> FALSE]
    /\ receiving = [x \in Files |-> FALSE]
    /\ pending = 0
    /\ failure = FALSE
    /\ everFailed = FALSE
    /\ wpc = [w \in W |-> "idle"]
    /\ job = [w \in W |-> 0]
    /\ written = [x \in Files |-> 0]
    /\ removing = FALSE
    /\ result = "none"

-----------------------------------------------------------------------------
(* Decoder: `parse` -> `Extraction::member` -> `write_file`.               *)

\* Next tar member, or the end-of-archive marker.
DecoderNextFile ==
    /\ dpc = "start"
    /\ IF f > Len(ChunksPerFile)
       THEN dpc' = "closing"
       ELSE dpc' = IF f \in Replaces THEN "drain" ELSE "take"
    /\ UNCHANGED <<f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, pending, failure, everFailed, wpc, job, written, removing, result>>

\* `writers.drain()` returns once `pending` is 0, then `check()` takes any failure.
DecoderDrains ==
    /\ dpc = "drain"
    /\ pending = 0
    /\ IF failure
       THEN /\ dpc' = "failing" /\ failure' = FALSE /\ removing' = FALSE
       ELSE /\ dpc' = "remove" /\ removing' = TRUE /\ UNCHANGED failure
    /\ UNCHANGED <<f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, pending, everFailed, wpc, job, written, result>>

\* `fs::remove_file(root.join(name))` for the earlier file of the same name.
DecoderRemovesEarlierFile ==
    /\ dpc = "remove"
    /\ dpc' = "take"
    /\ removing' = FALSE
    /\ UNCHANGED <<f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, pending, failure, everFailed, wpc, job, written, result>>

\* `writers.flight.take(..)` blocks while other chunks would pass the budget,
\* then `input.fill` reads the chunk.
DecoderTakesChunk ==
    /\ dpc = "take"
    /\ inflight = 0 \/ inflight + 1 <= Budget
    /\ inflight' = inflight + 1
    /\ holding' = TRUE
    /\ taken' = taken + 1
    /\ dpc' = IF taken = 0 THEN "count" ELSE "send_chunk"
    /\ UNCHANGED <<f, lanesOpen, laneQ, stream, sending, receiving, pending,
                   failure, everFailed, wpc, job, written, removing, result>>

\* The stream is corrupt: `parse` returns an error. The held chunk and the
\* stream sender drop as `write_file` unwinds.
DecoderFindsCorruptInput ==
    /\ DecoderCanFail
    /\ dpc \in {"take", "count", "send_chunk"}
    /\ dpc' = "failing"
    /\ inflight' = IF holding THEN inflight - 1 ELSE inflight
    /\ holding' = FALSE
    /\ sending' = [sending EXCEPT ![f] = FALSE]
    /\ UNCHANGED <<f, taken, lanesOpen, laneQ, stream, receiving, pending, failure,
                   everFailed, wpc, job, written, removing, result>>

\* `Writers::send` counts the job before it sends it.
DecoderCountsJob ==
    /\ dpc = "count"
    /\ pending' = pending + 1
    /\ dpc' = "send_job"
    /\ UNCHANGED <<f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, failure, everFailed, wpc, job, written, removing, result>>

\* `lane.send(job)` blocks while the lane is full. Any lane may be the hash.
\* Then `self.check()` takes a stored failure and returns it.
DecoderSendsJob ==
    /\ dpc = "send_job"
    /\ \E l \in L :
         /\ Len(laneQ[l]) < LaneCap
         /\ laneQ' = [laneQ EXCEPT ![l] = Append(@, f)]
    /\ holding' = FALSE
    /\ LET more == taken < ChunksPerFile[f] IN
         /\ receiving' = [receiving EXCEPT ![f] = more]
         \* On `?`, `write_file` returns and its local `stream` sender drops.
         /\ sending' = [sending EXCEPT ![f] = more /\ ~failure]
         /\ IF failure
            THEN /\ dpc' = "failing" /\ failure' = FALSE
            ELSE /\ dpc' = (IF more THEN "take" ELSE "next")
                 /\ UNCHANGED failure
    /\ UNCHANGED <<f, taken, inflight, lanesOpen, stream, pending, everFailed, wpc, job,
                   written, removing, result>>

\* `chunks.send(chunk)` blocks while the stream is full. With the receiver
\* gone it fails, and the chunk drops (`writers.stopped()`).
DecoderSendsChunk ==
    /\ dpc = "send_chunk"
    /\ \/ /\ receiving[f]
          /\ stream[f] < StreamCap
          /\ stream' = [stream EXCEPT ![f] = @ + 1]
          /\ holding' = FALSE
          /\ dpc' = IF taken < ChunksPerFile[f] THEN "take" ELSE "next"
          /\ UNCHANGED <<inflight, failure>>
       \/ /\ ~receiving[f]
          /\ holding' = FALSE
          /\ inflight' = inflight - 1
          /\ sending' = [sending EXCEPT ![f] = FALSE]
          /\ failure' = FALSE
          /\ dpc' = "failing"
          /\ UNCHANGED stream
    /\ UNCHANGED <<f, taken, lanesOpen, laneQ, receiving, pending, everFailed, wpc,
                   job, written, removing, result>>
    /\ (receiving[f] => UNCHANGED sending)

\* `drop(stream)` at the end of `write_file`; on to the next member.
DecoderFinishesFile ==
    /\ dpc = "next"
    /\ sending' = [sending EXCEPT ![f] = FALSE]
    /\ f' = f + 1
    /\ taken' = 0
    /\ dpc' = "start"
    /\ UNCHANGED <<holding, inflight, lanesOpen, laneQ, stream, receiving, pending,
                   failure, everFailed, wpc, job, written, removing, result>>

\* `writers.close()` after `parse` returns, success or error.
DecoderClosesLanes ==
    /\ dpc \in {"closing", "failing"}
    /\ lanesOpen' = FALSE
    /\ dpc' = IF dpc = "closing" THEN "joining" ELSE "joining_failed"
    /\ UNCHANGED <<f, taken, holding, inflight, laneQ, stream, sending, receiving,
                   pending, failure, everFailed, wpc, job, written, removing, result>>

\* Every writer thread is joined; then `parsed?` and `writers.check()?`.
DecoderJoins ==
    /\ dpc \in {"joining", "joining_failed"}
    /\ \A w \in W : wpc[w] = "exited"
    /\ result' = IF dpc = "joining_failed" \/ failure THEN "error" ELSE "ok"
    /\ dpc' = "done"
    /\ UNCHANGED <<f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, pending, failure, everFailed, wpc, job, written, removing>>

-----------------------------------------------------------------------------
(* Writers: `Writers::work` and `write_job`.                               *)

\* `jobs.lock().unwrap().recv()`: the next job, or exit once the lane is
\* empty and closed.
WriterReceives(w) ==
    /\ wpc[w] = "idle"
    /\ LET l == LaneOfWriter(w) IN
         \/ /\ laneQ[l] /= <<>>
            /\ job' = [job EXCEPT ![w] = Head(laneQ[l])]
            /\ laneQ' = [laneQ EXCEPT ![l] = Tail(@)]
            /\ wpc' = [wpc EXCEPT ![w] = IF failure THEN "drop" ELSE "first"]
         \/ /\ laneQ[l] = <<>>
            /\ ~lanesOpen
            /\ wpc' = [wpc EXCEPT ![w] = "exited"]
            /\ UNCHANGED <<job, laneQ>>
    /\ UNCHANGED <<dpc, f, taken, holding, inflight, lanesOpen, stream, sending,
                   receiving, pending, failure, everFailed, written, removing, result>>

\* `write(&job.first.data)`, which can fail or panic.
WriterWritesFirst(w) ==
    /\ wpc[w] = "first"
    /\ LET x == job[w] IN
         \/ /\ written' = [written EXCEPT ![x] = @ + 1]
            /\ wpc' = [wpc EXCEPT ![w] = IF ChunksPerFile[x] > 1 THEN "rest" ELSE "drop"]
            /\ UNCHANGED <<failure, everFailed>>
         \/ /\ WritesCanFail
            /\ failure' = TRUE
            /\ everFailed' = TRUE
            /\ wpc' = [wpc EXCEPT ![w] = "drop"]
            /\ UNCHANGED written
    /\ UNCHANGED <<dpc, f, taken, holding, inflight, lanesOpen, laneQ, stream, sending,
                   receiving, pending, job, removing, result>>

\* `for chunk in rest { write(&chunk.data)? }`: each chunk drops after its
\* write; the loop ends when the decoder drops the sender.
WriterWritesRest(w) ==
    /\ wpc[w] = "rest"
    /\ LET x == job[w] IN
         \/ /\ stream[x] > 0
            /\ stream' = [stream EXCEPT ![x] = @ - 1]
            /\ inflight' = inflight - 1
            /\ \/ /\ written' = [written EXCEPT ![x] = @ + 1]
                  /\ UNCHANGED <<failure, everFailed, wpc>>
               \/ /\ WritesCanFail
                  /\ failure' = TRUE
                  /\ everFailed' = TRUE
                  /\ wpc' = [wpc EXCEPT ![w] = "drop"]
                  /\ UNCHANGED written
         \/ /\ stream[x] = 0
            /\ ~sending[x]
            /\ wpc' = [wpc EXCEPT ![w] = "drop"]
            /\ UNCHANGED <<stream, inflight, written, failure, everFailed>>
    /\ UNCHANGED <<dpc, f, taken, holding, lanesOpen, laneQ, sending, receiving,
                   pending, job, removing, result>>

\* `drop(job)`: the first chunk, the stream receiver and its queued chunks
\* drop; then `pending -= 1` and `notify_all` at zero.
WriterDropsJob(w) ==
    /\ wpc[w] = "drop"
    /\ LET x == job[w] IN
         /\ inflight' = inflight - 1 - stream[x]
         /\ stream' = [stream EXCEPT ![x] = 0]
         /\ receiving' = [receiving EXCEPT ![x] = FALSE]
    /\ pending' = pending - 1
    /\ job' = [job EXCEPT ![w] = 0]
    /\ wpc' = [wpc EXCEPT ![w] = "idle"]
    /\ UNCHANGED <<dpc, f, taken, holding, lanesOpen, laneQ, sending, failure,
                   everFailed, written, removing, result>>

\* The run has ended; nothing changes.
Finished ==
    /\ dpc = "done"
    /\ UNCHANGED vars

Next ==
    \/ DecoderNextFile \/ DecoderDrains \/ DecoderRemovesEarlierFile
    \/ DecoderTakesChunk \/ DecoderFindsCorruptInput \/ DecoderCountsJob
    \/ DecoderSendsJob \/ DecoderSendsChunk \/ DecoderFinishesFile
    \/ DecoderClosesLanes \/ DecoderJoins
    \/ \E w \in W : WriterReceives(w) \/ WriterWritesFirst(w)
                    \/ WriterWritesRest(w) \/ WriterDropsJob(w)
    \/ Finished

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

\* Queued file data never passes the budget (`InFlight::take`).
BudgetHolds == inflight <= Budget

\* No chunk outlives the run: every `Chunk` dropped (`Drop for Chunk`).
NothingLeaks == dpc = "done" => inflight = 0

\* A run that reports success wrote every chunk of every file.
SuccessIsComplete == result = "ok" => \A x \in Files : written[x] = ChunksPerFile[x]

\* A failed write never ends in a reported success.
FailureIsReported == (dpc = "done" /\ everFailed) => result = "error"

\* An earlier file of the same name is removed only when no writer holds a job.
RemoveAfterDrain == removing => \A w \in W : job[w] = 0

\* No file is written past its size.
NoOverwrite == \A x \in Files : written[x] <= ChunksPerFile[x]
=============================================================================
