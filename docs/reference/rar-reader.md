# In-process RAR reader

Zipper extracts RAR 5 archives and RAR 4 archives that are not solid with its own Rust reader and decoders. The default engine and `--zip-engine native` send a file here when it starts with the RAR 5 signature `Rar!\x1A\x07\x01\x00` or the RAR 4 signature `Rar!\x1A\x07\x00`. Under the default engine, an archive whose first headers Zipper does not read goes to 7-Zip. That covers a volume of a set, an archive with encrypted headers, a RAR 4 solid archive and a damaged first header. `--zip-engine backend` hands every RAR to 7-Zip.

## Sources

The user decided on 4 November 2026 that Zipper reads RAR with its own decoder and that its authors never read the `unrar` source or 7-Zip's RAR code. Both carry unrar's licence restriction. The reader was written from these sources only:

- RARLAB's [RAR 5.0 archive format](https://www.rarlab.com/technote.htm) technote, for the container.
- libarchive's BSD licensed `archive_read_support_format_rar5.c`, for the RAR 5 compression, the filters and the details the technote leaves out. `src/rar5_lz.rs` follows it, and `LICENSE-libarchive` keeps its notice.
- libarchive's BSD licensed `archive_read_support_format_rar.c`, for the RAR 4 container, the RAR 2.9 compression, its PPMd blocks and the standard filters. `src/rar4.rs` and `src/rar4_lz.rs` follow it, and `LICENSE-libarchive` keeps its notice.
- The `ppmd-rust` crate's PPMd variant H decoder with the range coder RAR uses, which Zipper's 7z and ZIP readers already depend on.
- libarchive's test archives, which `tests/data/rar-corpus` holds under the same notice.
- `7zz` and `bsdtar` run as black boxes on test archives, for choices neither source settles.

## RAR 5 archives

A RAR 5 archive is the signature, a main header, a header for each file or service record, and an end of archive header. Each header carries a CRC-32 of its bytes. The reader supports these features:

- Stored entries and the five compression methods, with dictionaries from 128 KiB to 2 GiB. Compression version 0 and version 1 with the flag that marks a version 0 stream are read.
- The delta, E8, E8E9 and ARM filters.
- Solid archives, where each file continues the window, tables and distances of the file before it.
- Folders, Unix and Windows symlinks and hard links. A Windows symlink's `\` separators become `/`, and its target passes the same checks as a Unix one. A `\??\` or UNC target then starts with `/` and is refused. On macOS and Linux a drive target such as `C:\x` becomes the relative link `C:/x`, which stays inside the destination. Windows refuses it.
- CRC-32 and BLAKE2sp file checksums. BLAKE2sp is Zipper's own safe Rust in `src/blake2sp.rs`.
- Service headers, such as comments, quick open data and NTFS streams. The reader skips them.
- Bytes after the end of archive header, which the reader ignores, as RAR does.

The reader refuses these archives, and publishes nothing:

- An archive whose headers are encrypted, and an entry with an encryption record. The error suggests `--zip-engine backend`.
- A volume of a set, and an entry that continues into or from another volume. The error suggests `--zip-engine backend`.
- A self-extracting archive. The executable reader sees the PE file and refuses the data after it, as it does for every archive after an executable.
- Junctions and file copies. The error suggests `--zip-engine backend`.
- RAR 7 compression, a file of unknown size, and a compression method or host OS RAR 5 does not define.

## RAR 4 archives

A RAR 4 archive is the signature, a main header, then blocks for files, service records and old comments, each with a CRC-16 of its header. The reader supports these features:

- Stored entries, and entries compressed with RAR 2.9's LZ and Huffman coding, the algorithm RAR 3 and RAR 4 write for every method.
- PPMd variant H blocks that start a new model, mixed with LZ blocks.
- The standard filters RAR's virtual machine runs, recognized by their program's CRC-32 and length as libarchive recognizes them. These are delta, E8, E8E9, RGB and audio.
- Folders, Unix symlinks, DOS times with extended time fractions, Unix modes and the read-only attribute.
- An archive that ends without an end of archive header, as libarchive and 7-Zip read it.

