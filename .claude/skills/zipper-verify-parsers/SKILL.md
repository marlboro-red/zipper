---
name: zipper-verify-parsers
description: Prove with Kani that Zipper's byte-level parsers never panic or overflow on any input up to a bound. Use before committing any change to the 7z header readers (Bytes, Malformed, check), the LZMA2 chunk header and segment plan, the ZIP time fields, end record, ZIP64 field or data descriptor in src/native_zip.rs, the tar numeric fields (octal, number, size_field) in src/native_tar.rs, the ZIP LZMA and PPMd headers and 7z PPMd properties, the BCJ2 decoder in src/bcj2.rs, the BCJ2 folder graph (`bcj2_plan`) or the 7z byte-swap reader (`ByteSwap`), the LZ4 block decoder or frame reader in src/lz4.rs, the `compress` reader in src/lzw.rs, and when a parser panics or overflows on hostile input.
---

# Verify the parsers with Kani

A parser that panics or overflows on a crafted archive crashes a library caller or, in release builds, misreads the archive silently. Tests and fuzzing try many inputs. Kani compiles the real Rust code into a logic problem and checks every input up to a bound, so a pass covers inputs nobody thought to write. The proofs are the `#[cfg(kani)] mod proofs` modules in `src/native_7z.rs`, `src/native_zip.rs`, `src/native_tar.rs` and `src/bcj2.rs`.

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
| `lzma_headers_keep_the_properties_and_never_shrink_the_dictionary_below_need` | `lzma_alone_header` keeps the ZIP LZMA properties byte and the size, and caps the dictionary no lower than the entry or the declared dictionary needs | 3 s |
| `ppmd_headers_stay_inside_the_decoders_bounds` | every ZIP PPMd header `ppmd8_fields` accepts has an order, model size and restore method the decoder accepts, within its 256 MiB reservation | 1 s |
| `ppmd_properties_read_only_five_bytes` | `ppmd7_fields` reads exactly five 7z PPMd property bytes and nothing else | 2 s |
| `decoding_any_short_block_never_panics_or_passes_its_limit` | the LZ4 block decoder never panics on any block of up to 4 bytes after 0 or 1 byte of history, and never writes past its limit | 119 s |
| `decoding_any_short_streams_never_panics` | the BCJ2 decoder never panics or overflows on any main, call and jump streams of 4 bytes, range coder stream of 6 bytes and output size up to 8 | 46 s |

Some checks are too heavy for Kani, so exhaustive unit tests cover them:

- `record_limits` sorts, which is heavy for Kani. `record_limits_accept_exactly_the_layouts_without_overlap` in `src/native_zip.rs` compares it with a plain reference on every layout of up to 3 records, about 125,000 cases.
- `checksum_matches` over 512 symbolic bytes ran past 300 s. It reads its field with `octal`, which the proofs cover, and sums at most 512 bytes of 255.
- The tar pax readers and the path and symlink escape checks in `src/lib.rs` use std's string search, which ran past 300 s on 2-byte inputs. `pax_times_equal_their_decimal_value_on_every_short_string`, `pax_records_split_back_into_their_exact_bytes_on_every_short_string` and the two escape tests in `src/lib.rs` check every short string instead.

`SegmentPlan`, which splits an LZMA2 stream into segments that decode on their own, is too heavy for Kani. Four attempts ran out of time or memory. `segment_plans_start_at_resets_and_cover_the_folder_for_every_short_stream` in `src/native_7z.rs` checks it instead on every stream of up to four chunks of every kind, every minimum size and every folder boundary, more than 600,000 cases.

The BCJ2 folder graph is checked by exhaustive tests, under `[7z-bcj2-graph]` in `verification/coverage.txt`:

- `every_small_folder_graph_plans_only_sound_bcj2_branches` runs `bcj2_plan` on every folder of up to 3 coders with 4-to-1, 1-to-1 or 2-to-1 streams, every list of bind pairs and every list of packed inputs, over 1,000,000 shapes. Each plan it accepts must give BCJ2's four inputs chains that follow real bind pairs to distinct packed streams, use every coder and packed stream once, and leave BCJ2's output unbound. The oracle recomputes the stream numbering on its own.
- `every_bcj2_layout_with_up_to_four_chained_coders_is_planned` builds every valid layout of BCJ2 and up to four chained coders, at every position and in rotated orders, and checks that the plan finds exactly those chains.
- `every_short_input_round_trips_under_every_conversion_choice` in `src/bcj2.rs` encodes every input of up to 5 bytes over the bytes that BCJ2 treats specially, under every choice of conversions. The decoder must return the input and read every stream to its end, in reads of 1, 3 and 64 bytes.

Run them with `cargo test --release --lib bcj2`, then `cargo xtask verify-sync 7z-bcj2-graph`.

