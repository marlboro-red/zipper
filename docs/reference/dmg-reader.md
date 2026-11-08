# In-process DMG reader

Zipper extracts Apple disk images (`.dmg`) and the HFS+ and HFSX volumes inside them with its own Rust reader. It also reads a raw HFS+ or HFSX volume and a raw disk with an Apple partition map. The default engine sends a file here when the volume inside is HFS+, HFSX or APFS, and this reader hands an APFS container to the [APFS reader](apfs-reader.md). It finds a DMG by the UDIF `koly` trailer at its end, or rarely at its start, and a raw disk by its HFS+ volume header or Apple partition map. It sends a DMG or disk that holds FAT or another file system, or several file systems, to 7-Zip instead. `--zip-engine native` refuses those by name, and the error suggests `--zip-engine backend`. `--zip-engine backend` hands every image to 7-Zip.

## Supported images

A UDIF image is a data fork of chunks, a list of partitions and a 512-byte `koly` trailer. Each partition has a `mish` table of blocks, and each block is a run of sectors stored in one chunk. The reader supports these features:

- The trailer at the end of the file, at its start, or at the end of an image embedded at an offset in a larger file, found as 7-Zip finds it.
- The partition list as an XML property list, read with 7-Zip's own XML rules, or as a resource fork, which older images use and 7-Zip still reads.
- Blocks of zeros, blocks to ignore, raw blocks, and chunks compressed with ADC, zlib, bzip2, LZFSE or xz. `hdiutil` writes these as UDZO, UDBZ, ULFO, ULMO, UDRO and UDCO.
- The one partition whose name marks it as a file system, as 7-Zip picks it. Names with `Apple_partition_map`, `MBR`, ` GPT `, `Apple_Free`, `DDM`, `Driver`, `Patches` or the EFI system partition's GUID are not file systems. Names with `Apple_HFS`, `Apple_HFSX`, `Apple_APFS` and other names are.
- Inside that partition, an HFS+, HFSX or APFS volume, or an Apple partition map, a GPT or an MBR with one such volume, as 7-Zip opens them. The same holds for a raw disk with an Apple partition map.
- HFS+ inside an HFS wrapper, as old images store it.

ADC, LZFSE and LZVN are Zipper's own decoders in safe Rust, in `src/adc.rs`, `src/lzfse.rs` and `src/lzvn.rs`. They follow 7-Zip's end rules, which are stricter than Apple's. A stream must fill its declared size exactly and end exactly at the end of its input. LZFSE version 1 blocks, which no encoder writes and 7-Zip refuses, are refused. zlib and bzip2 use the `flate2` and `bzip2` crates, and xz uses Zipper's own decoder in `src/lzma.rs`.

## What it writes

The reader writes the volume as `7zz x` writes it on macOS and Linux:

- The volume's own folder, named after the volume, is at the top of the destination.
- Names are read from UTF-16 and kept as the volume stores them, decomposed as HFS+ stores accents. A `/` in a name becomes `_`. An empty name and a name of one space become `[]`. The private folder that holds hard link data becomes `[HFS+ Private Data]`.
- A file's mode is applied when its mode says it is a regular file. A folder's mode is applied, with the owner's read, write and search rights added, when its mode says it is a folder. Other entries get `0o666` or `0o777`. The umask limits all of them, and setuid, setgid and sticky bits are dropped.
- Files get their modification time. Folders get theirs after all files are written.
- A file whose mode says it is a symbolic link becomes a link to the target its data holds. Each link must stay inside the destination.
- A hard link becomes a hard link to its data in `[HFS+ Private Data]/iNodeN`, which is also written there.
- A file compressed with HFS+ compression is written decompressed. It may keep its data in its `com.apple.decmpfs` attribute, stored or compressed with zlib, LZVN or a copy, methods 3, 7 and 9. Or it may keep 64 KiB chunks of zlib or LZVN in its resource fork, methods 4 and 8. These are the methods 7-Zip decodes.

It never restores owners, flags, Finder information, access or creation times, or ACLs.

## Resource forks and extended attributes

7-Zip writes a file's resource fork on macOS and Linux as a file named `file:rsrc`, and each extended attribute as a file named `file:name`. Zipper's shared name check refuses a `:` in every reader. So Zipper refuses an image with a resource fork that is not compressed data, and the error names the file and suggests `--zip-engine backend`. It skips extended attributes, as the xar reader does, because macOS adds `com.apple.provenance` to most files and refusing them would refuse most images.

