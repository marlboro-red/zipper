---
name: zipper-verify-parsers
description: Prove with Kani that Zipper's byte-level parsers never panic or overflow on any input up to a bound. Use before committing any change to the 7z header readers (Bytes, Malformed, check), the LZMA2 chunk header and segment plan, the ZIP time fields, end record, ZIP64 field or data descriptor in src/native_zip.rs, the tar numeric fields (octal, number, size_field) in src/native_tar.rs, the ZIP LZMA and PPMd headers and 7z PPMd properties, the BCJ2 decoder in src/bcj2.rs, the BCJ2 folder graph (`bcj2_plan`) or the 7z byte-swap reader (`ByteSwap`), the LZ4 block decoder or frame reader in src/lz4.rs, the `compress` reader in src/lzw.rs, Zipper's own LZMA, LZMA2 and xz decoder in src/lzma.rs, the cpio and `ar` header and field readers in src/native_cpio.rs and src/native_ar.rs, the Intel HEX record reader in src/ihex.rs, the RPM lead, header and index readers in src/native_rpm.rs, the PE, COFF, TE, ELF, Mach-O and universal header readers in src/native_exe.rs, the SquashFS and CramFS superblock, inode and folder readers in src/native_squashfs.rs and src/native_cramfs.rs, the LZO1X decoder in src/lzo.rs, the raw Deflate decoder in src/inflate.rs, the zlib and gzip framing in src/framing.rs, the BZip2 block scanner, block decoder and block reader in src/bzip2.rs and src/bzip2_streams.rs, Zipper's own Zstandard decoder in src/zstandard.rs, and when a parser panics or overflows on hostile input.
---

# Verify the parsers with Kani

A parser that panics or overflows on a crafted archive crashes a library caller or, in release builds, misreads the archive silently. Tests and fuzzing try many inputs. Kani compiles the real Rust code into a logic problem and checks every input up to a bound, so a pass covers inputs nobody thought to write. The proofs are the `#[cfg(kani)] mod proofs` modules in `src/native_7z.rs`, `src/native_zip.rs`, `src/native_tar.rs`, `src/native_cpio.rs`, `src/native_ar.rs`, `src/native_rpm.rs`, `src/bcj2.rs`, `src/lz4.rs`, `src/zip_directory.rs`, `src/zip_crypto.rs`, `src/volumes.rs`, `src/xz_filters.rs`, `src/lzma.rs`, `src/inflate.rs` and `src/framing.rs`. Each proof runs in exactly one section of `verification/coverage.txt`, and each section lists only the code its proofs reach. So an edit reruns only the proofs that reach it. The cpio, `ar` and RPM proofs sit under `[cpio-fields]`, `[ar-fields]` and `[rpm-fields]`, and `cargo xtask verify-sync cpio-fields`, `ar-fields` or `rpm-fields` runs them with their field tests.

| Section | Its proofs | Kani time |
| --- | --- | ---: |
| `[7z-header-numbers]` | `number_never_panics_and_fits_its_length`, `header_readers_never_panic` | 3 s |
| `[7z-lzma2-chunks]` | `chunk_headers_stay_within_lzma2_limits`, with the `SegmentPlan` test | 0.1 s |
| `[7z-ppmd-properties]` | `ppmd_properties_read_only_five_bytes` | 0.1 s |
| `[7z-filters]` | the nine `xz_filters` proofs and `filter_properties_are_read_as_7zip_reads_them` | 245 s |
| `[7z-aes-properties]` | `aes_properties_never_panic_and_keep_their_salt_iv_and_cycles` | 5 s |
| `[cab-fields]` | the six CAB header, folder, file record, data block, checksum and DOS time proofs | a few seconds each |
| `[zip-legacy-methods]` | `shrink::proofs::any_short_code_stream_stays_inside_the_table` and `implode::proofs::any_short_tree_description_builds_a_decodable_tree_or_is_refused` | about 3 min each |
| `[zip-extra-times]` | `time_fields_never_panic_on_any_extra_field` | 128 s |
| `[zip-dos-times]` | `dos_time_arithmetic_never_overflows` | 0.4 s |
| `[zip64-extra-field]` | `zip64_fields_never_panic_and_fit_their_field` | 36 s |
| `[zip-data-descriptors]` | `every_descriptor_layout_reads_exactly_its_bytes` | 0.4 s |
| `[zip-lzma-headers]` | `lzma_headers_keep_the_properties_and_never_shrink_the_dictionary_below_need` | 0.4 s |
| `[zip-ppmd-headers]` | `ppmd_headers_stay_inside_the_decoders_bounds` | 0.1 s |
| `[tar-numbers]` | the three `native_tar` proofs | 25 s |
| `[bcj2-decoder]` | `decoding_any_short_streams_never_panics` | 422 s |
| `[lz4-blocks]` | `decoding_any_short_block_never_panics_or_passes_its_limit` | 136 s |
| `[zip-directory]` | the ten `zip_directory` proofs | 706 s |
| `[zip-crypto]`, `[split-volumes]`, `[cpio-fields]`, `[ar-fields]`, `[rpm-fields]` | each module's whole `proofs` | unchanged |
| `[xar-header]` | the module's whole `proofs` | a few seconds |
| `[quantum]` | the `quantum` proofs, with the CAB corpus and hostile tests | a few seconds |
| `[iso-fields]` | the `native_iso` and `udf` proofs | a few seconds each |
| `[wim-fields]` | the `native_wim` and `xpress` proofs, with the WIM corpus and hostile tests | a few seconds each |
| `[lzh-arj-fields]` | the `lzh` and `native_arj` proofs | a few seconds each |
| `[legacy-streams]` | the `mslz`, `lzma86` and `pmd` proofs; marked `speed = slow` | 343 s for the SZDD decoder |
| `[swf-fields]` | the `native_swf` and `native_flv` proofs, with the FLV and SWF corpus and hostile tests | under a second each |
| `[exe-fields]` | the `native_exe` and `native_exe::resources` proofs, with the executable corpus and hostile tests | a few seconds each |
| `[squashfs-fields]` | the four `native_squashfs::field_proofs` proofs, with the SquashFS corpus and hostile tests | 2 s for the slowest |
| `[cramfs-fields]` | the two `native_cramfs` proofs, with the CramFS corpus and hostile tests | 1 s each |
| `[lzo]` | `decoding_any_short_stream_never_panics_or_passes_its_block`, with the `lzop` tests in `cargo test --release --lib lzo` | 153 s |
| `[ext-fields]` | the nine `native_ext` proofs, with the ext corpus and hostile tests and the spread flip sweeps against 7-Zip | 36 s for the slowest |
| `[inflate]` | the three `inflate` proofs, with the zlib differential tests in `cargo test --release --lib inflate` | 2 s for the slowest |
| `[ihex-fields]` | the two `ihex` proofs, with the exhaustive hex digit test and the Intel HEX corpus and hostile tests against 7-Zip | 29 s for the slowest |
| `[bzip2-fields]` | the seven `bzip2` proofs | 389 s; marked `speed = slow` |
| `[bzip2-blocks]` | no Kani proof; the TLA+ model in `verification/bzip2-blocks/`, `cargo test --release --lib bzip2` and `tests/bzip2_blocks.rs` | about 2 minutes |
| `[deflate-framing]` | `gzip_headers_read_alike_in_any_split_and_end_where_their_fields_end`, with the flate2 differential tests in `cargo test --release --lib framing::` | 115 s |
| `[zstd-tables]` | the three `zstandard` proofs | 16 s for the slowest |
| `[zstd]` | no Kani proof; `cargo test --release --lib zstandard`, with libzstd as the oracle | tests only |
| `[refused-signatures]` | the three `refused` proofs, with `tests/refused.rs` | not yet timed here |
| `[lzma-...]` | the 18 `lzma` proofs, in the seven groups below | see below |

