---
name: zipper-verify-zip-budget
description: Model-check the ZIP entry workers and the shared decoder memory budget with TLA+. Use before committing any change to how ZIP workers reserve decoder memory (`ZIP_DECODER_MEMORY`, the reservation in `stream_entry`), to `Memory` in src/memory.rs, or to how a write-behind entry hands output to its writer thread, and when a ZIP extraction hangs or uses more memory than the budget.
---

# Verify the ZIP decoder budget

XZ, LZMA and PPMd entries can each need hundreds of MiB, and up to 64 Rayon workers decode at once. Each such entry reserves memory from one shared `Memory` before it decodes and waits while the budget is short. A worker that holds a reservation may also wait on the bounded channel to its write-behind writer. The model in `verification/zip-decoder-budget/` checks that these waits never deadlock and never exceed the budget.

## The rules

| Rule | Harm if it breaks |
| --- | --- |
| `MemoryBounded`: reservations never exceed the budget | Memory grows with the worker count, as it did before the budget, to 10.75 GB in one review case |
| `AllReleased`: the budget is whole again when extraction ends | The next extraction in the same process stalls |
| `DecodedAtMostOnce` and `SuccessIsComplete` | Missing or doubled output |
| `FailureIsReported`: a corrupt or unwritable entry is never published | A damaged archive reports success |
| No deadlock | The extraction hangs |

## What it covers

- Five entries, largest first. The first asks for more than the whole budget and writes behind, two reserve part of it, and two reserve nothing. Three workers, a budget of 4, two chunks per write-behind entry and a channel of depth 1.
- Decoding and writing can each fail. A failure stops Rayon from starting new entries, as `try_for_each_init` does.
- The writer is a scoped OS thread outside the Rayon pool. That is why holding a reservation while waiting on the channel cannot deadlock.

## Run it

1. Copy `verification/zip-decoder-budget/` into a fresh scratch folder. Never run TLC inside the repository.
2. Run the pstack `/statechart` skill on `MCZip.tla` with `zip.cfg`, with its own output folder.
3. The report must list all five rules and no deadlock under "Rules that hold". Then run `cargo xtask verify-sync zip-decoder-budget`.

## Proof that the model can fail

| Change to `ZipDecoders.tla` | What TLC reports |
| --- | --- |
| `Release` keeps the memory | `MemoryBounded`, `AllReleased` and no deadlock break |
| `Reserve` does not wait for the budget | `MemoryBounded` breaks |
| The writer can run only when a Rayon worker is idle, as a Rayon task would | Deadlock |