## Safety checks

The reader checks the layout before it writes each file, and it publishes nothing on failure:

- The trailer has the `koly` signature, version 4 and size 512, and its forks lie before it. Each partition's table has the `mish` signature, version 1 and a size that fits its block count. The partition it opens has blocks that follow one another from sector 0, end with an end record and count its declared sectors.
- The partition it opens decodes every block to exactly its size, from data inside the file. A zero block holds no data, and a raw block holds exactly its size. A chunk larger than 256 MiB, which 7-Zip will not open, is left to 7-Zip.
- After extraction the reader checks the partition's CRC-32 over every block 7-Zip counts, decoding any block the volume did not read. It checks the master checksum of the partitions' checksums, and the data fork's CRC-32 when the trailer sets one.
- A partition map's entries, and a GPT's header and entry table, lie inside the disk that holds them. The table is checked against the disk's size before it is read.
- The HFS+ volume header has the signature and version pair, a block size from 512 bytes to 1 GiB and counts within 7-Zip's limits. Its blocks fit inside the partition.
- The extents, catalog and attributes B-trees have a node size from 512 bytes to 32 KiB and no more nodes than their file holds. Their leaves are walked from the first leaf, and a leaf link that loops, leaves the file or reaches a node that is not a leaf is refused. Every record offset in a node rises and stays before the offset table.
- Every fork's extents hold exactly its block count, from the extents overflow file when it has more than eight, and lie inside the volume. No two extents share a block. A file's blocks hold exactly its size.
- The catalog's counts match the volume header, no two entries share a number, and every entry lies in a folder the catalog holds, below the root folder. Folders nest at most 1,024 deep and never in a loop. A folder that holds two entries of one name is refused.
- A compressed file has no data fork of its own. Its decompressed data, chunk table and chunks stay within the sizes it declares, and a chunk decodes to exactly its block.
- A hard link names data that the private folder holds. A hard link to a folder, which Time Machine writes, is refused.
- Names pass the same checks as tar names. `..`, `.`, backslashes, `:` and NUL are refused, and so is a name that is not valid UTF-16.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| XML property list | at most 64 MiB |
| Partitions | at most 4,096 |
| Blocks in one partition | at most 4,194,304 |
| One compressed chunk | at most 256 MiB, as 7-Zip allows |
| Decoded chunks held | at most 64 chunks and 64 MiB, and always the chunk being read |
| Nodes in one B-tree | at most 16,777,216 |
| Entries and attributes | at most 1,000,000 each, as the shared tree allows |
| Names and folder paths | at most 64 MiB each |
| Partition maps inside one another | at most 4 deep |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader never holds an image's data. It decodes the partition one chunk at a time into a small cache, and walks each B-tree one node at a time. It keeps one entry per catalog record, with its name in one buffer, and where each compressed file's decmpfs attribute lies, which it reads again when it writes that file. So memory follows the catalog, not the image. On a DMG of 999,001 entries in 1,000 folders, `zipper test` peaked at 665 MB of resident memory and `7zz t` at 957 MB. On the 6 GiB sparse corpus image, `zipper test` peaked at 8 MB and `7zz t` at 37 MB. `--threads` has no effect on DMG.

## Differences from 7-Zip