Times are the CBMC time of one `verify-sync` on an Apple M5 shared with other jobs.

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
| `decoding_any_short_streams_never_panics` | the BCJ2 decoder never panics or overflows on any main, call and jump streams of 4 bytes, range coder stream of 6 bytes and output size up to 8, with the main stream buffered in two slices cut at any point | 422 s |
| `newc_headers_never_panic_and_fit_their_fields` | the cpio newc reader never panics on any 110 header bytes, and every size and name size it returns fits 32 bits and pads below 4 | 54 s |
| `odc_headers_never_panic_and_fit_their_fields` | the cpio odc reader never panics on any 76 header bytes, and its size, time and name size fit their octal fields | 49 s |
| `binary_headers_never_panic_and_fit_their_fields` | the binary cpio reader never panics on any 26 header bytes in either byte order, and its fields fit their words | 0.4 s |
| `hex_and_octal_fields_never_panic_and_read_back_their_values` | the cpio hexadecimal and octal field readers never panic on any field of up to 8 bytes, and every 32-bit value reads back from its 8 hex digits | 6 s |
| `eleven_octal_digits_read_back_every_value_below_their_limit` | every value below 8^11 reads back from its 11-digit odc field | 2 s |
| `padding_aligns_every_length_of_every_form` | cpio name and data padding aligns every length in each form | 0.2 s |
| `headers_never_panic_and_their_sizes_fit_ten_digits` | the `ar` header reader never panics on any 60 bytes, and its size, time and mode fit their fields | 31 s |
| `names_never_panic_and_long_name_lengths_fit_their_field` | the `ar` name reader never panics on any 16 bytes, and BSD name lengths and GNU offsets fit their fields | 32 s |
| `every_size_reads_back_from_its_decimal_field` | every `ar` size below 10^10 reads back from its decimal field | 460 s |
| `superblocks_never_panic_and_keep_their_tables_in_order` | the SquashFS superblock reader never panics on any 96 bytes, and a superblock it accepts has a block size from 4 KiB to 1 MiB, its tables in order inside the image and its root inside the inode table | 2 s |
| `inodes_never_panic_and_keep_their_fields_in_range` | the SquashFS inode reader never panics on any base header and body of up to 40 bytes, and an inode it accepts has its type's body length, a listing that starts inside its block and a size and link target within their limits | 1 s |
| `folder_runs_and_entries_never_panic_and_stay_in_their_block` | the SquashFS folder run holds 1 to 256 entries, and an entry points inside its inode block | 0.2 s |
| `block_words_and_counts_stay_inside_the_block_size` | a SquashFS data block word gives a length from 1 to the block size, and a file's blocks cover its size, or all but a tail shorter than a block | 0.6 s |
| `cab_and_wim_headers_never_panic_and_name_lzx_only_from_a_folder` | on any CAB or WIM head of up to 48 bytes, the refusal checks never panic, a CAB counts as LZX only from a folder inside the head, and a WIM only when its header says it is compressed | not yet timed |
| `volume_and_boot_sector_checks_never_panic_on_any_short_head` | the compound, CHM, FAT, NTFS, APFS and Apple partition map signature checks never panic on any head of up to 520 bytes | not yet timed |
| `disk_signature_and_partition_table_checks_never_panic_on_any_short_head` | the VHDX, QCOW, VDI, VMDK, Android sparse, MBR table and ext magic checks never panic on any head of up to 520 bytes. A VDI header is named only from at least 72 bytes, and an MBR table only from a whole sector that ends with `55 AA` | not yet timed |
| `inodes_never_panic_and_fit_their_bit_fields` | the CramFS inode reader unpacks any 12 bytes of either byte order into fields that fit their bits, with names and offsets in whole words | 0.6 s |
| `superblocks_never_panic_and_keep_a_folder_root` | the CramFS superblock reader never panics on any 76 bytes, accepts only a folder root, and gives a size and CRC exactly for version 2 | 1 s |
| `decoding_any_short_stream_never_panics_or_passes_its_block` | the LZO1X decoder never panics on any input of up to 6 bytes, and never writes past an output of up to 4 bytes | 153 s |
| `pulled_bits_are_the_inputs_next_bytes_and_unused_whole_bytes_go_back` | from any bit buffer of up to 47 bits, `Bits::need` of up to 48 bits takes the next input bytes in order and no byte more than it needs, never holds more than 55 bits and fails only when the input ends. `take` returns the low bits. `give_back_whole_bytes` returns only whole bytes this call took | 2 s |
| `the_next_reversed_code_is_the_code_after_it_with_its_bits_reversed` | for every code of 1 to 15 bits, `next_reversed_code` gives the bit-reversed form of the next canonical code, and nothing after the last | 0.2 s |
| `every_symbols_entry_keeps_its_fields_apart_and_its_value_inside_the_format` | for every symbol of the three Deflate alphabets and every code length, the table entry keeps its code length, flags and value apart. A length is 3 to 258 with up to 5 extra bits, a distance 1 to 32,768 with up to 13, and the codes outside the format are marked invalid | 0.4 s |
| `the_scanner_returns_the_first_magic_and_never_panics` | on any input of up to 7 bytes and any starting bit, `next_magic` returns the first block or end magic at or after that bit, whole inside the input, or none when there is none | 25 s |
| `magics_are_read_only_from_48_whole_bits` | `bits48` on any 8 bytes and any bit returns the 48 bits there in stream order, and nothing when fewer than 48 remain | 2 s |
| `bits_come_in_stream_order_with_zeros_past_the_end` | the BZip2 bit reader, from any start in the first 3 bytes of up to 11 bytes, returns two fields of 1 to 32 bits in stream order, reads zeros past the end and counts every bit | 67 s |
| `block_starts_never_panic_and_start_inside_the_largest_block` | `block_start` on any 15 bytes at any bit of the first byte and any level accepts only the block magic, reads exactly 105 bits and keeps the origin within the level's largest block | 2 s |
| `used_bytes_of_one_range_never_panic_and_come_out_in_order` | `used_bytes` with at most one range set returns the set members of that range in rising order, reads exactly 32 bits and refuses an empty map | 190 s |
| `selectors_never_panic_and_name_only_tables_the_block_has` | `selectors` on up to 4 selectors and 2 to 6 tables keeps one table index per selector, each below the table count | 81 s |
| `code_lengths_never_panic_and_stay_from_1_to_20` | `code_lengths` on any 5 bytes gives two lengths from 1 to 20 or refuses, through any run of steps up and down | 21 s |
| `gzip_headers_read_alike_in_any_split_and_end_where_their_fields_end` | the gzip header reader gives the same result on any 13 bytes read whole or cut at any point. A header it completes starts with 31, 139 and 8, sets no reserved flag and ends where its extra field, name, comment and header CRC end | 115 s |
| `records_never_panic_and_keep_their_count_fields_and_checksum` | the Intel HEX record reader never panics on any digits of up to 14 bytes, and a record it accepts has twice its count plus 5 pairs of digits, the address, type and data those digits give, and bytes that sum to zero | 29 s |
| `steps_never_panic_and_grow_a_run_only_from_where_it_ended` | for any reader state and any record, the step that places an Intel HEX record never panics, refuses an address or start record among the first three, starts a new run exactly when a data record does not start where the last one ended, and sets the base only from a 2-byte address record | 3 s |

