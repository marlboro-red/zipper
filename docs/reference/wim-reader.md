# In-process WIM reader

Zipper extracts Windows Imaging files (`.wim`) and ESD images (`.esd`) with its own Rust reader. The default engine and `--zip-engine native` send a file here when it starts with the WIM signature `MSWIM` and three zero bytes. The default engine sends a part of a split WIM, and a header from a WIM version before 1.13, to 7-Zip instead. `--zip-engine native` refuses both. `--zip-engine backend` hands every WIM to 7-Zip.

## Supported images

A WIM is a header, a blob table, an XML description and the resources the blob table lists. Each image has one metadata resource that holds its security data and its directory tree. Every other resource is the data of one or more files, found by its SHA-1. The reader supports these features:

- The WIM 1.13 header that `wimlib-imagex`, DISM and 7-Zip write, and the ESD header of version 0.14.
- Stored resources, and chunked resources compressed with XPRESS, LZX or LZMS. A chunk whose packed size equals its size is stored, as the format allows.
- XPRESS chunks of 4 KiB to 64 KiB, LZX chunks of 32 KiB to 2 MiB and LZMS chunks of 32 KiB to 64 MiB. wimlib writes all of them, and Windows writes the smaller sizes.
- Solid resources, as ESD files and `wimlib-imagex capture --solid` hold them, with LZMS, XPRESS or LZX chunks. Many files share one solid resource, and each file is a range of its decoded bytes.
- Several images in one file.
- Hard links, symbolic links, junctions and WSL links, as the next section describes.

The decoders are Zipper's own safe Rust, written from wimlib's and Microsoft's descriptions of the formats and from 7-Zip's decoder behaviour. XPRESS is in `src/xpress.rs`. LZX is the CAB decoder in `src/lzx.rs`, in a WIM mode. There each chunk is its own stream, E8 calls always translate with a size of 12,000,000, and a block size is 32 KiB when its first bit is set. LZMS is in `src/lzms.rs`, with its adaptive Huffman codes rebuilt as wimlib builds them and its x86 filter.

## What it writes

The reader writes each image as `7zz x` writes it on macOS and Linux:

- One image is written at the top of the destination. Several images are written as folders `1`, `2` and so on, with the XML description beside them as `[1].xml`, as the file stores it in UTF-16.
- Names are read from UTF-16. A `/` in a name becomes `_`, as 7-Zip writes it.
- Files of one hard link group that share their data are hard linked, as `7zz x` links them. Files that share data without a link group are written as separate files.
- A symbolic link whose reparse data says it is relative becomes a symbolic link, with its backslashes turned into slashes. An absolute symbolic link or a junction is read against the root of its image and written as a relative link to the same place. A WSL link becomes a symbolic link to its target. Each link must stay inside the destination.
- Files get their modification time. A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. The umask limits both. Folders get mode `0o755` and their time, applied after all files are written.

It never restores security descriptors, short names, the hidden, system or archive attributes, or the Unix data that `wimlib-imagex capture --unix-data` stores.

## Alternate data streams

7-Zip writes an NTFS alternate data stream on macOS and Linux as a file named `file:stream`. Zipper's shared name check refuses a `:` in every reader, so Zipper refuses a WIM with a named stream, and the error suggests `--zip-engine backend`. The roadmap's Windows option, which writes each stream as an NTFS stream of its file through `src/ntfs_streams.rs`, is not built yet.

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
- Names pass the same checks as tar names. `..`, `.`, empty names, backslashes, `:` and NUL are refused, and so is a name that is not valid UTF-16.
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
| Chunks in one solid resource | at most 2^20 |
| Decoder memory | one chunk and its packed bytes, at most 64 MiB each, and one LZX window of up to 2 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader never holds an image's whole metadata. It reads the metadata once to check its SHA-1, keeping a SHA-1 of each 64 KiB piece, and then parses it through a 1 MiB window that checks each piece it reads again. So metadata that changes on disk between the two reads is refused. The blob table is read in batches of 1,024 entries and kept as one list sorted by hash. Folders, links and empty files reach the shared tree as they are parsed. Only folders and files with data are kept, with their names in one buffer. Files with data are written in the order of their data in the file, so a solid resource is decoded once.