`tests/dmg_corpus.rs` requires every DMG in `tests/data/dmg-corpus` to extract to the tree recorded when it was written, and to the tree 7-Zip extracts where it is installed. It also compares generated HFS+ and HFSX volumes with every chunk method, both partition list forms and Apple, GPT and MBR partition maps. They hold compressed files of every method 7-Zip decodes, hard links, links, modes, folders 60 deep and 1,500 wide, and Unicode names. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes each hard link as an empty file with the link's own mode. Zipper writes it as a hard link to its data, with that data's mode.
- 7-Zip writes resource forks and extended attributes as `file:rsrc` and `file:name` files. Zipper refuses a resource fork and skips extended attributes.
- 7-Zip refuses a relative link with `..` in it, such as `../hello.txt`, and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip opens a DMG's file system without checking any CRC-32, and only warns when the master or data fork checksum fails. Zipper checks the partition's CRC-32 and refuses the image when any check fails, also when a block the volume never reads is damaged.
- 7-Zip only warns when a DMG's trailer or other partitions are inconsistent, as Zipper also accepts. It reads a zero block that holds data, and a raw block whose data size differs from its own, without complaint. Zipper refuses both.
- 7-Zip writes an entry whose folder the catalog lacks at the top of the destination. It writes a hard link whose data is missing as an empty file, and cuts a name at its first NUL. Zipper refuses each.
- 7-Zip extracts files whose extents share blocks, and an entry nested more than 1,024 folders deep under the wrong path. Zipper refuses both.
- 7-Zip extracts a DMG with several file system partitions, or whose file system it cannot open, as raw partition files. Zipper refuses it, and the default engine sends it to 7-Zip.
- 7-Zip refuses a fork whose extents overflow records continue into the next B-tree node. Zipper reads it.
- 7-Zip decodes a decmpfs attribute attached to a hard link record as that link's data. Zipper refuses a compressed hard link record.
- 7-Zip reads a zlib compressed file whose resource data does not start at byte 256, from byte 256. Zipper refuses it.
- 7-Zip opens an MBR whose only partition is a protective or extended one. Zipper leaves such a disk, and an MBR that may use 4 KiB sectors, to 7-Zip.

`tests/hostile_dmg.rs` flips bits in every byte of the trailer, in every 5th byte of the property list or resource fork and every 89th byte of each chunk, and cuts each image every 97 bytes. It flips bits in every byte of the HFS+ volume header, of each B-tree header node's records, of each leaf node's descriptor and offset table, and in every 41st byte of each leaf. These full sweeps are ignored tests that `cargo xtask release-checks` runs under `[dmg-fields]`. The gate runs a sample of each. The sample flips every byte of the trailer's forks, checksum types and sector count, and of the volume header's counts and special forks. It also flips every byte of each B-tree header record and of each leaf's descriptor and offset table, with sparser bytes elsewhere. Zipper must refuse each damaged image whenever 7-Zip refuses it. When 7-Zip extracts it, Zipper must extract the same tree, apart from the hard links above, or refuse it for one of the stricter checks above. Crafted cases cover B-tree loops, leaves outside their file, shared extents, data larger than its declared size, unknown and oversized chunks, resource forks, unsafe names and links, deep and looping folders and repeated names.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes, with the volume's folder first. A hard link whose first name the selection or strip leaves out is written from the next selected name of its data. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[dmg-fields]` in `verification/coverage.txt`:

- The `koly` reader never panics on any 512 bytes. The `mish` header reader never panics on any 204 bytes and any size, and a header it accepts fills exactly its size. A block record it accepts lies in whole sectors below 7-Zip's limit.
- The Apple partition map readers never panic on any sector, and keep a block size of 512 to 4,096 bytes and an entry that ends below block 2^32.
- The HFS+ volume header reader never panics on any 512 bytes and keeps its block size and counts in range. A B-tree header it accepts holds no more nodes than its file. A node descriptor it accepts, on any node of up to 40 bytes, keeps every record inside the node.
- The catalog, extents and attribute record readers never panic on any record of up to 270, 78 and 120 bytes, and keep names and values inside the record. The decmpfs header reader never panics on any 24 bytes, and stored data it accepts is exactly its declared size.
- The ADC decoder never panics on any chunk of up to 8 bytes into an output of up to 6, and the LZVN decoder on any stream of up to 10 bytes. An LZVN stream it accepts ends at its padding and inside its input. The LZFSE block header reader keeps every count and state inside the format's limits.

Unit tests compare the ADC decoder with a byte-at-a-time version on every chunk of up to 6 bytes over 11 bytes that matter. They compare the LZVN decoder with one on every stream of up to 5 opcodes over 12 bytes. They check every LZFSE frequency code, every state of 9,000 random tables, and that zero runs have the CRC-32 of their bytes. Where Apple's `libcompression` is installed, they compress random data with it as LZFSE and as raw LZVN, and the Silesia files too when `ZIPPER_SILESIA` names them. Zipper's decoders must return the input, as Apple's own decoder does. Base64 is checked on every text of up to 7 characters over 8, and the XML reader against 7-Zip's rules.

`tests/data/dmg-corpus/NOTICE.txt` names the source of the corpus. Every image there was written by `hdiutil` on macOS 26, and no file in it comes from a third party.
