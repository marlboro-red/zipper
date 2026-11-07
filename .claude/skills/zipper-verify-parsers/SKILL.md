---
name: zipper-verify-parsers
description: Prove with Kani that Zipper's byte-level parsers never panic or overflow on any input up to a bound. Use before committing any change to the 7z header readers (Bytes, Malformed, check), the LZMA2 chunk header and segment plan, the ZIP time fields, end record, ZIP64 field or data descriptor in src/native_zip.rs, the tar numeric fields (octal, number, size_field) in src/native_tar.rs, the ZIP LZMA and PPMd headers and 7z PPMd properties, the BCJ2 decoder in src/bcj2.rs, the BCJ2 folder graph (`bcj2_plan`) or the 7z byte-swap reader (`ByteSwap`), the LZ4 block decoder or frame reader in src/lz4.rs, the `compress` reader in src/lzw.rs, Zipper's own LZMA, LZMA2 and xz decoder in src/lzma.rs, the cpio and `ar` header and field readers in src/native_cpio.rs and src/native_ar.rs, the RPM lead, header and index readers in src/native_rpm.rs, and when a parser panics or overflows on hostile input.
---

# Verify the parsers with Kani

A parser that panics or overflows on a crafted archive crashes a library caller or, in release builds, misreads the archive silently. Tests and fuzzing try many inputs. Kani compiles the real Rust code into a logic problem and checks every input up to a bound, so a pass covers inputs nobody thought to write. The proofs are the `#[cfg(kani)] mod proofs` modules in `src/native_7z.rs`, `src/native_zip.rs`, `src/native_tar.rs`, `src/native_cpio.rs`, `src/native_ar.rs`, `src/native_rpm.rs`, `src/bcj2.rs`, `src/zip_directory.rs` and `src/lzma.rs`. The cpio, `ar` and RPM proofs sit under `[cpio-fields]`, `[ar-fields]` and `[rpm-fields]` in `verification/coverage.txt`, and `cargo xtask verify-sync cpio-fields`, `ar-fields` or `rpm-fields` runs them with their field tests.

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
| `newc_headers_never_panic_and_fit_their_fields` | the cpio newc reader never panics on any 110 header bytes, and every size and name size it returns fits 32 bits and pads below 4 | 54 s |
| `odc_headers_never_panic_and_fit_their_fields` | the cpio odc reader never panics on any 76 header bytes, and its size, time and name size fit their octal fields | 49 s |
| `binary_headers_never_panic_and_fit_their_fields` | the binary cpio reader never panics on any 26 header bytes in either byte order, and its fields fit their words | 0.4 s |
| `hex_and_octal_fields_never_panic_and_read_back_their_values` | the cpio hexadecimal and octal field readers never panic on any field of up to 8 bytes, and every 32-bit value reads back from its 8 hex digits | 6 s |
| `eleven_octal_digits_read_back_every_value_below_their_limit` | every value below 8^11 reads back from its 11-digit odc field | 2 s |
| `padding_aligns_every_length_of_every_form` | cpio name and data padding aligns every length in each form | 0.2 s |
| `headers_never_panic_and_their_sizes_fit_ten_digits` | the `ar` header reader never panics on any 60 bytes, and its size, time and mode fit their fields | 31 s |
| `names_never_panic_and_long_name_lengths_fit_their_field` | the `ar` name reader never panics on any 16 bytes, and BSD name lengths and GNU offsets fit their fields | 32 s |
| `every_size_reads_back_from_its_decimal_field` | every `ar` size below 10^10 reads back from its decimal field | 460 s |

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

The LZ4 reader in `src/lz4.rs`, under `[lz4-frames]`, has these exhaustive tests:

- `every_short_block_decodes_as_the_reference_does` compares `decode_block` with a reference decoder written separately. It covers every block of up to 5 bytes over 8 bytes that matter, after 0, 1 or 3 bytes of history and within limits of 3 and 64, over 200,000 cases.
- `xxh32_matches_known_values_and_every_split` checks xxHash32 against its published values for the empty string and `abc`, and on every length up to 70 bytes fed in every 3-way split.
- `frames_round_trip_and_every_damage_is_refused` cuts frames at every length and flips each byte. A frame with a content checksum must never decode to other bytes.
- `a_long_match_is_refused_before_the_output_grows_past_its_limit` covers a match that would allocate far past the block limit before the final size check.

Run them with `cargo test --release --lib lz4`, then `cargo xtask verify-sync lz4-frames`.