On an image of 998,000 empty files and 1,000 folders whose paths total 63 MB, with 223 MB of metadata, `zipper list` peaked at 316 MB of resident memory and `7zz l` at 413 MB. The shared tree alone took 312 MB for the same paths in a cpio archive. On an image of 670,000 files with 8 bytes of data each and a 33.5 MB blob table, `zipper list` peaked at 237 MB and `7zz l` at 271 MB. `--threads` has no effect on WIM.

## Differences from 7-Zip

`tests/wim_corpus.rs` requires every WIM in `tests/data/wim-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. Where `wimlib-imagex` is installed, it also captures seeded trees with every method at 12 chunk sizes and layouts, solid and not. Each image must extract byte for byte to its source tree and to the tree 7-Zip extracts. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes an alternate data stream as a file named `file:stream`. Zipper refuses the WIM.
- 7-Zip writes an empty file `name:_` beside each link that wimlib stores. Zipper does not write it.
- 7-Zip refuses a relative link with `..` in it, such as `../hello.txt`, and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip writes an absolute link as an absolute path into the destination, from the destination's root even when the WIM holds several images. Zipper writes a relative link to the same name inside the image's own folder.
- 7-Zip writes a file whose data the blob table lacks as an empty file, and lists unreferenced data under `[DELETED]`. Zipper refuses a file without its data, and ignores data that no file names.
- 7-Zip extracts a root list with more than one entry, an older WIM version and a split WIM. Zipper refuses each, and the default engine sends the last two to 7-Zip.
- 7-Zip fixes a name without its terminating NUL and extracts it with a warning. Zipper refuses it.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes, with the image folder first when there are several images. A hard link whose first name the selection or strip leaves out is written from the next selected name of its group. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[wim-fields]` in `verification/coverage.txt`:

- The header reader never panics on any 208 bytes. A header it accepts has a chunk size of 4 KiB to 2 GiB, a part inside its part count, a method exactly when its compressed flag is set and a 56-bit blob table size.
- The blob entry reader never panics on any 50 bytes and keeps a 56-bit packed size, its flags and its hash.
- A chunk table it accepts fits inside its resource, and every chunk holds 1 byte to one chunk size and ends inside the resource's size. Kani found an overflow here, for a chunk size below 4 KiB, before the reader was first committed.
- A solid resource header it accepts has a chunk table that fits inside its resource, at most 2^20 chunks and chunks that cover its size.
- The directory entry reader never panics on any record of up to 128 bytes and any position and metadata size. An entry it accepts is aligned, fits the metadata and keeps its name and the name's NUL inside itself. The same holds for stream entries of up to 64 bytes, and the security data reader ends aligned inside its metadata.
- The XPRESS Huffman table, built from any 6 code lengths with a 2-bit lookup table, decodes every 15-bit value to a symbol whose code length it returns. So the decoder's slow path always finds a symbol for a complete code.

Unit tests in `src/xpress.rs` decode 400 encoded token streams of every match length form back to their tokens. They refuse each stream cut by 1 to 3 bytes or decoded to a size one byte off, and decode every flipped byte without a panic. Unit tests in `src/lzms.rs` check that every Huffman code built from 3,000 frequency sets is complete within 15 bits. They check that the x86 filter matches a separate byte-at-a-time version on 20,000 inputs, and that 20,000 random chunks decode or are refused without a panic. The LZX WIM mode is checked under `[lzx]`, and LZMS under `[lzms]`.

`tests/hostile_wim.rs` builds its WIMs with the test writer in `tests/common/wim_writer.rs`, since no tool here writes damaged images. It holds one case per refusal in the safety checks above, each of which must leave nothing beside the destination, both when extracting and when only verifying. It cuts a stored and an XPRESS WIM at every byte, and flips a bit in every byte outside the file data and in every 37th byte of it. A flipped WIM must be refused or extract exactly the files it stores.

`tests/data/wim-corpus/NOTICE.txt` names the source of the corpus. Every WIM there was written by `wimlib-imagex` 1.14.5 on macOS from seeded trees, through `write_the_corpus` in `tests/wim_corpus.rs`. No sample there comes from Microsoft's own tools.
