---
name: zipper-verify-zip-write-behind
description: Model-check the ZIP write-behind pipeline in src/native_zip.rs with TLA+. Use before committing any change to write_behind, WRITE_BEHIND_DEPTH, WRITE_BEHIND_CHUNK or the call that hands it a large entry, and when a large ZIP entry hangs, is truncated, or reports the wrong error.
---

# Verify the ZIP write-behind pipeline

The native ZIP reader decodes an entry of 16 MiB or more on its worker and hands full buffers to a second thread that writes them. The two threads pass buffers back and forth through two bounded channels, and either side can fail first. A wrong order of steps hangs the extraction, drops the last bytes of a file, or reports "channel closed" in place of the real write error. `verification/zip-write-behind/` is a TLA+ model of `write_behind`, and TLC checks every order of its steps within a bound.

## The rules

Each rule is an invariant in `WriteBehind.tla`. TLC also checks that no state is stuck, which is the rule `NoDeadlock`.

| Rule | In plain words | Harm if it breaks |
| --- | --- | --- |
| `NoDeadlock` | Every run ends, whether it succeeds, the decoder fails, a write fails, or the writer panics. | `zipper extract` hangs on a large entry. |
| `WrittenInOrder` | Buffers reach the file in the order they were filled, with none skipped. | Corrupt output. |
| `SuccessIsComplete` | A reported success wrote every buffer, including a final partial one. | A truncated file is published. |
| `FailureIsReported` | A failed decode, failed write or writer panic never ends in success. | A broken file looks good. |
| `WriterIsBlamed` | When the writer fails, the caller sees its error, never the closed channel. | The user sees "sending on a closed channel" and not "disk full". |
| `BuffersBounded` | At most `WRITE_BEHIND_DEPTH + 1` buffers exist. | Memory grows with entry size. |

## What the model covers

| Code at the sync commit | Model |
| --- | --- |
| The two `sync_channel` calls and the depth + 1 buffers sent to `empty` | `Init` |
| `let mut current = emptied.recv()?` | `ProducerTakesFirstBuffer` |
| The sink closure's `extend_from_slice` until `current` is full | `ProducerFillsBuffer` |
| `emptied.recv().inspect_err(..)?` | `ProducerTakesNextBuffer` |
| `full.send(mem::replace(..)).inspect_err(..)?` | `ProducerSendsBuffer` |
| `produce` returning, Ok or with a decode or CRC error | `ProducerFinishes`, `ProducerFailsToDecode` |
| `if produced.is_ok() && !current.is_empty() { let _ = full.send(current) }` | `FinalSend` |
| `drop(full)` | `ProducerDropsSender` |
| `writer.join()` and the `match (produced, written)` | `ProducerJoins` |
| The writer's `for mut chunk in filled`, `write_all`, `chunk.clear()` and `empty.send` | `WriterReceives`, `WriterWrites`, `WriterGivesBufferBack` |

One step of `ProducerFillsBuffer` stands for every sink call that fills one buffer, because nothing the other thread does depends on how a buffer fills. A decode error can happen between any two fills and after the last one, as the CRC check does. When the writer thread ends, by return, error or panic, its `filled` receiver and `empty` sender drop together, with any queued buffers.

The model leaves out the bytes themselves, the 256 KiB slices on Windows, `BufWriter` flushing, `finish`, and the decision in `read_archive_with_options` to call `write_behind`.

- The bound is 6 buffers with a depth of 4, which is `WRITE_BEHIND_DEPTH`. `MCFull` ends on a full buffer, and `MCPartial` ends on a partial one that `FinalSend` sends.
- Each model runs in about one second, with 641 and 614 distinct states.
- The sync commit is `d066d4e`.

## When to run it

Run it when a diff touches a row of the coverage table, `WRITE_BEHIND_DEPTH`, `WRITE_BEHIND_CHUNK`, or the order of the channel calls. To see what changed since the sync commit, run `git diff d066d4e -- src/native_zip.rs` and look at `write_behind`.

## Run it

1. Copy `verification/zip-write-behind/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCFull.tla` with `full.cfg`, and again on `MCPartial.tla` with `partial.cfg`. Give each run its own output folder.
3. Both reports must list all six rules under "Rules that hold".

## Read a result

- A rule that breaks comes with its shortest path. Read each step as the code step in the coverage table. Then replay the path against the real code, as a test or a live run.
- If the path cannot happen in the code, the model is wrong. Find the code step it gets wrong, fix the model, and say so in the commit.
- If the path can happen, it is a bug. Report it with the path in the code's words. Do not weaken a rule or the model to make it pass.

## Change the model with the code

When a change to covered code is intended, change the matching action in the same commit, so that each action still names one step of the code. Then run both models. After both pass and you have checked the model against the new code, move the sync commit in this file to the commit that holds the change.

The rules are proven able to fail. Each of these model changes, which match plausible regressions, breaks the named rule on `MCPartial`:

| Change | Rule that breaks |
| --- | --- |
| `ProducerSendsBuffer` no longer sets `writerStopped`, as if `full.send` lost its `inspect_err` | `WriterIsBlamed` |
| `FinalSend` never queues the partial buffer, as if the final `full.send(current)` were removed | `SuccessIsComplete` |
| `WriterGivesBufferBack` keeps the buffer, as if the writer skipped `empty.send(chunk)` | `NoDeadlock` |
| `ProducerJoins` reports success for a decode that succeeded and a write that failed | `SuccessIsComplete`, `FailureIsReported` |
