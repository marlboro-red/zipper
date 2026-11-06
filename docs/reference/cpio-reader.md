# In-process cpio reader

Zipper reads cpio archives with its own Rust reader. The default engine sends a file here when it starts with a cpio header. That is `070701` or `070702` for newc, `070707` for odc, or the binary magic in either byte order. A file that starts with a ZIP, tar or 7z signature goes to that reader first. A cpio inside gzip or another compressor goes to the [single compressed file reader](stream-reader.md), which writes the `.cpio` file, as 7-Zip does. `--zip-engine native` also reads cpio in process, and `--zip-engine backend` hands it to 7-Zip.

## Supported archives

The reader supports these features:

- The binary form `cpio -H bin` writes, in little-endian or big-endian byte order.
- The odc form `cpio -H odc` writes, with octal fields.
- The newc form `cpio -H newc` writes, with hexadecimal fields, and its CRC variant `070702`. The reader checks the CRC variant's sum for every file.
- Files, directories, symlinks that stay inside the destination, and hard links.
- A leading `./` and the `.` entry that `find . | cpio -o` writes.

GNU cpio and bsdcpio store a newc hard link once. Every name but the last carries no data, and the last carries the file. The reader links each name with the same inode and device to the name that carries the data. It keeps an inode's group until the group has its data and as many names as the link count says, then frees it. So an archive of complete pairs holds no link records. A name without data that comes after its group is freed, because the link count was too low, starts a new group. It waits for the next name of that inode that carries data, and without one it becomes an empty file, as bsdcpio writes it. A group in which no name ever carries data becomes one empty file and links to it. While a group still expects names, its data name cannot be replaced by an entry that follows, so a link never reaches another file's data. Each name without data passes the name checks and counts toward the name limit when it arrives. A file whose other links lie outside the archive, as in a snapshot of a hard-linked backup, keeps one record until the end. In the binary and odc forms each name carries its own copy of the data, so each name becomes its own file, as 7-Zip writes it.

The reader does not support these features:

- Devices, FIFOs and sockets.
- Names and link targets that are not UTF-8.
- Several archives one after another, as a Linux initramfs holds them.
- The cpio inside an RPM package, which the [RPM reader](rpm-reader.md) extracts.

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
- No entry sits inside a file or a link, and no entry reuses a name of another kind. A repeated file replaces the earlier one, unless a hard link names it or a newc group still waits on it. A repeated directory merges.
- A symlink target is at most 4,096 bytes, checked before it is read, and follows the [ZIP symlink rule](zip-reader.md#symlinks). Links are created after every file, so no file is written through a link.
- A directory carries no data.
- The archive ends with the `TRAILER!!!` entry, which carries no data. Only zero bytes may follow it.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Headers | at most 1,000,000 |
| Names, implied directories, link targets and hard link names waiting for their data | at most 64 MiB in total |
| Records of newc inodes, hard links waiting for their data and names that carry data | at most 128 MiB, counting each table entry at 3.5 times its size and each waiting record at twice its size plus its heap block |
| One name | at most 8,192 bytes |
| One symlink target | at most 4,096 bytes |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The link budget is charged for the memory a record really takes, including a hash table while it grows. So an archive of about 290,000 newc names that never get their data is refused at 128 MiB, where a looser charge would accept it at about 150 MiB.

One thread reads the archive and writes each file in archive order through a 1 MiB buffer, so memory does not grow with file size. `--threads` has no effect on cpio.

## Differences from 7-Zip

`tests/parity.rs` requires the trees of the binary, odc and newc archives in the [parity corpus](parity.md) to match 7-Zip's. `tests/hostile_cpio.rs` compares every form with 7-Zip where it is installed. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes a device, FIFO or socket as an empty file. Zipper refuses the archive.
- 7-Zip removes a leading `/` and `..` from a name and writes the file. Zipper refuses the archive.
- 7-Zip skips a symlink that leaves the destination and writes the rest. Zipper refuses the archive. 7-Zip also skips a link whose target starts with `..` but stays inside, and Zipper creates it.
- 7-Zip ignores anything after the trailer. Zipper refuses it unless it is all zero bytes.
- A newc name without data that comes after the name carrying the data becomes an empty file in 7-Zip. Zipper links it to the data while the group still expects names, by the rule above.

## Differences from bsdcpio

The newc hard link rule follows bsdcpio 3.5.3, built on libarchive 3.7.4, except in these cases:

- When two names of one inode both carry data, bsdcpio gives every name the data of the last one. Zipper links the names that wait to the first data name, and writes the second as its own file.
- When a name without data repeats the name of an earlier file, bsdcpio keeps the earlier file and links the names of the inode that follow to it. Zipper writes an empty file in its place and links those names to the empty file. It refuses the archive instead when the earlier file is one a hard link names or a group still waits on.
- When a name's link count differs from the first header of its inode, bsdcpio starts a new group at the change. Zipper keeps one group, so names that bsdcpio leaves empty get the data.
- A name without data that shares the inode of a symlink or a folder becomes an empty file. bsdcpio makes it a symlink, or skips it with an error for a folder.
- When a second file with data takes the name of a data name, bsdcpio keeps the first file and leaves the inode's waiting names empty. Zipper writes the second file and links the waiting names to it, unless a group still waits on the first file, which Zipper refuses.
- bsdcpio extracts these shapes, which Zipper refuses because a name would change kind or replace a linked file:
  - a waiting name, then a file of the same name with data;
  - a file that a link names, then a waiting name of the same spelling;
  - a waiting name and its data under the same name, in either order;
  - a file with data that replaces a data name a group still waits on.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [tar reader](tar-reader.md#extraction-options). A selected hard link whose data the selection or strip leaves out is refused. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries, while data is copied, and every 1,024 links while links are created at the end.

## Formal checks

Kani proves that the newc, odc and binary header readers never panic on any header bytes, and that every field they return fits its width. It also proves that hexadecimal and octal fields read back every value they can spell, and that name and data padding aligns every length in each form. The proofs are under `[cpio-fields]` in `verification/coverage.txt`.

`tests/hostile_cpio.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts each form at every byte before the end of the trailer.