Some checks are too heavy for Kani, so exhaustive unit tests cover them:

- `record_limits` sorts, which is heavy for Kani. `record_limits_accept_exactly_the_layouts_without_overlap` in `src/native_zip.rs` compares it with a plain reference on every layout of up to 3 records, about 125,000 cases.
- `checksum_matches` over 512 symbolic bytes ran past 300 s. It reads its field with `octal`, which the proofs cover, and sums at most 512 bytes of 255.
- The tar pax readers and the path and symlink escape checks in `src/lib.rs` use std's string search, which ran past 300 s on 2-byte inputs. `pax_times_equal_their_decimal_value_on_every_short_string`, `pax_records_split_back_into_their_exact_bytes_on_every_short_string` and the two escape tests in `src/lib.rs` check every short string instead.
- `build_table` in `src/inflate.rs`, the Deflate table builder, has nested loops whose counts come from the code lengths. A proof on 4 symbols in 12 entries passed 8 GB without a verdict, and one on 3 symbols in 4 entries passed 5 GB. `every_small_code_is_built_or_refused_and_read_as_a_bit_at_a_time_reference_reads_it` checks it instead on every assignment of lengths 0 to 5 to 7 symbols, in tables of 1 to 4 index bits, 1,119,744 builds. A code is accepted exactly when it is complete, empty or a single 1-bit code. Each accepted table then reads every 7-bit buffer, with every count of valid bits, as a bit-at-a-time reference reads it.

`SegmentPlan`, which splits an LZMA2 stream into segments that decode on their own, is too heavy for Kani. Four attempts ran out of time or memory. `segment_plans_start_at_resets_and_cover_the_folder_for_every_short_stream` in `src/native_7z.rs` checks it instead on every stream of up to four chunks of every kind, every minimum size and every folder boundary, more than 600,000 cases.

The BCJ2 folder graph is checked by exhaustive tests, under `[7z-bcj2-graph]` in `verification/coverage.txt`:

- `every_small_folder_graph_plans_only_sound_bcj2_branches` runs `bcj2_plan` on every folder of up to 3 coders with 4-to-1, 1-to-1 or 2-to-1 streams, every list of bind pairs and every list of packed inputs, over 1,000,000 shapes. Each plan it accepts must give BCJ2's four inputs chains that follow real bind pairs to distinct packed streams, use every coder and packed stream once, and leave BCJ2's output unbound. The oracle recomputes the stream numbering on its own.
- `every_bcj2_layout_with_up_to_four_chained_coders_is_planned` builds every valid layout of BCJ2 and up to four chained coders, at every position and in rotated orders, and checks that the plan finds exactly those chains.
- `every_short_input_round_trips_under_every_conversion_choice` in `src/bcj2.rs` encodes every input of up to 5 bytes over the bytes that BCJ2 treats specially, under every choice of conversions. The decoder must return the input and read every stream to its end, in reads of 1, 3 and 64 bytes.
- `every_main_stream_split_decodes_as_the_byte_at_a_time_reference` and `random_streams_decode_or_fail_as_the_byte_at_a_time_reference` in `src/bcj2.rs` compare the slice decoder with the earlier byte-at-a-time decoder, kept in the test. Encoded inputs with every conversion choice and random hostile streams must give the same bytes, the same error and the same unused input in every read. The main stream is buffered in slices cut at every offset, so an opcode and its `0F` prefix fall on both sides of an edge.

Run them with `cargo test --release --lib bcj2`, then `cargo xtask verify-sync 7z-bcj2-graph`.

`byte_swap_reverses_every_whole_group_under_every_read_split`, under `[7z-byte-swap]`, checks the Swap2 and Swap4 reader on every length up to 13 bytes. The inner reader yields 1 to 6 bytes at a time, the caller reads 1 to 6, and the buffer holds 1, 4, 5, 7 or 64 bytes, 5,040 runs in all. Run it with `cargo test --release --lib byte_swap`, then `cargo xtask verify-sync 7z-byte-swap`.

Zipper's own BZip2 block decoder in `src/bzip2.rs` and the reader that decodes blocks on several threads in `src/bzip2_streams.rs` sit under `[bzip2-blocks]`. The tar, single file, RPM, 7z, ZIP and xar readers all use them, and with one thread the reader decodes each block in place. The `bzip2` crate is a test dependency only, and it is the oracle of every test:

- `every_kind_of_data_at_every_level_decodes_as_the_bzip2_crate_does` covers six kinds of data at levels 1 to 9, from empty to several blocks.
- `every_cut_and_many_bit_flips_decode_or_fail_as_the_bzip2_crate_does` cuts four streams at every length, or the last 600, and flips every one of the first 2,000 bits and 3,000 random ones. The decoder must give the crate's bytes or refuse when it refuses.
- `bits_between_a_block_and_the_next_magic_are_refused` puts 1 to 4,000 bits after each block, which the one-thread reader refuses too.
- `randomised_blocks_decode_as_the_bzip2_crate_does` sets the randomised bit, which no writer has set since bzip2 0.9.5, and repairs both CRCs from the crate's own output.
- `a_magic_inside_a_block_does_not_end_it` plants each magic in spare selectors of each block. `a_magic_planted_inside_any_block_is_passed_over_by_decoding_on` and `a_planted_stream_end_and_header_inside_a_block_start_no_stream` run those streams through the reader at 1 to 4 block threads.
- The `cuts_of_*` and `every_third_flip_*` tests compare the reader with the one-thread reader on cuts and flips of one stream of three blocks and of three streams in a row. Every third flip of each stream is its own test, so CI's test partitions can spread them.
- `a_block_may_fill_the_longest_block_and_one_bit_more_is_refused` pads a block's first code length with steps up and down to exactly 4 MiB and to 4 bits more.
- `one_stream_alone_refuses_or_leaves_what_follows_it_as_the_bzip2_crate_does` covers the two single-stream modes. ZIP refuses data after the stream, and xar leaves it.
- `blocks_in_flight_stay_within_the_workers_and_their_memory` counts queued blocks and their charge after every read.
- `the_lookup_decodes_every_code_as_the_bit_at_a_time_walk_does`, `moving_to_the_front_matches_a_rotation_at_every_index`, `used_bytes_are_the_set_bits_of_the_two_level_map`, `the_crc_is_the_bitwise_bzip2_crc_on_every_split` and `runs_expand_the_same_into_every_size_of_read` compare each kernel with a plain reference.
- `tests/bzip2_blocks.rs` extracts every BZip2 fixture of the corpora and generated tar, 7z and ZIP archives at 1, 2, 4 and default threads, and compares the trees.

