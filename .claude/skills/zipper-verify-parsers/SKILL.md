---
name: zipper-verify-parsers
description: Prove with Kani that Zipper's byte-level parsers never panic or overflow on any input up to a bound. Use before committing any change to the 7z header readers (Bytes, Malformed, check), the LZMA2 chunk header and segment plan, or the ZIP time fields in src/native_zip.rs, and when a parser panics or overflows on hostile input.
---

# Verify the parsers with Kani

A parser that panics or overflows on a crafted archive crashes a library caller or, in release builds, misreads the archive silently. Tests and fuzzing try many inputs. Kani compiles the real Rust code into a logic problem and checks every input up to a bound, so a pass covers inputs nobody thought to write. The proofs are the `#[cfg(kani)] mod proofs` modules in `src/native_7z.rs` and `src/native_zip.rs`.

## The proofs

| Proof | It proves, for every input in its bound | Time |
| --- | --- | ---: |
| `number_never_panics_and_fits_its_length` | `Bytes::number` on any 9 bytes never panics, consumes exactly the length its first byte gives, and returns a value that fits that length | 4 s |
| `header_readers_never_panic` | `count`, `size`, `bits` and `u32` on any 5 bytes never panic and never move past the data | 4 s |
| `chunk_headers_stay_within_lzma2_limits` | `chunk_header` on any control byte and size fields gives sizes within LZMA2's limits and the exact reset and properties flags | 1 s |
| `time_fields_never_panic_on_any_extra_field` | `exact_time` and `ntfs_time` never panic on any extra field of up to 20 bytes | 80 s |
| `dos_time_arithmetic_never_overflows` | the DOS date arithmetic never overflows for any 32-bit DOS time | 2 s |

`SegmentPlan`, which splits an LZMA2 stream into segments that decode on their own, is too heavy for Kani. Four attempts ran out of time or memory. `segment_plans_start_at_resets_and_cover_the_folder_for_every_short_stream` in `src/native_7z.rs` checks it instead on every stream of up to four chunks of every kind, every minimum size and every folder boundary, more than 600,000 cases.

## When to run it

The gate's `verify` step fails when code listed under `[parsers]` in `verification/coverage.txt` has changed since the proofs last passed. Run them then, and before changing that code on purpose.

## Run it

```sh
cargo kani
```

Kani needs `cargo install --locked kani-verifier` and `cargo kani setup` once, on macOS or Linux. Every proof must report `VERIFICATION:- SUCCESSFUL`. Then `cargo xtask verify-sync parsers` runs `cargo kani` again and records the fingerprint only if every proof passes.

## Read a result

- A failure names the check, such as `attempt to shift right with overflow`. `cargo kani --harness NAME --concrete-playback=print` prints the input that breaks it. Turn that input into a unit test, then fix the code.
- `VERIFICATION:- FAILED` with "CBMC appears to have run out of memory" is no verdict. Shrink the bound or split the code, as `SegmentPlan` was split from the file reading.
- Never weaken an assertion or narrow a bound to make a real failure pass.

## Keep proofs fast

- The readers Kani proves return `Malformed`, not `anyhow::Error`. Building an `anyhow` error reaches `catch_unwind`, which crashes Kani 0.68's compiler, and runtime `format!` in an error path made one proof run for over 34 minutes. `Malformed` holds static text, joined only when shown.
- Keep file reading outside the proven function. `independent_lzma2_segments` reads chunk headers and hands them to `chunk_header` and `SegmentPlan`, which take plain values.
- Give each loop an unwind bound one above its largest count, as `#[kani::unwind(11)]` does for nine bits.

## Proof that the proofs can fail

| Change | What Kani reports |
| --- | --- |
| `Bytes::number` computes its mask as `0xFF >> (extra + 1)` on a `u8`, the bug the 7z review found | `attempt to shift right with overflow` |
| `ntfs_time` reads a time without checking that the attribute holds 8 bytes | a failed slice bounds check |
