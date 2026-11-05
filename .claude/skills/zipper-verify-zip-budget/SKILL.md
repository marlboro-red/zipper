---
name: zipper-verify-zip-budget
description: Model-check the ZIP entry workers and the shared decoder memory budget with TLA+. Use before committing any change to how ZIP workers take decoder memory (`ZIP_DECODER_MEMORY`, `decoder_need`, the `reserve_within` at the start of an entry, `DECODER_MEMORY_PATIENCE`, the pass loop and `for_the_next_pass`), to `Memory` in src/memory.rs, or to how a write-behind entry hands output to its writer thread, and when a ZIP extraction hangs or uses more memory than the budget.
---

# Verify the ZIP decoder budget

XZ, LZMA and PPMd entries can each need hundreds of MiB, and up to 64 Rayon workers decode at once. Each such entry takes memory from one shared `Memory` before it starts, with `reserve_within`, which waits at most `DECODER_MEMORY_PATIENCE`. When the budget stays short, the entry is recorded in `for_the_next_pass` and the worker moves on, and `run` decodes the recorded entries in another pass. Until `80af2b5` the worker waited for memory without a limit, which a progress callback that runs Rayon work could turn into a hang, as `zipper-verify-progress` describes. A worker that holds a reservation may also wait on the bounded channel to its write-behind writer. The model in `verification/zip-decoder-budget/` checks that the workers never deadlock, never exceed the budget, and always end.

## The rules

| Rule | Harm if it breaks |
| --- | --- |
| `MemoryBounded`: reservations never exceed the budget | Memory grows with the worker count, as it did before the budget, to 10.75 GB in one review case |
| `AllReleased`: the budget is whole again when extraction ends | The next extraction in the same process stalls |
| `DecodedAtMostOnce` and `SuccessIsComplete` | Missing or doubled output |
| `FailureIsReported`: a corrupt or unwritable entry is never published | A damaged archive reports success |
| No deadlock | The extraction hangs |
| `EventuallyDone`, under weak fairness | Passes repeat forever |

## What it covers

- Five entries, largest first. The first asks for more than the whole budget and writes behind, two reserve part of it, and two reserve nothing. Three workers, a budget of 4, two chunks per write-behind entry and a channel of depth 1.
- Decoding and writing can each fail. A failure stops Rayon from starting new entries, as `try_for_each_init` does, and ends the pass loop.
- After a pass, recorded entries go back in every possible order for the next one.
- A short reserve may wait while other steps run, then take the memory or record the entry, as the patience allows.
- It has 237,969 distinct states.
- The writer is a scoped OS thread outside the Rayon pool. That is why holding a reservation while waiting on the channel cannot deadlock.

## Run it

1. Copy `verification/zip-decoder-budget/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCZip.tla` with `zip.cfg`, with its own output folder.
3. The report must list all five rules, no deadlock and `EventuallyDone` under "Rules that hold". Then run `cargo xtask verify-sync zip-decoder-budget`.

## Proof that the model can fail

| Change to `ZipDecoders.tla` | What TLC reports |
| --- | --- |
| `Release` keeps the memory | `MemoryBounded`, `AllReleased` and `EventuallyDone` break |
| `Reserve` takes the memory even when it is short | `MemoryBounded` breaks |
| `CallerFinishes` ignores recorded entries, as if `run` made one pass | `SuccessIsComplete` breaks |
| `CallerStartsNextPass` drops the recorded entries | `SuccessIsComplete` breaks |

At `80af2b5`, a writer that could run only when a Rayon worker was idle, as a Rayon task would, deadlocked here, because the other workers waited for memory. With `reserve_within` no worker waits for memory without a limit, so that change no longer deadlocks within this bound.