The reader refuses these RAR 4 archives, and publishes nothing:

- A solid archive, or a solid entry. libarchive does not read them either, so nothing here could check the result. The error suggests `--zip-engine backend`.
- An encrypted archive or entry, and a volume of a set. The error suggests `--zip-engine backend`.
- An entry compressed with the RAR 1.5 or RAR 2.0 algorithms. The error suggests `--zip-engine backend`.
- A PPMd block that continues the model before it, and a PPMd block that holds a filter. The `ppmd-rust` decoder has no way to restart its range coder on a kept model, so `test_read_format_rar_ppmd_lzss_conversion.rar` is refused. A debugging run that let it continue matched bsdtar's output on its first PPMd block of 32 KiB and the 40 MB of LZ data after it, by CRC-32.
- A filter program that is not one of the standard filters, since Zipper runs no RAR virtual machine.

A RAR 4 name is UTF-8, or RAR's Unicode encoding when its flag is set and the name holds a zero byte. A name that is not UTF-8, in a code page, is refused, with a hint to use `--zip-engine backend`. RAR 4 writes `\` between folders on every host, so the reader reads it as a separator.

## Names

A name is UTF-8 with `/` between folders, as RAR 5 writes it. Names then pass the same checks as tar names, so absolute names, `..`, backslashes, drive letters and empty names are refused. A file with a version record extracts as `[VER]/N/name`, as 7-Zip names it.

## Metadata

- A file gets its modification time, to the nanosecond when the time record carries it. RAR 5 times are UTC. A RAR 4 time is a DOS time, read as local time, with the extended time's fraction in 100 ns steps.
- A file written on Windows with the read-only attribute gets mode `0o444`, and every other file `0o644`. A folder gets `0o755`. An entry written on Unix gets the permission bits of its mode. The umask limits all of them.

The reader never restores owners, creation or access times, or the hidden, system or archive attributes.

## Safety checks

The reader checks every entry before it publishes anything, and it publishes nothing on failure:

- Every header holds 1 byte to 2 MiB, its size field is at most 3 bytes, and its CRC-32 matches. The first header is the main header, and the archive ends with its end of archive header.
- Every number is at most 10 bytes and fits 64 bits. Every field, extra record, name and link target lies inside its header.
- A stored entry packs exactly as many bytes as it holds. A folder carries no data.
- A compressed block's check byte matches, and the block lies inside its file's packed data. Tables are complete or under-full. A code that an under-full table leaves unused is refused when it is read.
- A match reaches no further back than the start of its stream or its dictionary. No literal or match writes past the declared size. A filter lies inside its file, after the filter before it, and spans 4 bytes to 4 MiB and at most half the dictionary.
- The packed data ends where the decoded file ends. Each file's CRC-32 and BLAKE2sp must match.
- Every RAR 4 header holds at least 7 bytes and its CRC-16 matches. Its fields, name, salt and extended time lie inside it, and its data lies inside the file.
- A RAR 4 match reaches no further back than the start of its file or the 4 MiB window. A RAR 4 filter lies inside its file, after the filter before it unless a program reset dropped it, and inside the 256 KiB filter memory, and its length register equals its block length.
- A RAR 4 file may start new PPMd models only while their sizes stay inside the allowance in the limits below. An adversarial review built an 8 KB archive that started a 256 MiB model 1,000 times for one byte of output, which took 3.9 s of CPU before the allowance.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 1,000,000 |
| One header | at most 2 MiB, as the technote allows |
| One name | at most 8,192 bytes |
| All names | at most 64 MiB |
| Dictionary | at most 2 GiB, the decoder budget the 7z reader also uses. RAR 4 uses a 4 MiB window, the RAR 3 maximum |
| PPMd model | at most 256 MiB, the most a RAR 4 block can ask for |
| PPMd models in one file | 1 GiB of models, plus 64 bytes of model for each byte decoded, since each new model is filled with zeros |
| Decoder memory | the window, grown as the stream writes it up to the dictionary, one block of up to 16 MiB and one filter of up to 4 MiB. A RAR 4 window and its largest PPMd model fit the same 2 GiB budget |