The `compress` (`.Z`) reader in `src/lzw.rs`, under `[compress]`, has three tests. `every_pseudo_random_stream_decodes_as_the_reference_does` compares it with a reference decoder written separately in gzip's style, on 60,000 streams in block mode and without it. `real_compress_output_decodes_at_every_code_size` decodes macOS `compress` output at 9 to 16 bits, and a 9-bit file in `tests/data/stream` must widen to 10-bit codes as 7-Zip does. `a_stream_without_clear_codes_changes_width_mid_group_as_7zip_reads_it` checks the mode that changes width inside a group of codes against 7-Zip. Run them with `cargo test --release --lib lzw`, then `cargo xtask verify-sync compress`. A larger Kani bound for the block decoder ran past 11 minutes and was dropped.

Zipper's own ZIP central-directory reader in `src/zip_directory.rs`, under `[zip-directory]`, has nine Kani proofs. `extra_fields_never_panic_and_keep_every_value_without_a_sentinel` covers extra fields of up to 20 bytes. `central_records_never_panic_and_end_inside_the_directory` covers central records of up to 54 bytes. `end_records_and_zip64_records_end_where_their_directory_ends` checks that the end and ZIP64 records place the directory exactly before them, within the entry and metadata limits. `the_end_record_is_the_last_signature_and_reaches_the_end` is the same rule as the native reader's proof of that name. The other five cover the walk through the records:

- `directory_bytes_stay_inside_the_directory_and_the_record` gives `DirectoryBytes::append` a stream of up to 4 bytes in reads of any size, any of which may fail. It holds 0 to 2 bytes already and asks for up to 5. It never reads past the remaining directory bytes, keeps the bytes it held, and grows the buffer no further than the record.
- `the_record_walk_reads_exactly_the_directory_or_refuses` runs `walk_records` on every directory of up to 100 bytes whose first two records declare at most 4 bytes of name, extra field and comment, with any count the extent check allows. Each record may be accepted or refused, but only a whole 46-byte header is accepted, as the central record proof shows. The walk never panics, reads no byte past the directory, and succeeds only after reading all of it, with exactly the declared count of entries.
- `every_short_record_becomes_an_entry_or_a_refusal` runs `central_entry` on every record of up to 50 bytes and any position. It never panics, and an entry it accepts has its name inside the record and a local header offset that fits.
- `unicode_names_never_panic_and_rename_only_when_the_checksum_matches` covers extra fields of up to 12 bytes and stored names of up to 2. A rename always comes from a Unicode path field whose checksum matches the stored name, and spells the stored name in CP437.
- `unix_modes_never_panic_or_keep_special_bits` covers every external attributes value and version made by.

Two stand-ins keep these proofs within Kani's reach. Kani 0.68's compiler panics on crc32fast's Arm CRC32 instructions, so under Kani `name_checksum` uses a table CRC-32. `the_proofs_checksum_matches_crc32fast` checks that table against crc32fast on every input of 1 and 2 bytes. Std's UTF-8 check ran for over 10 minutes on 6 bytes, so the record and Unicode proofs let each UTF-8 check give any answer. That covers every answer std could give. A plant that drops the checksum check, the final count check, the limit to the remaining bytes or the clearing of the special bits makes the matching proof fail. A differential test compares the reader with the `zip` crate on the corpora, hand-built shapes and damaged archives. It alone covers `read_directory`, the real UTF-8 check, records longer than 50 bytes and the CP437 table's contents. Run `cargo xtask verify-sync zip-directory`, which runs the proofs itself.

The xz filters in `src/xz_filters.rs`, under `[xz-filters]`, have exhaustive tests against liblzma, and the 7z reader decodes its branch and Delta filters with them. Each of the eight branch filters and Delta round-trips every short input under liblzma's encoder and decodes as liblzma does at every split and several start offsets, and `every_ia64_template_and_slot_mix_decodes_as_liblzma_does` covers all 32 IA-64 templates. Chains and their refusals are compared with liblzma in `src/lzma.rs`. Run `cargo xtask verify-sync xz-filters`, which runs `cargo test --release --lib xz_filters` itself. Under `[7z-filters]`, nine Kani proofs in `xz_filters::proofs` cover two calls of up to 12 bytes, 17 for IA-64, from any start position. Each branch filter never panics, converts only a prefix and moves its position by exactly what it converted. Delta adds the byte its distance back, for every distance from 1 to 256. `native_7z::proofs::filter_properties_are_read_as_7zip_reads_them` checks every 7z filter ID and property of up to 5 bytes against 7-Zip's rules. Run `cargo xtask verify-sync 7z-filters`.

