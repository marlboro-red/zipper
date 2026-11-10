# In-process SquashFS and CramFS readers

Zipper extracts SquashFS 4.0 images with its own Rust reader, in `src/native_squashfs.rs`, and CramFS images with another, in `src/native_cramfs.rs`. Each reads a raw image and an image in the lone partition of a disk, through the [disk reader](disk-reader.md). The default engine sends an image here when it starts with a little-endian SquashFS 4 superblock or a CramFS superblock in either byte order. It sends SquashFS versions 1 to 3, big-endian SquashFS and the swapped vendor signatures to 7-Zip, since no tool here writes them. `--zip-engine native` refuses those by name, and the error suggests `--zip-engine backend`. `--zip-engine backend` hands every image to 7-Zip.

## Supported images

The SquashFS reader follows the layout of squashfs-tools 4.7 and checks each structure as 7-Zip 26.05 checks it:

- The 96-byte superblock, with major version 4, a block size from 4 KiB to 1 MiB that matches its logarithm, and tables in their order inside the image.
- Metadata blocks of up to 8 KiB, stored or compressed, for the inode table, the folder table, the fragment table and the owner table.
- Every inode type, basic and extended. These are folders with or without an index, files, links, block and character devices, named pipes and sockets.
- Files whose blocks are stored, compressed or sparse, with a tail in a shared fragment block or in a block of its own.
- Every compressor 7-Zip lists. gzip uses Zipper's own Deflate decoder in `src/inflate.rs` with zlib framing. zstd uses Zipper's own decoder in `src/zstandard.rs`. LZMA and xz use Zipper's own decoder in `src/lzma.rs`, with the xz branch filters. LZO uses Zipper's own LZO1X decoder in `src/lzo.rs`, and LZ4 its own block decoder in `src/lz4.rs`. The compressor options block is skipped, since each block carries what its decoder needs.

The CramFS reader reads images of either byte order with 4 KiB pages compressed with zlib, as `mkfs.cramfs` writes them and 7-Zip reads them. It checks the CRC-32 of a version 2 image over the size its superblock gives. It reads a version 1 image, which has no CRC and no size, up to 256 MiB.

## What it writes

Each reader writes the tree `7zz x` writes on macOS and Linux:

- Entries are listed folder by folder, in the order of each folder's listing, as 7-Zip lists them. The root folder's own inode is not written.
- Files that end in a fragment block are written after the rest, in the order of their tails in the fragment blocks. So each fragment block is read once, whatever order the folders list its files in. `tests/counters.rs` checks this on files that take turns between 9 fragment blocks.
- A file's mode is applied without its setuid, setgid and sticky bits. A folder's mode is applied, with the owner's read, write and search rights added. Devices, named pipes and sockets become empty files of mode `0o666`. The umask limits all of them.
- Files and folders get their SquashFS modification time, in whole seconds. Folders get theirs after all files are written. CramFS keeps no times, so its entries keep the time they were written.
- A link becomes a link to its target. Each link must stay inside the destination.
- Names that share a SquashFS inode become hard links to the first of them that is written. CramFS keeps no inode numbers, so files that share data are written as copies, as 7-Zip writes them.
- A sparse SquashFS block and a CramFS hole are written as zeros. `zipper test` skips a sparse SquashFS block without producing its zeros, so its time depends on the image's size, not the size the file declares.

It never restores owners or extended attributes. 7-Zip ignores SquashFS extended attributes as well, so the SquashFS reader skips the attribute table and never reads it.

## Safety checks

The readers check the layout before they write each file, and they publish nothing on failure:

- Every block, table and index lies inside the image. A data or fragment block lies after the superblock and before the inode table, in the data area.
- Every metadata block holds 1 to 8 KiB on disk and decodes to at most 8 KiB. The inode table's blocks end exactly at the folder table, and the folder table's at the fragment table.
- Every inode of the table parses, and the table holds exactly the count the superblock gives, as 7-Zip parses them. Every reference names the start of an inode. A folder listing starts inside a block of the folder table and ends exactly at its size.
- The fragment and owner tables decode to exactly the entries their counts give.
- A data block word has a length from 1 to the block size, or is zero for a sparse block, and no bits above its stored flag. A stored block holds exactly the bytes its place in the file needs.
- Each block decodes into a buffer of the size its place in the file needs, so a block that decodes past that size is refused while it decodes. An LZMA block must declare its size in its header, no larger than that buffer. A fragment block decodes into one block. A file's tail must lie inside its fragment block.
- No folder appears twice, so a loop is refused. A folder holds no two entries of one name.
- Names pass the same checks as tar names. `..`, `.`, backslashes, `:` and NUL are refused, and so is a `/` or a name that is not UTF-8.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| SquashFS block size | 4 KiB to 1 MiB |
| SquashFS inodes | at most 1,000,001 |
| Entries | at most 1,000,000, as every reader |
| SquashFS fragments | at most one for each inode |
| Names | at most 256 bytes |
| Link targets | at most 4,096 bytes, as every reader, checked before a target is read |
| SquashFS folder table | at most 1,048,576 metadata blocks |
| Names and link targets in all | at most 64 MiB |
| Decoded metadata blocks kept | 64, of up to 8 KiB each |
| xz dictionary | at most the block size, or 8 KiB for smaller blocks |
| CramFS files | at most 524,288, as 7-Zip refuses more |
| CramFS version 1 image | at most 256 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The SquashFS reader never holds a table whole. It reads metadata through a cache of 64 decoded blocks, one data block and one fragment block at a time. It keeps one 8-byte reference for each inode, so it can check that each reference names an inode's start, and one entry for each block of the folder table. It reads the fragment index once and the fragment and owner tables block by block. It never reads the export table. So memory follows the folders, not the data. The CramFS reader reads each inode and block from the image when it needs it. On an image of 998,000 named pipes in 1,000 folders written by `mksquashfs`, `zipper test` peaked at 228 MB of resident memory and `7zz t` at 155 MB. With names long enough to bring the paths near the 64 MiB name limit, the adversarial review measured 355 MB against 265 MB. Most of Zipper's peak is the shared list of entry names that every reader keeps to refuse duplicates and report entries. `--threads` has no effect on either reader.