`byte_swap_reverses_every_whole_group_under_every_read_split`, under `[7z-byte-swap]`, checks the Swap2 and Swap4 reader on every length up to 13 bytes. The inner reader yields 1 to 6 bytes at a time, the caller reads 1 to 6, and the buffer holds 1, 4, 5, 7 or 64 bytes, 5,040 runs in all. Run it with `cargo test --release --lib byte_swap`, then `cargo xtask verify-sync 7z-byte-swap`.

`stored_lzma2_returns_every_length_across_chunk_boundaries_and_flushes_branch_filters`, under `[7z-stored-lzma2]`, wraps data of 0 to 5 bytes and lengths around the 64 KiB chunk limit in stored LZMA2 chunks, read 1, 3, 65,536 or any number of bytes at a time. liblzma must decode the chunks to the same bytes, and an x86 filter over them must release every byte. Run it with `cargo test --release --lib stored_lzma2`, then `cargo xtask verify-sync 7z-stored-lzma2`.

The LZ4 reader in `src/lz4.rs`, under `[lz4-frames]`, has these exhaustive tests:

- `every_short_block_decodes_as_the_reference_does` compares `decode_block` with a reference decoder written separately. It covers every block of up to 5 bytes over 8 bytes that matter, after 0, 1 or 3 bytes of history and within limits of 3 and 64, over 200,000 cases.
- `xxh32_matches_known_values_and_every_split` checks xxHash32 against its published values for the empty string and `abc`, and on every length up to 70 bytes fed in every 3-way split.
- `frames_round_trip_and_every_damage_is_refused` cuts frames at every length and flips each byte. A frame with a content checksum must never decode to other bytes.
- `a_long_match_is_refused_before_the_output_grows_past_its_limit` covers a match that would allocate far past the block limit before the final size check.

Run them with `cargo test --release --lib lz4`, then `cargo xtask verify-sync lz4-frames`.

The `compress` (`.Z`) reader in `src/lzw.rs`, under `[compress]`, has three tests. `every_pseudo_random_stream_decodes_as_the_reference_does` compares it with a reference decoder written separately in gzip's style, on 60,000 streams in block mode and without it. `real_compress_output_decodes_at_every_code_size` decodes macOS `compress` output at 10 to 16 bits. `a_stream_without_clear_codes_changes_width_mid_group_as_7zip_reads_it` checks the mode that changes width inside a group of codes against 7-Zip. Run them with `cargo test --release --lib lzw`, then `cargo xtask verify-sync compress`. A larger Kani bound for the block decoder ran past 11 minutes and was dropped.

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
| `lzma_alone_header` caps every dictionary at 4096 | `lzma_headers_keep_the_properties_and_never_shrink_the_dictionary_below_need` fails |
| `ppmd8_fields` drops the smallest-order check | `ppmd_headers_stay_inside_the_decoders_bounds` fails |
| `ppmd7_fields` accepts more than five bytes | `ppmd_properties_read_only_five_bytes` fails |
| The BCJ2 decoder computes `self.position - self.size` | `decoding_any_short_streams_never_panics` reports an arithmetic overflow |
| `bcj2_plan` drops the check that a coder is used twice | `every_small_folder_graph_plans_only_sound_bcj2_branches` never ends, because a cycle loops |
| `bcj2_plan` drops the check that every coder is used | `every_small_folder_graph_plans_only_sound_bcj2_branches` fails |
| `bcj2_plan` maps an output to the first coder whose start is below it | `every_bcj2_layout_with_up_to_four_chained_coders_is_planned` fails |
| `bcj2_plan` takes a branch's size from its last coder, not its first | both plan tests fail |
| The byte-swap reader also reverses a tail shorter than its group | `byte_swap_reverses_every_whole_group_under_every_read_split` fails |
| The byte-swap reader stops at the end of its input without passing on the unswapped tail | the same test fails |
| The byte-swap reader keeps reading into its buffer without moving the held bytes to the front | the same test fails |
| `decode_block` drops the check that a match offset lies inside the output | the Kani proof fails, and the exhaustive test fails |
| `decode_block` drops the check that a match stays inside the limit | `a_long_match_is_refused_before_the_output_grows_past_its_limit` fails |
| An independent LZ4 block keeps the previous block as history | `linked_blocks_reach_back_into_the_previous_block` fails |
| The `.Z` reader keeps reading a group after the code width grows | the test against 7-Zip fails |
| The `.Z` reader skips the empty entry a clear code leaves | the reference test and the `compress` test fail |
| The `.Z` reader drops the case of a code equal to the next free entry | both fail |
| The stored LZMA2 writer leaves out the end marker | `stored_lzma2_returns_every_length_across_chunk_boundaries_and_flushes_branch_filters` fails |
| The stored LZMA2 writer writes a chunk's size without subtracting one | the same test fails |
| The BCJ2 decoder converts a branch with fewer than 4 bytes of output left | `a_conversion_past_the_end_of_the_output_is_refused` fails |