Zipper's own LZMA decoder in `src/lzma.rs` has 18 Kani proofs in seven groups, each its own section in `verification/coverage.txt`. 7z decodes LZMA and LZMA2 with it, but its xz reader is built only for tests and needs the `sha2` dev-dependency, so these proofs run with `cargo kani --tests`. Each proof gives the range coder a symbolic range of at least 2^24, a symbolic code and as many symbolic input bytes as it may read, so a read past them fails the proof. Probability tables start at one symbolic value from 31 to 2017, the values a probability can reach from 1024. The proofs named `..._decode_as_one_bit_at_a_time_...` instead give every entry its own symbolic value and compare the pipelined kernel with a one-bit-at-a-time reference. The window contents are never symbolic, because a proof of the match copy ran out of memory.

Each group lists only the items its proofs reach, method by method, with the proofs and their helpers. An edit reruns only the groups whose items it touches. A change to `decode`, `Window`, the LZMA2 chunk reader or the xz reader reruns no Kani proof, only the `[lzma]` tests. A change to `bit_value`, `normalize`, `byte` or a shared constant such as `TOP` reruns every group except `[lzma-input]`.

| Group | Its proofs cover | Kani time |
| --- | --- | ---: |
| `[lzma-bits]` | `bit_value`, `bit`, `branching_bit`, `normalize`, `byte`, `direct_bits`, `parts` | 14 s |
| `[lzma-trees]` | `tree` and `reverse_tree` | 1,238 s |
| `[lzma-lengths]` | `LengthDecoder::decode` | 48 s |
| `[lzma-literal]` | `Properties::from_byte`, `literal_context` and `literal` | 367 s |
| `[lzma-matched-literal]` | `matched_literal` and `matched_literal_index` | 2,393 s |
| `[lzma-distances]` | `distance_from_slot`, with `LengthDecoder::new` to build its tables | 282 s |
| `[lzma-input]` | `RangeDecoder::bits`, `consumed`, `limited_end`, `complete`, `keep` and the `Bits` input checks | 2 s |