The reader starts a block's thread before the block before it has confirmed where it ends. `verification/bzip2-blocks/Blocks.tla` models that for five magics in every layout of true block starts and false magics, with every set of corrupt blocks. TLC checks nine rules. `InOrder` says only true blocks go out, in stream order with none skipped. `FrontIsTrue` says the reader never acts on the result of a decoder started at a false magic. The others are `OkIsComplete`, `FailureIsReported`, `MemoryBounded`, `ThreadsBounded`, `NothingLeaks`, `NoThreadOutlivesReader` and `NoDeadlock`. Run `cargo xtask verify-sync bzip2-blocks`, which runs the model and both test sets, and `cargo xtask verify-sync bzip2-fields` for the seven Kani proofs of the scanner, the bit reader and the block header readers.

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

Zipper's own LZMA decoder in `src/lzma.rs` has 18 Kani proofs in seven groups, each its own section in `verification/coverage.txt`. 7z decodes LZMA and LZMA2 with it, and tar, ZIP and RPM decode xz with its xz reader. The proofs run with `cargo kani --tests`, as each section's full checks give. Each proof gives the range coder a symbolic range of at least 2^24, a symbolic code and as many symbolic input bytes as it may read, so a read past them fails the proof. Probability tables start at one symbolic value from 31 to 2017, the values a probability can reach from 1024. The proofs named `..._decode_as_one_bit_at_a_time_...` instead give every entry its own symbolic value and compare the pipelined kernel with a one-bit-at-a-time reference. The window contents are never symbolic, because a proof of the match copy ran out of memory.

Each group lists only the items its proofs reach, method by method, with the proofs and their helpers. An edit reruns only the groups whose items it touches. A change to `decode`, `Window`, the LZMA2 chunk reader or the xz reader reruns no Kani proof, only the `[lzma]` tests. A change to `bit_value`, `normalize`, `byte` or a shared constant such as `TOP` reruns every group except `[lzma-input]`.

| Group | Its proofs cover | Kani time |
| --- | --- | ---: |
| `[lzma-bits]` | `bit_value`, `bit`, `branching_bit`, `normalize`, `byte`, `direct_bits`, `parts` | 14 s |
| `[lzma-trees]` | `tree` and `reverse_tree` | 1,238 s |
| `[lzma-lengths]` | `LengthDecoder::decode` | 48 s |
| `[lzma-literal]` | `Properties::from_byte`, `literal_context` and `literal` | 367 s |
| `[lzma-matched-literal]` | `matched_literal` and `matched_literal_children` | 2,393 s |
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
| `matched_literals_stay_inside_their_coder_for_every_match_byte` | matched literal | a literal after a match, for any match byte, reads 8 bytes, writes none of the 256 entries its coder holds past its 768 probabilities, and gives a symbol from 0x100 to 0x1FF | 1,079 s |
| `matched_literals_decode_as_one_bit_at_a_time_with_any_sibling_probabilities` | matched literal | a matched literal, with any probability in every entry and any match byte, returns the reference's symbol, coder state and table | 1,313 s |
| `matched_literal_children_stay_in_their_coder_apart_from_the_entry_they_follow` | matched literal | both children `matched_literal_children` gives for a step are the specification's indices, lie inside the 768 entries and differ from the entry they follow, so its mask to the 1,024-entry coder never changes an index | 0.1 s |
| `short_distances_stay_inside_the_special_probabilities` | distances | for slots 4 to 13, `distance_from_slot` reads inside the 115 special probabilities and at most 5 bytes | 50 s |
| `long_distances_fit_32_bits` | distances | for slots 14 to 63, `distance_from_slot` reads at most 30 bytes, stays inside the align table and returns a distance that fits 32 bits | 232 s |
| `input_limits_keep_every_symbol_inside_the_buffer_and_the_chunk` | input | for any input buffer state and LZMA2 input limit, the room `bits` gives fits the buffer, a symbol of up to 64 bytes started when `needs_input` is false stays in it, and a decode that passes `ran_past_input` keeps the start before the end and the consumed input within the limit. `finished_cleanly` is exactly a zero code | 2 s |

Times are from one `verify-sync` run of each group on an Apple M5 shared with other jobs. The heavy proofs are the two matched literal proofs, the three pipelined tree proofs with 6 to 8 bits or reverse trees, and the plain literal reference proof. Together the proofs bound one symbol's input. A match reads at most 1 + 1 + 10 + 6 + 26 + 4 = 48 bytes, a repeated match 15 and a literal 9. All fit the 64 bytes `needs_input` keeps in hand. That sum is argued here, not proved, because `decode_symbols` as one proof would need a symbolic window. The slot tree is the 6-bit tree, and `decode_distance` adds only the `length_state` clamp to the two distance proofs.

The `.lzma` and lzip header readers in `src/lzma.rs` have three more proofs, under `[lzma-headers]`. Run them with `cargo xtask verify-sync lzma-headers`.

| Proof | It proves, for every input in its bound | Time |
| --- | --- | ---: |
| `lzma_file_headers_keep_their_fields_and_only_properties_liblzma_accepts` | on any 13-byte `.lzma` header, `lzma_file_header` keeps the properties byte, dictionary and size, reads a size of all ones as unknown, and accepts only `lc` plus `lp` up to 4. `liblzma_dictionary` gives at least 4 KiB, rounded up to 16 bytes and capped at `u32::MAX` | 1 s |
| `lzip_headers_accept_only_dictionaries_from_4_kib_to_512_mib` | on any 6 bytes, `lzip_header` accepts only `LZIP` and version 0 or 1, with a dictionary from 4 KiB to 512 MiB that `lzip_dictionary` decodes | 1 s |
| `lzip_trailers_accept_only_their_crc_and_sizes` | for any trailer, CRC-32, data size and decoded input, `lzip_trailer_matches` accepts only the stored CRC-32 and data size, and for version 1 the member size, without overflow | 1 s |

The differential tests in `lzma::lzma_file_and_lzip_tests` and `native_tar::lzip_tests`, `native_zip::lzma_entry_tests` and `native_rpm::lzma_payload_tests` compare these readers with liblzma on every preset, the corpora, every cut and the bit flips at each boundary. `cargo xtask verify-sync lzma` runs the first.

Zipper's own raw Deflate decoder in `src/inflate.rs`, under `[inflate]`, decodes ZIP Deflate entries. Its tests compare it with zlib through `flate2`. Each stream is fed in nine piece sizes from 1 byte to the whole stream, into three window sizes. The decoder must give zlib's bytes and consume zlib's count of input bytes, or both must refuse. The streams are these:

