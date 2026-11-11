# In-process WIM reader

Zipper extracts Windows Imaging files (`.wim`) with its own Rust reader. The default engine and `--zip-engine native` send a file here when it starts with the WIM signature `MSWIM` and three zero bytes. The default engine sends a part of a split WIM, and a header from a WIM version before 1.13, to 7-Zip instead. `--zip-engine native` refuses both. `--zip-engine backend` hands every other WIM to 7-Zip, but refuses one whose header names LZX or LZMS before 7-Zip starts.

## Supported images

A WIM is a header, a blob table, an XML description and the resources the blob table lists. Each image has one metadata resource that holds its security data and its directory tree. Every other resource is the data of one or more files, found by its SHA-1. The reader supports these features:

- The WIM 1.13 header that `wimlib-imagex`, DISM and 7-Zip write, and the ESD header of version 0.14.
- Stored resources, and chunked resources compressed with XPRESS. A chunk whose packed size equals its size is stored, as the format allows.
- XPRESS chunks of 4 KiB to 64 KiB.
- Solid resources with XPRESS chunks, in an image whose header names no other method. Many files share one solid resource, and each file is a range of its decoded bytes.
- Several images in one file.
- Hard links, symbolic links, junctions and WSL links, as the next section describes.

The XPRESS decoder in `src/xpress.rs` is Zipper's own safe Rust, written from wimlib's and Microsoft's descriptions of the format and from 7-Zip's decoder behaviour.

An image whose header names LZX or LZMS, and a solid resource compressed with either, is refused with `WIM image is compressed with LZX, which Zipper does not decode` or its LZMS and solid forms, and nothing is written. Every ESD file and every solid WIM that wimlib writes compresses its metadata with LZMS, so each is refused. The LZX and LZMS decoders were removed on 12 November 2026 for licence reasons, as the [refused formats page](refused.md) records.

## What it writes

The reader writes each image as `7zz x` writes it on macOS and Linux:

- One image is written at the top of the destination. Several images are written as folders `1`, `2` and so on, with the XML description beside them as `[1].xml`, as the file stores it in UTF-16.
- Names are read from UTF-16. A `/` in a name becomes `_`, as 7-Zip writes it.
- Files of one image that share a hard link group and their data are hard linked, as `7zz x` links them. Files that share data without a link group are written as separate files.
- A symbolic link whose reparse data says it is relative becomes a symbolic link, with its backslashes turned into slashes. An absolute symbolic link or a junction is read against the root of its image and written as a relative link to the same place. A WSL link becomes a symbolic link to its target. Each link must stay inside the destination.
- Files get their modification time. A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. The umask limits both. Folders get mode `0o755` and their time, applied after all files are written.

It never restores security descriptors, short names, the hidden, system or archive attributes, or the Unix data that `wimlib-imagex capture --unix-data` stores.

## Alternate data streams

A WIM can store NTFS alternate data streams beside a file's data. 7-Zip 26.05 on macOS writes each one as a file named `file:stream`, both by default and with `-sns`. With `-sns-` it skips them. Zipper's shared name check refuses a `:` in every reader, so by default Zipper refuses a WIM with a named stream. The error names `--alternate-streams` and `--zip-engine backend`.

`zipper extract --alternate-streams`, or `alternate_streams: true` in `ExtractOptions`, writes them on Windows:

- Each named stream of a file is written as the NTFS stream `file:stream` through `src/ntfs_streams.rs`. It is written after the file's data and before the file gets its time and read-only attribute back, inside the staging folder. So a failure anywhere publishes nothing, as for every other entry.
- A read-only file is made writable while its streams are written, then made read-only again.
- A stream of a hard link group is written once, with the first file of the group, and the other names share it. Every file of a group must list the same streams with the same data, or the WIM is refused.
- A stream counts against free space or `--max-extracted-bytes`, like a file, also when no destination is given. Its data passes the SHA-1 check of its resource.
- A file that the selection or strip leaves out gets no streams, and their data is still checked.
- With `--content-only`, a stored `Zone.Identifier` stream is not written, as no Mark of the Web is. Without it, the archive's own mark, when it has one, replaces a stored `Zone.Identifier`.
- A destination whose file system keeps no named streams, such as FAT, is refused.

The option checks every stream name as the walk of the directory tree reaches it, on every platform, before any stream is written. A name is refused when it is empty or not valid UTF-16, or when it is longer than 255 UTF-16 units, as NTFS allows. It is also refused when it holds a control character or one of `\ / : * ? " < > |`, or when it ends with a dot or a space, which Windows drops. A Windows device name such as `CON`, `NUL`, `COM1` or `LPT9`, with or without an extension, is refused. Two streams of one file whose names differ only in case are refused, since NTFS compares stream names that way. Case is compared one UTF-16 unit at a time, as NTFS does, so `ß` and `SS` are two names. A stream whose data the blob table lacks is refused. A named stream on a folder, a link or the image root is refused, because a staged folder merges into an existing destination and its streams would not move with it.

