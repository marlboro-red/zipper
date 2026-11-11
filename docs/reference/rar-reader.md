# In-process RAR reader

Zipper extracts RAR 5 archives with its own Rust reader and decoder. The default engine and `--zip-engine native` send a file here when it starts with the RAR 5 signature `Rar!\x1A\x07\x01\x00`. Under the default engine, a RAR 4 archive and a RAR 5 archive whose first headers Zipper does not read go to 7-Zip. That covers a volume of a set, an archive with encrypted headers and a damaged first header. `--zip-engine native` refuses a RAR 4 archive. `--zip-engine backend` hands every RAR to 7-Zip.

## Sources

The user decided on 4 November 2026 that Zipper reads RAR with its own decoder and that its authors never read the `unrar` source or 7-Zip's RAR code. Both carry unrar's licence restriction. The reader was written from these sources only:

- RARLAB's [RAR 5.0 archive format](https://www.rarlab.com/technote.htm) technote, for the container.
- libarchive's BSD licensed `archive_read_support_format_rar5.c`, for the RAR 5 compression, the filters and the details the technote leaves out. `src/rar5_lz.rs` follows it, and `LICENSE-libarchive` keeps its notice.
- libarchive's test archives, which `tests/data/rar-corpus` holds under the same notice.
- `7zz` and `bsdtar` run as black boxes on test archives, for choices neither source settles.

## Supported archives

A RAR 5 archive is the signature, a main header, a header for each file or service record, and an end of archive header. Each header carries a CRC-32 of its bytes. The reader supports these features:

- Stored entries and the five compression methods, with dictionaries from 128 KiB to 4 GiB. Compression version 0 and version 1 with the flag that marks a version 0 stream are read.
- The delta, E8, E8E9 and ARM filters.
- Solid archives, where each file continues the window, tables and distances of the file before it.
- Folders, Unix and Windows symlinks and hard links. A Windows symlink's `\` separators become `/`, and its target passes the same checks as a Unix one, so an absolute Windows target is refused.
- CRC-32 and BLAKE2sp file checksums. BLAKE2sp is Zipper's own safe Rust in `src/blake2sp.rs`.
- Service headers, such as comments, quick open data and NTFS streams. The reader skips them.
- Bytes after the end of archive header, which the reader ignores, as RAR does.

The reader refuses these archives, and publishes nothing:

- An archive whose headers are encrypted, and an entry with an encryption record. The error suggests `--zip-engine backend`.
- A volume of a set, and an entry that continues into or from another volume. The error suggests `--zip-engine backend`.
- A self-extracting archive. The executable reader sees the PE file and refuses the data after it, as it does for every archive after an executable.
- Junctions and file copies. The error suggests `--zip-engine backend`.
- RAR 7 compression, a file of unknown size, and a compression method or host OS RAR 5 does not define.

## Names

A name is UTF-8 with `/` between folders, as RAR 5 writes it. Names then pass the same checks as tar names, so absolute names, `..`, backslashes, drive letters and empty names are refused. A file with a version record extracts as `[VER]/N/name`, as 7-Zip names it.

## Metadata

- A file gets its modification time, to the nanosecond when the time record carries it. RAR 5 times are UTC.
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
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 1,000,000 |
| One header | at most 2 MiB, as the technote allows |
| One name | at most 8,192 bytes |
| All names | at most 64 MiB |
| Dictionary | at most 4 GiB, the RAR 5 maximum |
| Decoder memory | the window, grown as the stream writes it up to the dictionary, one block of up to 16 MiB and one filter of up to 4 MiB |

The window grows only as data arrives, so a small file declaring a 4 GiB dictionary allocates no more than about twice its size. Growth uses a fallible allocation, so a window the machine cannot hold is an error, not an abort. `--threads` has no effect on RAR.

## Differences from 7-Zip

No RAR writer is installed here. The Homebrew build of 7-Zip 26.05 has no RAR codec, so it lists every RAR but extracts only stored entries. `tests/rar_corpus.rs` therefore checks the archives of libarchive's BSD licensed test suite in `tests/data/rar-corpus`, recorded by `scripts/record_rar_corpus.py`. Each readable archive must extract to the tree `7zz x` extracts where 7-Zip decodes it, and to the tree bsdtar extracts where it does not. Each damaged or unsupported one must be refused. `tests/hostile_rar.rs` cuts the corpus archives at every byte, flips two bits in every byte, and builds hostile headers with its own writer. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip refuses a main header with bytes after its fields. Zipper skips them, as libarchive does.
- 7-Zip extracts an archive cut inside or before its end of archive header. Zipper refuses it.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options). An entry the selection leaves out is still decoded, since a solid stream needs it. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries.

## Formal checks

Kani proves that the number reader never panics and ends on a byte without the high bit, within 10 bytes. It proves that the general header reader keeps its fields and extra area inside the header. The extra record reader keeps every link target inside its record, and the file header reader keeps the name inside its fields. Every dictionary the compression information gives lies from 128 KiB to 4 GiB. The block header reader reads only its own 3 to 5 bytes and gives a size below 16 MiB. The proofs are under `[rar-fields]` in `verification/coverage.txt`. Tests under `[rar5-lz]` check the decoder and BLAKE2sp. The Huffman tables decode every code as a bit-at-a-time walk does, and each filter undoes its encoding. A block writer in the tests builds literals, matches, repeats and filters, which must decode as a byte-at-a-time model does, and builds the matches, filters and sizes the decoder must refuse. BLAKE2sp matches Python's `hashlib` on every split. The corpus and hostile tests run under the same section.