- Generated data at levels 0 to 9, and with `ZIPPER_SILESIA` set, each Silesia file at levels 1, 6 and 9.
- Every Deflate entry of every archive under `tests/data`.
- 1,500 random streams of stored, fixed and dynamic blocks from a writer in the test. Its dynamic blocks use random complete codes of up to 15 bits, one-code and empty distance codes, and every repeat code. Each must also decode to the bytes it was built from.
- Every cut of 40 such streams and of a level 6 stream, every single bit flip in the first 400 bytes of 60 streams, random flips, and 20,000 random byte strings.
- Hand-built streams for each refusal, such as block type 3, 287 length codes, a repeat with no length before it, and the unused half of a one-code table.

`every_match_copy_equals_a_byte_at_a_time_copy` checks the match copy at 76 distances with every length from 3 to 258, and that it writes at most 32 bytes past the match. `random_whole_alphabets_decode_every_codeword_to_its_symbol` builds 3,000 random codes in the real table sizes. Kani does not cover `build_table`, `lookup`, `symbol`, `fast`, the match copy or `Inflate::decode`. Run `cargo xtask verify-sync inflate`, which runs the proofs and `cargo test --release --lib inflate` itself.

The framing in `src/framing.rs`, under `[deflate-framing]`, wraps that decoder for every other reader. `Reader` reads raw Deflate for 7z, zlib for xar and SWF, and gzip for tar and `.gz` files, with every member in a row as `flate2`'s `MultiGzDecoder` reads them. `inflate_block` and `zlib_block` decode one block into a fixed output for CAB, SquashFS and CramFS. Its tests keep `flate2` as the oracle:

- `every_zlib_header_is_accepted_exactly_when_zlib_accepts_it` tries all 65,536 header pairs.
- `every_gzip_header_flag_and_member_count_reads_as_multigzdecoder_reads_it` tries every flag byte with three sets of fields and good and bad header CRCs, alone and as two members, cut and flipped. `gzip_names_and_comments_stop_at_flate2s_limit` checks names and comments of 65,534 to 65,536 bytes.
- The reader tests run every framing of generated data at levels 0, 1, 6 and 9, cut and flipped. They feed input in pieces of 1 to 65,536 bytes and read output in pieces of 1 byte to 1 MiB. Each must give flate2's bytes and leave flate2's count of input unread, or both must refuse. The gzip corpus files are cut at every length and flipped in every byte.
- The block tests compare each block function with `flate2::Decompress` and `Finish` into outputs one byte short, exact and longer, with and without a preset history.
- `a_gzip_reader_with_any_window_hands_out_what_flate2_reads_whole_cut_and_flipped` reads through `fill_buf` from windows of 64 KiB to 544 KiB, as the single-file reader does. `a_read_fills_the_callers_buffer_until_the_stream_ends` and `an_error_after_decoded_bytes_reaches_the_next_read_with_its_own_message` cover a read that takes several decode steps. The Adler-32 tests compare every length up to 700 bytes and the block and run edges with a byte-at-a-time sum, and start from the largest sums over the largest bytes.
- `src/crc32.rs` folds a gzip member's CRC-32 with carry-less multiplies on aarch64 CPUs that have `aes` and `sha3`. Its tests compare every length up to 1,700 bytes at four offsets and three splits with a bit-at-a-time CRC, and long inputs in ten piece sizes with crc32fast. `cargo xtask verify-sync deflate-framing` runs them.

CAB, SquashFS, CramFS and SWF each have a test that runs their old `flate2` code beside the new one on the same cut and flipped streams. The tar, 7z and xar readers call `Reader` alone. Run `cargo xtask verify-sync deflate-framing`.

Every section that runs Kani has `cargo xtask kani-reach NAME` as its first full check, and the gate's `verify` step fails when one lacks it. The check keeps the section's item list honest. It runs Kani's code generation for the section's proofs, with each `--harness` prefix expanded to the proofs it matches. It lists every source line in each proof's goto program with `goto-instrument --list-eloc`. It fails when a reached line lies outside the section's items, and names the item to add. Kani places the code that `#[derive]` generates on the attribute line or on a field, so such a line needs the type it belongs to. A reached line that lies in no item is reported as a cut span. It honours `CARGO_TARGET_DIR`.

Code evaluated while compiling leaves no line in the goto program, so the check also follows names. It requires every constant, static, struct, enum, type, trait, macro and `const fn` that a reached function, its `impl` header or one of those definitions names. Inside a constant, a static, a type or a `const fn`, it also requires every function the code calls. It follows `crate::` paths and `use crate::...` imports, nested brace groups and `as` aliases included. It reads a `use super::...` or `use self::...` from the module it sits in, so one that leaves the file is followed as a `crate::` import, and one that climbs out of the crate is refused. It follows them into other files, whose items the group must then list. Strings and comments do not count. It refuses any form it cannot follow to a definition, and names the line. Those forms are `Type::NAME` on a type from another file, a re-export, a `use crate::...::*`, `include!` and a `super::` path that leaves the file. A `crate::` path or import that resolves to nothing is refused too. A path through an imported crate module, such as `xz_filters::Branch::X86` after `use crate::xz_filters`, is followed to its definition. A variant of an enum from another file, such as `AesExtra::Absent`, is followed to that enum when the enum declares it. An edit that adds a call from a listed item changes that item, so the group goes stale and its next `verify-sync` runs the check again.

An item ends at its closing brace, or at the first `;` outside any bracket, brace or parenthesis. So an item whose signature holds `[u8; N]` is fingerprinted whole. `every_manifest_item_is_fingerprinted_to_its_end` in `xtask/src/verify.rs` fails when an item in `verification/coverage.txt` ends inside an open bracket. `each_lzma_edit_makes_stale_only_the_groups_whose_proofs_reach_it` in the same file checks which groups an edit to each kernel makes stale. `each_parser_edit_makes_stale_only_the_section_whose_proofs_reach_it` checks that an edit to the `ar` header reader makes only `[ar-fields]` stale, an edit to the tar numeric fields only `[tar-numbers]`, and an edit to the ZIP64 field reader only `[zip64-extra-field]`. An item also starts at the attributes right above it when one of them is a `#[kani::...]` attribute, so a changed unwind bound or stub on a proof makes its section stale. Other attributes above an item, such as `#[derive]`, are not fingerprinted. A section that lists a whole `mod proofs {` fingerprints every attribute inside it.

The gate's `verify` step, which `cargo xtask verify` runs alone, finds every `#[kani::proof]` in `src/` from the real module nesting, under any visibility and past comments and other attributes. It fails when any proof runs in no Kani full check or in more than one, whether the filters name it exactly or match it as part of a longer path. It also fails when a `--harness` filter matches no proof its check can run, and when a section runs Kani in more than one `cargo kani` line. A `cargo kani` line may pass only `--tests`, `--exact` and `--harness`, so no flag such as `--only-codegen` or `--no-default-checks` can skip or weaken a check. A proof under any cfg other than `#[cfg(kani)]`, or `#[cfg(test)]` with `--tests`, fails it by name. So does any attribute that names `proof` other than `#[kani::proof]` alone on its own line outside any macro, such as `#[proof]`, `#[::kani::proof]`, `#[kani::proof_for_contract]` or `#[cfg_attr(..., kani::proof)]`, and any `use kani::proof` or `use kani::*`.