Off Windows the option keeps a WIM without named streams extracting as before. A WIM with a named stream is refused before anything is written, and the error says that Zipper writes NTFS streams only on Windows. A library call with the option and no destination reads every stream and checks its SHA-1 on every platform. `zipper test` and `zipper list` take no such option, so they refuse a WIM with a named stream. Every reader other than WIM refuses the option.

wimlib stores a second unnamed stream with no data beside each link. 7-Zip writes it as an empty file `name:_` beside the link. Zipper skips it. An unnamed second stream that holds data is refused.

## Safety checks

The reader checks the whole layout before it publishes anything, and it publishes nothing on failure:

- The header has the signature, a size of 208 bytes, version 1.13 or 0.14, a known method and a chunk size that is a power of two of at least 4 KiB. A file that is a part of a split WIM is refused, and the error names its part.
- Every resource lies inside the file and past the header. No two resources share a byte. A stored resource packs exactly its size.
- The blob table is a whole number of 50-byte entries in part 1. No hash appears twice, and no data entry lacks a hash. An image's metadata is used by at most one image, and the header's image count matches the blob table. The boot metadata, when the header names one, is the metadata of the image the boot index names.
- A solid resource has a known method, a chunk table inside it and chunks that fill it exactly. A solid blob lies inside one solid resource of its run.
- A chunk table's offsets never go backward or past their resource. A chunk is decoded only when it packs to less than the chunk size, as 7-Zip decodes it.
- Every resource is checked against its SHA-1, as 7-Zip checks it. That covers each file's data, each link's reparse data and each image's metadata. The XML and the blob table have no hash.
- The security data fills its declared length. Each directory entry and stream entry is aligned to 8 bytes, has a length that fits the metadata and holds its names. A name has no NUL inside it and ends with one.
- Each 8-byte unit of metadata is claimed by at most one directory entry, stream entry or list end. So a folder list that points back at an earlier list, or two folders that share a list, are refused, and the walk cannot loop.
- The root list holds one unnamed folder, as WIM 1.13 writes it. A link folder with entries under it is refused.
- Names pass the same checks as tar names. `..`, `.`, empty names, backslashes, `:` and NUL are refused, and so is a name that is not valid UTF-16. A folder that holds two entries of one name, after a `/` becomes `_`, is refused.
- With `--alternate-streams`, stream names pass the checks in the section on alternate data streams.
- Every file names data that the blob table holds. 7-Zip writes such a file empty. Zipper refuses the WIM.
- An encrypted ESD is refused.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Blob table | at most 32 MiB, 671,088 entries |
| XML description | at most 64 MiB |
| Metadata of one image | at most 256 MiB |
| Entries in one image | at most 1,000,000, as the shared tree allows |
| Paths of one image | at most 64 MiB in total |
| Reparse data of one link | at most 64 KiB |
| Alternate data streams kept for one image | at most 1,000,000, with at most 64 MiB of names |
| Chunks in one solid resource | at most 2^20 |
| Decoder memory | one chunk and its packed bytes, at most 64 KiB each |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader never holds an image's whole metadata. It reads the metadata once to check its SHA-1, keeping a SHA-1 of each 64 KiB piece, and then parses it through a 1 MiB window that checks each piece it reads again. So metadata that changes on disk between the two reads is refused. The blob table is read in batches of 1,024 entries and kept as one list sorted by hash. Folders, links and empty files reach the shared tree as they are parsed. Only folders and files with data are kept, with their names in one buffer. Files with data are written in the order of their data in the file, so a solid resource is decoded once.

Peak resident memory on images that fill the limits, written by `write_wims_at_the_documented_limits` in `tests/hostile_wim.rs`, is in this table. Each path is about 63 bytes, and each of the first two images holds 223 MB of metadata.

| Image | `zipper list` | `zipper extract` | `7zz l` | Shared tree alone |
| --- | ---: | ---: | ---: | ---: |
| 998,000 empty files in 1,000 folders | 317 MB | | 413 MB | 312 MB |
| 999,000 empty folders in 1,000 folders | 744 MB | 941 MB | 414 MB | 629 MB |
| 670,000 files of 8 bytes, 33.5 MB blob table | 237 MB | | 271 MB | |
| The same, each file in its own hard link group | 237 MB | 224 MB | 271 MB | |

The last column is `zipper list` on a cpio archive of the same paths, which uses the same shared tree. The tree keeps several copies of each folder's path, so on a folder-heavy image Zipper peaks near twice what 7-Zip does. The WIM reader's own share there is about 115 MB. `--threads` has no effect on WIM.

