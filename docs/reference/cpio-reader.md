# In-process cpio reader

Zipper reads cpio archives with its own Rust reader. The default engine sends a file here when it starts with a cpio header. That is `070701` or `070702` for newc, `070707` for odc, or the binary magic in either byte order. A file that starts with a ZIP, tar or 7z signature goes to that reader first. A cpio inside gzip or another compressor goes to the [single compressed file reader](stream-reader.md), which writes the `.cpio` file, as 7-Zip does. `--zip-engine native` also reads cpio in process, and `--zip-engine backend` hands it to 7-Zip.

## Supported archives

The reader supports these features:

- The binary form `cpio -H bin` writes, in little-endian or big-endian byte order.
- The odc form `cpio -H odc` writes, with octal fields.
- The newc form `cpio -H newc` writes, with hexadecimal fields, and its CRC variant `070702`. The reader checks the CRC variant's sum for every file.
- Files, directories, symlinks that stay inside the destination, and hard links.
- A leading `./` and the `.` entry that `find . | cpio -o` writes.

GNU cpio and bsdcpio store a newc hard link once. Every name but the last carries no data, and the last carries the file. The reader links each name with the same inode and device to the name that carries the data. A group in which no name carries data becomes one empty file and links to it. In the binary and odc forms each name carries its own copy of the data, so each name becomes its own file, as 7-Zip writes it.

The reader does not support these features:

- Devices, FIFOs and sockets.
- Names and link targets that are not UTF-8.
- Several archives one after another, as a Linux initramfs holds them.
- The cpio inside an RPM package, which stays on the 7-Zip backend.

## Metadata

The reader restores permission bits and times the way the [tar reader](tar-reader.md#metadata) does:

- Each file and directory gets its archive mode, masked to `0o777`. The umask limits it.
- Directory modes and times are applied after all files are written, deepest first.
- Files get their modification time in whole seconds.
- Hard links share one file. Symlinks get no time or mode.

It never restores owners, devices or the inode numbers the archive stores.

## Safety checks

The reader checks the whole archive before it publishes anything, and it publishes nothing on failure:

- Every header has the form of the first one. Each newc field holds only hexadecimal digits, and each odc field only octal digits.
- A name size runs from 1 to 8,192 bytes, including the NUL that must end the name. The reader checks it before it reads the name.
- Names pass the same checks as tar names after a leading `./` is removed. Absolute names, `..`, drive letters, backslashes and NUL bytes are rejected.
- No entry sits inside a file or a link, and no entry reuses a name of another kind. A repeated file replaces the earlier one, unless a hard link names it. A repeated directory merges.
- A symlink target is at most 4,096 bytes, checked before it is read, and follows the [ZIP symlink rule](zip-reader.md#symlinks). Links are created after every file, so no file is written through a link.
- A directory carries no data.
- The archive ends with the `TRAILER!!!` entry, which carries no data. Only zero bytes may follow it.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Headers | at most 1,000,000 |
| Names, implied directories, link targets and hard link names waiting for their data | at most 64 MiB in total |
| One name | at most 8,192 bytes |
| One symlink target | at most 4,096 bytes |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

One thread reads the archive and writes each file in archive order through a 1 MiB buffer, so memory does not grow with file size. `--threads` has no effect on cpio.

## Differences from 7-Zip

`tests/parity.rs` requires the trees of the binary, odc and newc archives in the [parity corpus](parity.md) to match 7-Zip's. `tests/hostile_cpio.rs` compares every form with 7-Zip where it is installed. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes a device, FIFO or socket as an empty file. Zipper refuses the archive.
- 7-Zip removes a leading `/` and `..` from a name and writes the file. Zipper refuses the archive.
- 7-Zip skips a symlink that leaves the destination and writes the rest. Zipper refuses the archive. 7-Zip also skips a link whose target starts with `..` but stays inside, and Zipper creates it.
- 7-Zip ignores anything after the trailer. Zipper refuses it unless it is all zero bytes.
- A newc name without data that comes after the name carrying the data becomes an empty file in 7-Zip. Zipper links it to that file, as GNU cpio does.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [tar reader](tar-reader.md#extraction-options). A selected hard link whose data the selection or strip leaves out is refused. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while data is copied.

## Formal checks

Kani proves that the newc, odc and binary header readers never panic on any header bytes, and that every field they return fits its width. It also proves that hexadecimal and octal fields read back every value they can spell, and that name and data padding aligns every length in each form. The proofs are under `[cpio-fields]` in `verification/coverage.txt`.

`tests/hostile_cpio.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts each form at every byte before the end of the trailer.