| Proof | Group | It proves, for every input in its bound | Time |
| --- | --- | --- | ---: |
| `both_bit_decoders_agree_and_keep_the_coder_in_range` | bits | on any range, code and probability up to 2048, the branchless and branching bit decoders return the same bit, range, code, probability and position, and read at most one byte. From a range of at least 2^24 and a probability from 31 to 2017, both stay in those bounds, and a code below the range stays below it | 11 s |
| `branching_bits_normalized_in_each_arm_match_one_normalization_after_both` | bits | the branching bit decoder, which normalizes inside each arm, gives the same bit, probability, range, code and position as one normalization after both arms | 2 s |
| `direct_bits_fit_their_count_and_keep_the_range_normalized` | bits | `direct_bits` of up to 26 bits returns a value below 2^count, reads at most one byte a bit and keeps the range at least 2^24 | 1 s |
| `every_bit_tree_size_stays_inside_its_table` | trees | the 3, 6 and 8-bit trees and the 4-bit reverse tree index only their own table, read at most one byte a bit and return a value below 2^bits | 41 s |
| `length_trees_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | trees | the 3-bit tree, with any probability in every entry, returns the reference's value, coder state and table | 39 s |
| `slot_trees_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | trees | the same for the 6-bit slot tree | 367 s |
| `high_length_trees_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | trees | the same for the 8-bit tree | 475 s |
| `reverse_trees_decode_as_one_bit_at_a_time_reading_no_entry_past_their_bits` | trees | reverse trees of 1 to 5 bits return the reference's value, coder state and table | 315 s |
| `lengths_stay_below_the_longest_match_at_every_position_state` | lengths | the length decoder, at each of the 16 position states, reads at most 10 bytes and returns less than 272, so no match is longer than 273 bytes | 48 s |
| `literal_contexts_stay_inside_the_literal_table_for_every_property_byte` | literal | every properties byte `Properties::from_byte` accepts has `lc` up to 8 and `lp` and `pb` up to 4, and for any position and previous byte the literal context is below 2^(lc+lp), so it selects a coder inside the table `reset` allocates. The position state is below 16 | 0.1 s |
| `plain_literals_stay_inside_the_first_quarter_of_their_coder` | literal | a literal without a match byte reads only the first 256 probabilities and 8 bytes, and gives a symbol from 0x100 to 0x1FF | 23 s |
| `plain_literals_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | literal | a plain literal, with any probability in every entry, returns the reference's symbol, coder state and table | 344 s |
| `matched_literals_stay_inside_their_coder_for_every_match_byte` | matched literal | a literal after a match, for any match byte, reads only its 768 probabilities and 8 bytes, and gives a symbol from 0x100 to 0x1FF | 1,079 s |
| `matched_literals_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | matched literal | a matched literal, with any probability in every entry and any match byte, returns the reference's symbol, coder state and table | 1,313 s |
| `matched_literal_children_stay_in_their_coder_apart_from_the_entry_they_follow` | matched literal | both children `matched_literal_index` gives for a step lie inside the 768 entries and differ from the entry they follow | 0.1 s |
| `short_distances_stay_inside_the_special_probabilities` | distances | for slots 4 to 13, `distance_from_slot` reads inside the 115 special probabilities and at most 5 bytes | 50 s |
| `long_distances_fit_32_bits` | distances | for slots 14 to 63, `distance_from_slot` reads at most 30 bytes, stays inside the align table and returns a distance that fits 32 bits | 232 s |
| `input_limits_keep_every_symbol_inside_the_buffer_and_the_chunk` | input | for any input buffer state and LZMA2 input limit, the room `bits` gives fits the buffer, a symbol of up to 64 bytes started when `needs_input` is false stays in it, and a decode that passes `ran_past_input` keeps the start before the end and the consumed input within the limit. `finished_cleanly` is exactly a zero code | 2 s |

Times are from one `verify-sync` run of each group on an Apple M5 shared with other jobs. The heavy proofs are the two matched literal proofs, the three pipelined tree proofs with 6 to 8 bits or reverse trees, and the plain literal reference proof. Together the proofs bound one symbol's input. A match reads at most 1 + 1 + 10 + 6 + 26 + 4 = 48 bytes, a repeated match 15 and a literal 9. All fit the 64 bytes `needs_input` keeps in hand. That sum is argued here, not proved, because `decode_symbols` as one proof would need a symbolic window. The slot tree is the 6-bit tree, and `decode_distance` adds only the `length_state` clamp to the two distance proofs.

Each group's first full check, `cargo xtask kani-reach NAME`, keeps its item list honest. It runs Kani's code generation for the group's proofs and lists every source line in each proof's goto program with `goto-instrument --list-eloc`. It fails when a reached line lies outside the group's items, and names the item to add. A reached line that lies in no function's span is reported as a cut span. It honours `CARGO_TARGET_DIR`.

Code evaluated while compiling leaves no line in the goto program, so the check also follows names. It requires every constant, static, struct, enum, type, trait, macro and `const fn` that a reached function, its `impl` header or one of those definitions names. Inside a constant, a static, a type or a `const fn`, it also requires every function the code calls. It follows `crate::` paths and `use crate::...` imports, nested brace groups and `as` aliases included, into other files, whose items the group must then list. Strings and comments do not count. It refuses any form it cannot follow to a definition, and names the line. Those forms are a module imported and then used as a path, `Type::NAME` on a type from another file, a re-export, a `use crate::...::*`, `include!` and a `super::` path that leaves the file. A `crate::` path or import that resolves to nothing is refused too. An edit that adds a call from a listed item changes that item, so the group goes stale and its next `verify-sync` runs the check again.

The groups set `item-end = semicolon outside brackets`, so an item whose signature holds `[u8; N]` is fingerprinted to its closing brace. The older sections end an item at its first `;` and so fingerprint such a function only up to that `;`. The ignored test `print_items_cut_short_at_a_bracketed_semicolon` in `xtask/src/verify.rs` lists those items. `each_lzma_edit_makes_stale_only_the_groups_whose_proofs_reach_it` in the same file checks which groups an edit to each kernel makes stale. An attribute line above an item, such as `#[kani::unwind(28)]` above a proof, lies outside the item and is not fingerprinted.

The gate's `verify` step, which `cargo xtask verify` runs alone, finds every `#[kani::proof]` in `src/` from the real module nesting, under any visibility and past comments and other attributes. It fails when any proof runs in no full check, and when a proof in a module that a section names with `--exact` runs in more than one. A proof under any cfg other than `#[cfg(kani)]`, or `#[cfg(test)]` with `--tests`, fails it by name. So does any attribute that names `proof` other than `#[kani::proof]` alone on its own line outside any macro, such as `#[proof]`, `#[::kani::proof]`, `#[kani::proof_for_contract]` or `#[cfg_attr(..., kani::proof)]`, and any `use kani::proof` or `use kani::*`.