## Differences from 7-Zip

`tests/wim_corpus.rs` requires every WIM in `tests/data/wim-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. Where `wimlib-imagex` is installed, it also captures seeded trees stored and with XPRESS at three chunk sizes. Each image must extract byte for byte to its source tree and to the tree 7-Zip extracts. It also captures six LZX, LZMS and solid images, and each must be refused by its method's name with nothing written. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes an alternate data stream as a file named `file:stream`. Zipper refuses the WIM, and writes the stream as an NTFS stream only on Windows with `--alternate-streams`.
- 7-Zip writes an empty file `name:_` beside each link that wimlib stores. Zipper does not write it.
- 7-Zip refuses a relative link with `..` in it, such as `../hello.txt`, and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip writes an absolute link as an absolute path into the destination, from the destination's root even when the WIM holds several images. Zipper writes a relative link to the same name inside the image's own folder.
- 7-Zip links files of one hard link group across images. Zipper links them only within one image, and writes each image's own copy.
- 7-Zip extracts a folder named `.` into its parent, and a second entry of one name over the first. Zipper refuses both.
- 7-Zip writes a file whose data the blob table lacks as an empty file, and lists unreferenced data under `[DELETED]`. Zipper refuses a file without its data, and ignores data that no file names.
- 7-Zip extracts a root list with more than one entry, an older WIM version and a split WIM. Zipper refuses each in every engine, since it hands no WIM to 7-Zip.
- 7-Zip fixes a name without its terminating NUL and extracts it with a warning. Zipper refuses it.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes, with the image folder first when there are several images. A hard link whose first name the selection or strip leaves out is written from the next selected name of its group. An absolute link is made relative for its place before the strip, so a strip that moves it can make it point outside the destination, and then the WIM is refused. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[wim-fields]` in `verification/coverage.txt`:

- The header reader never panics on any 208 bytes. A header it accepts has a chunk size of 4 KiB to 2 GiB, a part inside its part count, a method exactly when its compressed flag is set and a 56-bit blob table size.
- The blob entry reader never panics on any 50 bytes and keeps a 56-bit packed size, its flags and its hash.
- A chunk table it accepts fits inside its resource, and every chunk holds 1 byte to one chunk size and ends inside the resource's size. Kani found an overflow here, for a chunk size below 4 KiB, before the reader was first committed.
- A solid resource header it accepts has a chunk table that fits inside its resource, at most 2^20 chunks and chunks that cover its size.
- The directory entry reader never panics on any record of up to 128 bytes and any position and metadata size. An entry it accepts is aligned, fits the metadata and keeps its name and the name's NUL inside itself. The same holds for stream entries of up to 64 bytes, and the security data reader ends aligned inside its metadata.
- The XPRESS Huffman table, built from any 6 code lengths with a 2-bit lookup table, decodes every 15-bit value to a symbol whose code length it returns. So the decoder's slow path always finds a symbol for a complete code.

Unit tests in `src/xpress.rs` decode 400 encoded token streams of every match length form back to their tokens. They refuse each stream cut by 1 to 3 bytes or decoded to a size one byte off, and decode every flipped byte without a panic.

`tests/hostile_wim.rs` builds its WIMs with the test writer in `tests/common/wim_writer.rs`, since no tool here writes damaged images. It holds one case per refusal in the safety checks above, each of which must leave nothing beside the destination, both when extracting and when only verifying. It cuts a stored and an XPRESS WIM at every byte, and flips a bit in every byte outside the file data and in every 37th byte of it. A flipped WIM must be refused or extract exactly the files it stores.

`tests/wim_corpus.rs` also flips one bit in every 31st byte of the data of three corpus WIMs, `xpress.wim`, `xpress-4k.wim` and `two-images.wim`. `links.wim` is left out, because a flip in its reparse data fails Zipper's SHA-1 check and 7-Zip extracts it. Zipper must refuse each one exactly when 7-Zip refuses it, and must otherwise extract the same tree.

An adversarial review built crafted WIMs in Rust with the test writer and ran them against both native engines and 7-Zip. It found three problems, each fixed with a test that failed first. A chunk table could name a packed size of up to 2^50 bytes in a sparse file, and the reader allocated it before it checked the size, which crashed the process. A second file with the name of a hard link's first file could replace it, so the link carried the other file's bytes. A link group per file kept one path per group in memory. It also found that a folder named `.` was merged into its parent. 38,000 random mutations of wimlib images caused no panic.

`tests/data/wim-corpus/NOTICE.txt` names the source of the corpus. Every WIM there was written by `wimlib-imagex` 1.14.5 on macOS from seeded trees, through `write_the_corpus` in `tests/wim_corpus.rs`. No sample there comes from Microsoft's own tools.