The `verify` step also fails when a section runs a proof that none of its items covers, and when Kani-only code goes unlisted. Kani-only code is every item in a `#[cfg(kani)]` module, and every item under a cfg that names `kani`, such as the `name_checksum` stand-in. A section that runs every proof beside such code must list it. When the proofs are split across sections, as the LZMA groups are, at least one of those sections must list it. The message names the `item = ...` line to add. A section that runs every proof of a module lists the whole proofs module, such as `item = src/native_ar.rs | mod proofs {`, so a new proof or helper there is covered at once. The sections that split `native_7z::proofs` and `native_zip::proofs` list each proof they run, and a new proof there runs in no section until one names it, so the gate fails.

The text checks can miss a form nobody thought of. `cargo xtask release-checks` covers that by rerunning every proof on the exact release code, and it also compares the gate's list of proofs with `cargo kani list` and `cargo kani --tests list`.

A `verify-sync` builds a section's proofs once. Kani passes the `--harness` filters to its compiler, so a run with other filters rebuilds the crate, and one with the same filters reuses the build. The reach check therefore runs `cargo kani --only-codegen` with the section's own `cargo kani` line, so the Kani run reuses its build. Each codegen prints whether it reused the build or how many proofs it compiled. A section may hold only one `cargo kani` line for that reason.

Exhaustive and differential tests cover the rest. `chunked_match_copy_matches_a_byte_at_a_time_copy_in_every_window_state` checks `Window::copy` against a byte-at-a-time copy in every window state of four sizes. `long_runs_at_every_short_distance_match_a_byte_at_a_time_copy_across_the_wrap` checks every distance from 1 to 15 with every length up to 273 in every window state of three sizes, which covers the cold `copy_repeating` path. Both bit decoders update a probability in 7-Zip's form, `p - ((bit ? p : p - 2017) >> 5)`. `the_offset_update_equals_the_masked_update_for_every_probability_and_bit` checks it against the masked form it replaced, on every probability from 0 to 2048 and both bits. `branching_bits_normalized_in_each_arm_match_one_normalization_after_both` compares it with the specification's update. Each literal coder holds 1,024 probabilities, of which a matched literal uses 768, so the matched literal masks its indices instead of checking them. `the_memory_charge_covers_what_each_reader_allocates_for_every_literal_context` checks that `lzma_memory` and `lzma2_memory` charge at least what each reader allocates, for every `lc` and `lp`. The liblzma differential tests cover every `lc`, `lp` and `pb`, every cut stream, LZMA2 control codes and properties, and the xz reader's fields, cuts and damage. Kani does not cover `RangeDecoder::start`, `refill`, `Window`, `decode_symbols`, the LZMA2 chunk reader or the xz reader. `[lzma]` lists the whole decoder, and `cargo xtask verify-sync lzma` runs `cargo test --release --lib lzma` over it. `cargo xtask verify-sync lzma-bits`, or another group's name, runs that group's reach check and proofs and records its fingerprint only if both pass.

Zipper's own Zstandard decoder in `src/zstandard.rs` has three Kani proofs under `[zstd-tables]`, and its tests under `[zstd]`, which lists the whole decoder.

| Proof | It proves, for every input in its bound | Time |
| --- | --- | ---: |
| `weights_that_pass_form_a_complete_tree_with_its_deepest_leaves_in_pairs` | `weights_from_list` on any 8 weight bytes never panics. A list it accepts sums to a power of two no deeper than 12 bits, with every weight within the depth and the rank counts exact. It has an even number of deepest leaves, at least two | 16 s |
| `refills_keep_the_reader_inside_its_stream_and_its_count_of_bits` | `BitD::step_back` from any position never moves below the stream's start, keeps the count of unread bits, leaves the reader at the start or with fewer than 8 bits consumed, and reports an overflow exactly when more than 64 bits were consumed | 0.4 s |
| `frame_headers_never_panic_and_bound_their_window_and_blocks` | `frame_header` on any descriptor and up to 14 field bytes never panics, accepts only the length the descriptor announces, and gives a block limit of at most 128 KiB and a window of 1 KiB to 3.75 GiB unless the frame is a single segment | 2 s |

The table builders are checked by exhaustive tests at the real table sizes. A Kani proof of both Huffman tables ran for over 8 minutes on 8 slots and was dropped.

- `every_small_count_list_and_random_large_ones_build_states_that_stay_inside_their_table` builds the sequence and weight tables from every list of 5 counts that fills 32 states, over 50,000 lists, and from 20,000 random lists of up to 512 states. Every state must step inside the table, and each symbol must hold as many states as its count.
- `both_huffman_tables_spell_the_same_codes_for_every_small_tree_and_random_deep_ones` builds both Huffman tables from every list of up to 6 weights of 0 to 6 and from 3,000 random trees of up to 12 bits and 255 symbols. Every slot of the two-symbol table must hold the one-symbol table's symbol, and a second symbol exactly when its code fits.
- `table_descriptions_of_every_two_bytes_and_many_random_ones_fill_their_table_or_are_refused` runs `read_ncount` on every 2-byte description and on 3,000,000 random ones of up to 24 bytes, for each of the four symbol limits. A description it accepts must use no more bytes than it has and give counts of at least -1 that sum to exactly the table's size, for every table of up to 2^14 states. Every caller refuses a table above 2^9. A Kani proof of the same rule on 10 bytes and 8 symbols passed 11 GB after 5 minutes and was dropped.
- `the_refill_schedule_keeps_every_field_of_a_sequence_inside_the_container` checks, for every count of extra bits the three codes can ask for, that the sequence loop never shifts its 64-bit container by 64 or more.
- `a_state_keeps_its_fields_and_finds_its_next_state_as_a_bit_at_a_time_reader_does` packs every state that `state_step` can give in a table of up to 2^9 states, with five counts of extra bits. Every field must read back. `next_state` must give the base plus the bits a bit-at-a-time reader takes, at every position in the container and for both spellings of the shift. The table test also checks that every base is a multiple of two to its bit count, which is why an OR adds the bits.
- `a_field_of_up_to_31_bits_reads_as_a_bit_at_a_time_reader_reads_it` checks `bits_at` for every count from 0 to 31 at every position.
- `a_match_from_the_last_lap_that_would_copy_past_the_buffer_takes_the_careful_path` calls `fast_sequences` on one hand-built sequence. Its match starts in the previous lap, 8 bytes from the end of the output, so a 32-byte step would pass the buffer. The loop must stop for the careful copy.

The sequence loop is `fast_sequences`. It makes no call that returns, and it returns to `run_sequences` for each sequence that needs a careful copy. Keep it that way. The [record](../../../benchmarks/zstd/own-decoder/README.md) shows that a call in the loop moves its states to the stack, which costs an Apple M2 Max 7% of an extraction's cycles and shows on no test. After a change to the loop, disassemble it and count cycles on that machine as the record describes. One planted break in it is caught by nothing. Without `offset > bounds.window` on a match from the previous lap, `compressed_block` still refuses the block by its largest offset.

