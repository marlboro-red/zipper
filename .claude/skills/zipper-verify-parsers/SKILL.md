---
name: zipper-verify-parsers
description: Prove with Kani that Zipper's byte-level parsers never panic or overflow on any input up to a bound. Use before committing any change to the 7z header readers (Bytes, Malformed, check), the LZMA2 chunk header and segment plan, the ZIP time fields, end record, ZIP64 field or data descriptor in src/native_zip.rs, or the tar numeric fields (octal, number, size_field) in src/native_tar.rs, and when a parser panics or overflows on hostile input.
---

# Verify the parsers with Kani

A parser that panics or overflows on a crafted archive crashes a library caller or, in release builds, misreads the archive silently. Tests and fuzzing try many inputs. Kani compiles the real Rust code into a logic problem and checks every input up to a bound, so a pass covers inputs nobody thought to write. The proofs are the `#[cfg(kani)] mod proofs` modules in `src/native_7z.rs`, `src/native_zip.rs` and `src/native_tar.rs`.

## The proofs

| Proof | It proves, for every input in its bound | Time |
| --- | --- | ---: |
| `number_never_panics_and_fits_its_length` | `Bytes::number` on any 9 bytes never panics, consumes exactly the length its first byte gives, and returns a value that fits that length | 4 s |
| `header_readers_never_panic` | `count`, `size`, `bits` and `u32` on any 5 bytes never panic and never move past the data | 4 s |
| `chunk_headers_stay_within_lzma2_limits` | `chunk_header` on any control byte and size fields gives sizes within LZMA2's limits and the exact reset and properties flags | 1 s |
| `time_fields_never_panic_on_any_extra_field` | `exact_time` and `ntfs_time` never panic on any extra field of up to 20 bytes | 80 s |
| `dos_time_arithmetic_never_overflows` | the DOS date arithmetic never overflows for any 32-bit DOS time | 2 s |
| `numeric_fields_never_panic_on_any_bytes` | `octal` and `size_field` never panic on any field of 1 to 12 bytes, and a size always fits an `i64` | 14 s |
| `every_value_reads_back_from_its_encoding` | every `i64` reads back from its 12-byte base-256 field, and every value below 8^11 from its octal field | 9 s |
| `base_256_fields_have_one_spelling` | a base-256 field that `number` accepts is the one encoding of its value | 4 s |
| `the_end_record_is_the_last_signature_and_reaches_the_end` | on any ZIP tail of up to 26 bytes, the end record `end_record` accepts has its signature, its comment reaches the end, and no signature follows it | 26 s |
| `zip64_fields_never_panic_and_fit_their_field` | `single_zip64_field` never panics on any extra field of up to 20 bytes and returns no more values than fit | 27 s |
| `every_descriptor_layout_reads_exactly_its_bytes` | each data descriptor length `descriptor_layout` accepts is read in full by `descriptor_values`, inside its bytes | 2 s |

Some checks are too heavy for Kani, so exhaustive unit tests cover them:

- `record_limits` sorts, which is heavy for Kani. `record_limits_accept_exactly_the_layouts_without_overlap` in `src/native_zip.rs` compares it with a plain reference on every layout of up to 3 records, about 125,000 cases.
- `checksum_matches` over 512 symbolic bytes ran past 300 s. It reads its field with `octal`, which the proofs cover, and sums at most 512 bytes of 255.
- The tar pax readers and the path and symlink escape checks in `src/lib.rs` use std's string search, which ran past 300 s on 2-byte inputs. `pax_times_equal_their_decimal_value_on_every_short_string`, `pax_records_split_back_into_their_exact_bytes_on_every_short_string` and the two escape tests in `src/lib.rs` check every short string instead.

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
| `number` drops the `i64::MIN >> 8` check before a negative base-256 shift | `base_256_fields_have_one_spelling` fails |
| `octal` subtracts `b'1'` instead of `b'0'` | `every_value_reads_back_from_its_encoding` fails |
| `octal` slices `digits[end + 1..]` | a failed slice bounds check |
| `end_record` skips the check for a second end signature after the one it picks | `the_end_record_is_the_last_signature_and_reaches_the_end` fails |
| `descriptor_layout` marks the 20-byte layout as signed | `every_descriptor_layout_reads_exactly_its_bytes` fails |