The text checks can miss a form nobody thought of, so `cargo xtask release-checks` adds two checks that read what Kani compiles. One compares that list of proofs with `cargo kani list` and `cargo kani --tests list`. The other compares each proof's goto program with its `goto-fingerprint` line in `verification/coverage.txt`, which `verify-sync` writes when the proofs pass. The fingerprint hashes the output of `goto-instrument --show-goto-functions` with source locations, crate hashes and allocation numbers left out, together with the proof's Kani attributes, such as its unwind bound, and the `cargo kani` flags. Constants are folded into the program, so an edit that changes what a proof checks changes its fingerprint, whatever Rust form caused it, and a blank line does not. Two runs of the same tree, one from an empty target folder, gave the same 18 fingerprints. Fingerprinting all 18 lzma proofs took 79 s with a warm build. `cargo xtask goto-fingerprints` runs that comparison alone, and `cargo xtask goto-fingerprints NAME` prints a group's lines. The gate's `verify` step fails when a group's `goto-fingerprint` lines do not name exactly its proofs.

Exhaustive and differential tests cover the rest. `chunked_match_copy_matches_a_byte_at_a_time_copy_in_every_window_state` checks `Window::copy` against a byte-at-a-time copy in every window state of four sizes. The liblzma differential tests cover every `lc`, `lp` and `pb`, every cut stream, LZMA2 control codes and properties, and the xz reader's fields, cuts and damage. Kani does not cover `RangeDecoder::start`, `refill`, `Window`, `decode_symbols`, the LZMA2 chunk reader or the xz reader. `[lzma]` lists the whole decoder, and `cargo xtask verify-sync lzma` runs `cargo test --release --lib lzma` over it. `cargo xtask verify-sync lzma-bits`, or another group's name, runs that group's reach check and proofs and records its fingerprint only if both pass.

## When to run it

The gate's `verify` step fails when code listed under `[parsers]`, `[lzma]`, an `[lzma-...]` group or another section that names this skill in `verification/coverage.txt` has changed since the proofs last passed. Run them then, and before changing that code on purpose.

## Run it

```sh
cargo xtask verify-sync parsers
```

Kani needs `cargo install --locked kani-verifier` and `cargo kani setup` once, on macOS or Linux. Every proof must report `VERIFICATION:- SUCCESSFUL`. `[parsers]` runs `cargo kani` with a `--harness` filter for each module's `proofs` except `lzma::proofs`, whose proofs run only from their groups. It records the fingerprint only if every proof passes. A module with new proofs needs its filter added there, or the gate fails.

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
| The `.Z` reader starts a 9-bit stream at the 512-code limit, so it never widens | the 9-bit tests fail, as the second review found |
| The `.Z` reader skips the empty entry a clear code leaves | the reference test and the `compress` test fail |
| The `.Z` reader drops the case of a code equal to the next free entry | both fail |
| The stored LZMA2 writer writes a chunk's size without subtracting one | the same test fails |
| The BCJ2 decoder converts a branch with fewer than 4 bytes of output left | `a_conversion_past_the_end_of_the_output_is_refused` fails |
| The LZMA range coder normalizes only below 2^23 | `both_bit_decoders_agree_and_keep_the_coder_in_range` fails |
| `direct_bits` shifts its value by two bits a step | `direct_bits_fit_their_count_and_keep_the_range_normalized` fails |
| `tree` subtracts `1 << (bits - 1)` from its index | `every_bit_tree_size_stays_inside_its_table` fails |
| The length decoder adds 17 to a high length | `lengths_stay_below_the_longest_match_at_every_position_state` fails |
| The literal context shifts the position by `lc + 1` | `literal_contexts_stay_inside_the_literal_table_for_every_property_byte` fails |
| `Properties::from_byte` accepts the byte 225 | the same proof fails |
| A plain literal decodes until its symbol reaches 0x200 | `plain_literals_stay_inside_the_first_quarter_of_their_coder` fails |
| A matched literal ORs its offset mask instead of ANDing it | `matched_literals_stay_inside_their_coder_for_every_match_byte` fails |
| A short distance starts its special probabilities one entry too far on | `short_distances_stay_inside_the_special_probabilities` fails |
| A long distance shifts its direct bits two places too far | `long_distances_fit_32_bits` fails |
| `bits` gives one byte more room than the padding past the end | `input_limits_keep_every_symbol_inside_the_buffer_and_the_chunk` fails |
| The cpio `hex` reader accepts base-17 digits | `newc_headers_never_panic_and_fit_their_fields` fails |
| The `ar` field reader accepts a digit equal to its radix | `headers_never_panic_and_their_sizes_fit_ten_digits` fails |