## Differences from 7-Zip

`tests/squashfs_corpus.rs` requires every image in `tests/data/squashfs-corpus` to extract to the tree recorded when it was written, and to the tree 7-Zip extracts where it is installed, with modes and SquashFS times. It does the same for images that `mksquashfs` writes of every compressor at block sizes of 4 KiB, 64 KiB and 1 MiB. It also covers nine table and compressor options, and `mkfs.cramfs` images of both byte orders. Each is also checked raw, in a GPT partition, and against the files it was written from. The readers differ from 7-Zip 26.05 on purpose in these cases:

- 7-Zip does not open SquashFS images compressed with LZ4 and reports that the method is not implemented. Zipper reads them, and the tests check them against `unsquashfs` and against the files they were written from.
- 7-Zip writes each name of a SquashFS hard link as a copy of its data. Zipper writes hard links to one copy.
- 7-Zip refuses a relative link with `..` in it, such as `../words.txt`, and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip restores a link's own modification time. Zipper, as every Zipper reader, does not.
- 7-Zip fails on a CramFS hole, which `mkfs.cramfs -z` writes. Zipper writes zeros, as Linux reads it.
- 7-Zip keeps the first bytes of a block that decodes past the size its place in the file needs, and accepts the image. Zipper refuses it.
- 7-Zip reads a data block word that has bits above its stored flag, a stored length of zero or a length past the block size, and writes bytes from elsewhere in the image. Zipper refuses the word. It also refuses a stored block of the wrong size, and a data or fragment block that lies outside the data area.
- 7-Zip refuses some damaged zstd blocks that libzstd, the reference zstd decoder, decodes without error, such as the `zstd.sqfs` corpus image with bit 4 of byte 151 flipped. Zipper's zstd decoder accepts and refuses what libzstd 1.5.7 does, so it writes what `unsquashfs` writes. The hostile tests accept such a block only when the tree matches `unsquashfs`.
- 7-Zip writes both entries of a folder that holds two of one name. Zipper refuses the folder.
- 7-Zip writes a name that is not UTF-8 in its own way. Zipper refuses it.
- 7-Zip writes a link whose target holds a NUL, and the system cuts the target there. Zipper refuses the link.
- 7-Zip extracts nothing from a CramFS image whose root inode is not a folder, and reports success. Zipper refuses it.
- 7-Zip reads blocks in another layout when CramFS flags set `0x800`, `0x1000`, `0x2000` or `0x8000`. Zipper refuses such an image by name.

`tests/hostile_squashfs.rs` flips a bit in bytes of the superblock, the data area and the tables of images of every compressor but LZ4, and of CramFS images of both byte orders. It reseals a CramFS image's CRC after each flip, so the flip reaches the structure behind it. It also cuts images at many lengths. Zipper must refuse each damaged image whenever 7-Zip refuses it. When 7-Zip extracts it, Zipper must extract the same tree or refuse it for one of the stricter checks above. The gate runs a sample of each sweep. The full sweeps flip every byte of the superblock and tables and every 7th byte of the data, and every byte of the CramFS listings. They are ignored tests that `cargo xtask release-checks` runs under `[squashfs-fields]` and `[cramfs-fields]`. Crafted images come from the test writer in `tests/common/squashfs_writer.rs`. They cover folder loops, references into an inode, blocks that decode past their size, sparse blocks and holes, and inodes that start a metadata block. They also cover unsafe and repeated names, links that leave the destination, and each refusal by name.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. A hard link whose first name the selection or strip leaves out is written from the next selected name of its inode. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[squashfs-fields]` in `verification/coverage.txt`:

- The superblock reader never panics on any 96 bytes. A superblock it accepts has a block size from 4 KiB to 1 MiB, tables in order inside the image and a root inode inside the inode table.
- The inode reader never panics on any base header and body of up to 40 bytes. An inode it accepts has the body length its type needs, a folder listing that starts inside its block, and a file size and link target within their limits.
- The folder run and entry readers never panic, and an entry they accept points inside its inode block.
- A data block word gives a length from 1 to the block size. A file's block count covers its size, or all of it but a tail shorter than one block when the tail is in a fragment.

Under `[cramfs-fields]`, Kani proves that the inode reader unpacks any 12 bytes of either byte order into fields that fit their bits. It also proves that the superblock reader never panics on any 76 bytes and accepts only a folder root. Unit tests read back random inodes packed in both byte orders.

Under `[lzo]`, Kani proves that the LZO1X decoder never panics on any input of up to 6 bytes and never writes past an output of up to 4 bytes. Unit tests decode `lzop` output of levels 1, 3, 7 and 9, kept in `tests/data/squashfs-corpus/lzo`, and fresh `lzop` output where it is installed. Every cut of a stream is refused, and every flipped stream is refused or stays inside its block.

`tests/data/squashfs-corpus/NOTICE.txt` names the source of the corpus.