Each RAR 5 file's dictionary is charged against a 2 GiB decoder budget, the one the 7z reader uses, before any window is allocated. A file that declares more is refused, with a hint to use `--zip-engine backend`. An adversarial review built a 768 KiB archive with a 4 GiB dictionary and 1 GiB of zeros, which raised `zipper test` to 1.08 GB of resident memory before the budget. `tests/hostile_rar.rs` builds a 90 KB file that declares a 4 GiB dictionary and decodes to 328 MB. Before the budget `zipper test` extracted it at 541 MB of resident memory, and now the test requires a refusal that raises peak memory by less than 64 MiB. The window grows only as data arrives, so a small file with a large dictionary allocates no more than about twice its size. Growth uses a fallible allocation, so a window the machine cannot hold is an error, not an abort. The reader keeps only the tree that the cpio reader also uses, with each name. On a RAR 5 archive of 300,000 empty stored files whose names total 60 MB, `zipper test` peaked at 189 MB of resident memory, and `7zz t`, which read every RAR before this reader, at 133 MB. On the same files in RAR 4 they peaked at 173 MB and 133 MB. `--threads` has no effect on RAR.

## Differences from 7-Zip

No RAR writer is installed here. The Homebrew build of 7-Zip 26.05 has no RAR codec, so it lists every RAR but extracts only stored entries. `tests/rar_corpus.rs` therefore checks the archives of libarchive's BSD licensed test suite in `tests/data/rar-corpus`, recorded by `scripts/record_rar_corpus.py`. Each readable archive must extract to the tree `7zz x` extracts where 7-Zip decodes it, and to the tree bsdtar extracts where it does not. Each damaged or unsupported one must be refused. `tests/hostile_rar.rs` cuts the corpus archives at every byte and flips bits in every byte. It builds hostile RAR 5 and RAR 4 headers with its own writers, and reads the archives the adversarial reviews built in `tests/data/rar-hostile`. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip refuses a main header with bytes after its fields. Zipper skips them, as libarchive does.
- 7-Zip extracts a RAR 5 archive cut inside or before its end of archive header. Zipper refuses it. A RAR 4 archive cut between two blocks extracts the entries before the cut, as 7-Zip and libarchive extract it.
- bsdtar 3.7.4 extracts a RAR 5 filter longer than half the dictionary, and a solid file that declares a larger dictionary than its stream. Zipper refuses both, as libarchive's current source does. No RAR writer was at hand to show whether RAR writes either.
- 7-Zip reads a RAR 4 DOS time with the offset from UTC in force today. Zipper reads it with the offset in force on that date, as its ZIP and ARJ readers do, so a file written in another daylight saving season differs by an hour. The corpus records no RAR 4 times for that reason.
- libarchive refuses `test_read_format_rar_filter_incomplete_block.rar`, which its test calls a filter that is never decoded in full. Zipper extracts it, and the file's CRC-32 matches.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options). An entry the selection leaves out is still decoded, since a solid stream needs it. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries.

## Formal checks

Kani proves that the number reader never panics and ends on a byte without the high bit, within 10 bytes. It proves that the general header reader keeps its fields and extra area inside the header. The extra record reader keeps every link target inside its record, and the file header reader keeps the name inside its fields. Every dictionary the compression information gives lies from 128 KiB to 4 GiB. The block header reader reads only its own 3 to 5 bytes and gives a size below 16 MiB. The proofs are under `[rar-fields]` in `verification/coverage.txt`. Tests under `[rar5-lz]` check the decoder and BLAKE2sp. The Huffman tables decode every code as a bit-at-a-time walk does, and each filter undoes its encoding. A block writer in the tests builds literals, matches, repeats and filters, which must decode as a byte-at-a-time model does, and builds the matches, filters and sizes the decoder must refuse. BLAKE2sp matches Python's `hashlib` on every split. The corpus and hostile tests run under the same section. Kani also proves that the RAR 4 file header reader keeps the name inside the header. The extended time reader gives at most 24 bits of 100 ns units, and a filter record's VM number reader reads no bit past its record. Tests under `[rar4-lz]` build RAR 2.9 streams with fixed-width tables, which must decode as a byte-at-a-time model does or be refused.