The differential tests compare the decoder with libzstd 1.5.7, as a stream and in one call. They cover frames at every level, and every cut and every flipped bit of 14 frames from libzstd and of the hand-built frames. They also cover random damage, every frame header shape, every block type and size, frames in a row and skippable frames. Both decoders must give the same bytes or both refuse. Two differences are allowed, both in a stream. Zipper refuses a match from beyond the window, and a content size that lies before an empty last block. The hand-built frames come from a small encoder in the tests, and a byte-at-a-time model gives their expected bytes. They reach what libzstd's encoder never writes, such as a Huffman tree 12 bits deep, four streams under 8 bytes, and matches across the end of the window buffer. Run `cargo xtask verify-sync zstd`, then `cargo xtask verify-sync zstd-tables`.

## When to run it

The gate's `verify` step fails when code listed under a section that names this skill in `verification/coverage.txt` has changed since its checks last passed. It names each stale section. Run those, and before changing that code on purpose.

## Run it

```sh
cargo xtask verify-sync tar-numbers
```

Name the stale section. Kani needs `cargo install --locked kani-verifier` and `cargo kani setup` once, on macOS or Linux. Every proof must report `VERIFICATION:- SUCCESSFUL`. Check `pgrep -x cbmc` first and never run two Kani runs at once. Each section runs its reach check first and records its fingerprints only if every proof passes. A new proof needs a filter in exactly one section, or the gate fails.

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
| The probability update subtracts 2018 instead of 2017 before its shift | `branching_bits_normalized_in_each_arm_match_one_normalization_after_both` fails |
| The probability update shifts its target 6 places | `the_offset_update_equals_the_masked_update_for_every_probability_and_bit` fails |
| `direct_bits` shifts its value by two bits a step | `direct_bits_fit_their_count_and_keep_the_range_normalized` fails |
| `tree` subtracts `1 << (bits - 1)` from its index | `every_bit_tree_size_stays_inside_its_table` fails |
| The length decoder adds 17 to a high length | `lengths_stay_below_the_longest_match_at_every_position_state` fails |
| The literal context shifts the position by `lc + 1` | `literal_contexts_stay_inside_the_literal_table_for_every_property_byte` fails |
| `Properties::from_byte` accepts the byte 225 | the same proof fails |
| A plain literal decodes until its symbol reaches 0x200 | `plain_literals_stay_inside_the_first_quarter_of_their_coder` fails |
| A matched literal ORs its offset mask instead of ANDing it | `matched_literals_stay_inside_their_coder_for_every_match_byte` fails |
| `matched_literal_children` gives the matched base to the wrong child | `matched_literal_children_stay_in_their_coder_apart_from_the_entry_they_follow` fails |
| `decoder_memory` charges 768 probabilities a literal coder, not 1,024 | `the_memory_charge_covers_what_each_reader_allocates_for_every_literal_context` fails |
| A short distance starts its special probabilities one entry too far on | `short_distances_stay_inside_the_special_probabilities` fails |
| A long distance shifts its direct bits two places too far | `long_distances_fit_32_bits` fails |
| `bits` gives one byte more room than the padding past the end | `input_limits_keep_every_symbol_inside_the_buffer_and_the_chunk` fails |
| The Deflate bit reader's `pull` adds 7 bits a byte | `pulled_bits_are_the_inputs_next_bytes_and_unused_whole_bytes_go_back` fails |
| `give_back_whole_bytes` keeps a whole byte | the same proof fails, and so does `bytes_after_the_last_block_are_left_unconsumed` |
| `next_reversed_code` keeps the bits above the one it sets | `the_next_reversed_code_is_the_code_after_it_with_its_bits_reversed` fails |
| The end-of-block entry loses its flag | `every_symbols_entry_keeps_its_fields_apart_and_its_value_inside_the_format` fails |
| The gzip header reader accepts reserved flag bit 5 or 7 | `gzip_headers_read_alike_in_any_split_and_end_where_their_fields_end` fails |
| The gzip extra length is read big-endian | the same proof fails |
| The zlib header allows a window of 64 KiB or a preset dictionary | `every_zlib_header_is_accepted_exactly_when_zlib_accepts_it` fails |
| The gzip header CRC, ISIZE or the zlib Adler-32 goes unchecked | the gzip flag test or the cut and flipped reader and block tests fail |
| The gzip reader stops after one member | `python_gzip_members_read_as_flate2_reads_them_whole_cut_and_flipped` fails |
| A gzip trailer is gathered again after an interrupted read | `an_interrupted_read_at_any_byte_is_retried_without_losing_a_member` fails |
| A failed reader reads on | `a_failed_trailer_stays_failed_when_the_right_trailer_follows` fails |
| An error that follows decoded bytes in one read is dropped | `an_error_after_decoded_bytes_reaches_the_next_read_with_its_own_message` fails |
| A read returns after one decode step | `a_read_fills_the_callers_buffer_until_the_stream_ends` fails |
| The gzip reader with its own window size skips the gzip framing | `a_gzip_reader_with_any_window_hands_out_what_flate2_reads_whole_cut_and_flipped` fails |
| Adler-32 loses the weight of a block, or a run grows to twice 5,504 bytes | `adler32_matches_a_byte_at_a_time_reference_in_every_split` fails |
| The CRC-32 fold's constant is one bit off, or the running CRC is left out of the first lane | `every_length_offset_and_split_gives_the_bit_at_a_time_crc` fails |
| MSZIP drops its history, or SquashFS, CramFS or SWF drops a size or end check | that caller's test against its old `flate2` code fails |
| Length code 285 gets 5 extra bits | the same proof fails |
| The Deflate `build_table` accepts an over-subscribed code | `every_small_code_is_built_or_refused_and_read_as_a_bit_at_a_time_reference_reads_it` fails |
| `build_table` accepts any incomplete code | the same test fails |
| `build_table` gives the unused half of a one-code table the code | the same test fails |
| `build_table` sizes a subtable one bit short | the same test fails |
| The long match copy reads two periods back at distances up to 48 | `every_match_copy_equals_a_byte_at_a_time_copy` fails |
| The short-period copy doubles its distance one step early | the same test fails |
| The Deflate fast loop never refills before a distance | `the_longest_codes_back_to_back_decode_as_zlib_does_at_every_bit_offset` fails |
| The fast loop refills for the distance but not for the next lookup | the same test fails |
| The fast loop or the careful path accepts a distance one byte before the start | the zlib differential tests fail |
| A stored block's two lengths are not compared | the zlib differential tests fail |
| 287 length codes or 31 distance codes are accepted | `dynamic_headers_outside_the_format_are_refused` fails |
| A block with no end-of-block code is accepted | `dynamic_codes_are_accepted_and_refused_as_zlib_does` fails |
| A code length repeat may run one past the lengths | the zlib differential tests fail |
| `stream_entry` accepts a Deflate stream that ends before its compressed size | `tests/hostile_zip.rs` fails |
| The cpio `hex` reader accepts base-17 digits | `newc_headers_never_panic_and_fit_their_fields` fails |
| The `ar` field reader accepts a digit equal to its radix | `headers_never_panic_and_their_sizes_fit_ten_digits` fails |
| `lzip_header` accepts a base 2 logarithm of 30 | `lzip_headers_accept_only_dictionaries_from_4_kib_to_512_mib` fails |
| `lzip_trailer_matches` checks the member size only past version 1 | `lzip_trailers_accept_only_their_crc_and_sizes` fails |
| `lzma_file_header` accepts `lc` plus `lp` up to 8 | `lzma_file_headers_keep_their_fields_and_only_properties_liblzma_accepts` fails |
| `liblzma_dictionary` does not round up to 16 bytes | the same proof fails |
| `next_magic` tries only 7 of the 8 bit shifts of a byte | `the_scanner_finds_the_first_magic_a_bit_at_a_time_search_finds` fails |
| `decode_block` no longer reports a block that runs past its cut | `a_magic_planted_inside_any_block_is_passed_over_by_decoding_on` fails |
| The block reader skips a block CRC, a stream CRC before another stream, or the last stream CRC | an `every_third_flip_*` test of `bzip2_streams` fails, each time |
| The block reader takes the newest queued block, not the oldest | `blocks_come_out_in_order_for_every_worker_count_input_step_and_read_size` fails |
| The block reader accepts a block that ends before its cut | `bits_between_a_block_and_the_next_magic_are_refused` fails |
| The block reader drops the 4,096 bytes a further stream must bring | `concatenated_streams_decode_or_are_refused_as_the_serial_reader_does` fails |
| The block reader drops the 4 MiB limit on a cut | `a_block_may_fill_the_longest_block_and_one_bit_more_is_refused` fails |
| The block reader skips its budget check, or starts one block more than its workers | `blocks_in_flight_stay_within_the_workers_and_their_memory` fails, each time |
| The block reader keeps the level of a stream it started past a false cut | `a_planted_stream_end_and_header_inside_a_block_start_no_stream` fails |
| A BZip2 run of four with no length byte is accepted | `a_run_of_four_takes_the_next_byte_as_its_length_and_must_have_one` fails |
| A randomised BZip2 block is not unrandomised | `randomised_blocks_decode_as_the_bzip2_crate_does` fails |
| The BZip2 lookup table stores a code length with its low bit flipped | `the_lookup_decodes_every_code_as_the_bit_at_a_time_walk_does` fails |
| BZip2 move-to-front carries the wrong byte between its 16-byte words | `moving_to_the_front_matches_a_rotation_at_every_index` fails |
| A single BZip2 stream accepts data after it where none may follow, or reads what xar leaves as another stream | `one_stream_alone_refuses_or_leaves_what_follows_it_as_the_bzip2_crate_does` fails, each time |
| The walk from both ends trusts a broken cycle, or drops the middle byte of an odd block | the crate differential tests fail, each time |
| The reader with no threads skips the 4 MiB block limit | `a_block_may_fill_the_longest_block_and_one_bit_more_is_refused` fails |
| `Blocks.tla` keeps the cursor past a false cut, or takes a truncated front as whole | TLC reports a rule broken |
| `Blocks.tla` drops the budget check, the worker check, the join at a false cut or the join on drop | TLC reports a rule broken, each time |
| `Blocks.tla` takes a failed front, or finishes with blocks queued | TLC reports a rule broken, each time |
| The Zstandard window buffer keeps one slack of 64 bytes, not two, past the window | `matches_reach_across_the_end_of_the_window_buffer_as_the_byte_at_a_time_model` fails |
| The Zstandard decoder drops the window check on matches inside one lap of its buffer | `a_streamed_match_from_beyond_the_window_inside_one_buffer_lap_is_refused` fails |
| The two-symbol Huffman reader clamps its last symbol at 65 consumed bits, not 64 | `a_tree_built_for_four_streams_reads_a_later_single_stream_as_libzstd_reads_it` fails |
| The Zstandard stream reader, or `decode_frames_into`, accepts any frame checksum | `frame_headers_of_every_shape_read_as_libzstd_reads_them` fails |
| The Zstandard stream reader accepts a frame longer than its content size | the same test fails |
| `decode_frames_into` drops libzstd's limit of 32 bytes past the largest block | `a_block_that_decodes_to_more_than_the_largest_block_is_refused_as_libzstd_refuses_it` fails |
| The fast four-stream Huffman loop drops the check that a stream stayed within 8 bytes of its start | the bit flip tests fail |
| `single_frame` goes on to the next frame | `a_single_frame_reader_stops_where_the_zstd_crate_stops` fails |
| `state_step` reads one bit more for every state | `every_small_count_list_and_random_large_ones_build_states_that_stay_inside_their_table` fails |
| `build_double` pairs a second symbol only when one bit more than it needs is left | `both_huffman_tables_spell_the_same_codes_for_every_small_tree_and_random_deep_ones` fails |
| The sequence loop refills at 39 extra bits, not 31 | `the_refill_schedule_keeps_every_field_of_a_sequence_inside_the_container` fails |
| `SequenceState::new` sets the reach one past its last bit or leaves the extra bits out of the bits read, or `next_state` shifts by one more | `a_state_keeps_its_fields_and_finds_its_next_state_as_a_bit_at_a_time_reader_does` fails, each time |
| `bits_at` masks one bit more | `a_field_of_up_to_31_bits_reads_as_a_bit_at_a_time_reader_reads_it` fails |
| `fast_sequences` drops a term from the bit position of the literal, match or offset state, or from the bits consumed | `cargo test --release --lib zstandard::tests::` fails, each time |
| The refilled path of `fast_sequences` counts one bit less or skips the literal state's bits | the same tests fail, each time |
| `run_sequences` reads the first states one bit late, or `fast_sequences` advances the states after the last sequence | the same tests fail, each time |
| `run_sequences` leaves the literals of a careful copy unread | the same tests fail |
| `fast_sequences` runs to the end of the buffer | the same tests fail |
| `fast_sequences` skips the check of the literal end | the same tests fail under `--profile ci`, where `rest_of_literals` overflows. A release build still refuses the block, so no test fails there |
| A literal or match copy past 32 bytes stops at 64 | the same tests fail, each time |
| A match from the previous lap stays in the loop when it is longer than what the lap holds | the same tests fail |
| `fast_sequences` copies a match from the previous lap that ends within 32 bytes of the buffer's end | `a_match_from_the_last_lap_that_would_copy_past_the_buffer_takes_the_careful_path` fails |
| `read_ncount` drops the check that the counts fill the table | `table_descriptions_of_every_two_bytes_and_many_random_ones_fill_their_table_or_are_refused` fails |
| `weights_from_list` accepts a remainder that is not a power of two | `weights_that_pass_form_a_complete_tree_with_its_deepest_leaves_in_pairs` fails |
| `BitD::step_back` gives back 4 bits a byte, not 8 | `refills_keep_the_reader_inside_its_stream_and_its_count_of_bits` fails |
| `frame_header` accepts a window log of 32 | `frame_headers_never_panic_and_bound_their_window_and_blocks` fails |
