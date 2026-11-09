# In-process ext reader

Zipper extracts ext2, ext3 and ext4 volumes with its own Rust reader, in `src/native_ext.rs`. It reads a raw volume and a volume in the lone partition of a disk the [disk reader](disk-reader.md) opens. So an MBR or GPT disk, a VHD, VHDX, QCOW, VDI, VMDK or Android sparse image holding one ext volume extracts in process too. The default engine sends a file here when its superblock at byte 1024 passes the layout checks below. A volume that uses a feature Zipper does not read is refused by name, and the error suggests `--zip-engine backend`. The default engine sends such a volume to 7-Zip. `--zip-engine backend` hands every volume to 7-Zip.

## Supported volumes

The reader follows the kernel's ext4 documentation:

- Blocks of 1 KiB to 64 KiB, inodes of 128 bytes up to the block size, and revision 0 and 1 superblocks.
- Group descriptors of 32 bytes, and of 64 bytes or more with the `64bit` feature, placed after the superblock or with `meta_bg`. `flex_bg` places bitmaps and inode tables anywhere in the volume.
- Block maps with direct, single, double and triple indirect blocks, and extent trees up to 5 levels deep. A hole reads as zeros, and so does an extent marked uninitialized.
- Inline data in the inode and its `system.data` attribute, for files, folders and links.
- Fast links held in the inode, and links held in blocks or inline data.
- Folders read record by record, so hashed `dir_index` folders read as plain ones.
- Metadata checksums, both the CRC32C of `metadata_csum` and the CRC-16 descriptors of `uninit_bg`. With either feature, the reader skips the groups whose inodes are unused and the unused tail of each inode table.

## What it writes

The reader writes the tree `7zz x` writes on macOS and Linux:

- Each folder record from the root folder down becomes a path. A regular file keeps its permission bits without setuid, setgid or sticky. A folder adds its owner's read, write and search rights. A named pipe, socket or device becomes an empty file of mode `0o666`. The umask limits all of them.
- Files and folders get their modification time, to the nanosecond where the inode keeps it. Folders get theirs after all files are written.
- A link becomes a link to its target, which must stay inside the destination.
- Names that share an inode become hard links to the first of them that is written, as 7-Zip writes them.
- `lost+found` is written as a folder.
- The reserved inodes 1, 3, 4, 5, 6, 8, 9 and 10 whose size is not zero become files in `[SYS]`, named `Bad`, `UserQuota`, `GroupQuota`, `BootLoader`, `Undelete`, `Journal`, `Exclude` and `Replica`. So the journal is written as `[SYS]/Journal`. The resize inode 7 is never written.
- A regular file in use with one link and no folder record becomes `[UNKNOWN]/N`, named by its inode number. The orphan file and the project quota file of a fresh `mke2fs` volume are written this way.

It never restores owners, flags, access, change or creation times, or extended attributes, and 7-Zip does not write them either.

## Volumes it sends to 7-Zip

The default engine sends the whole volume to 7-Zip, and `--zip-engine native` refuses it with the feature's name, when the superblock names:

- a journal that needs recovery, the `recover` feature, or orphan inodes that need recovery, either the `orphan_present` feature or an orphan list in the superblock;
- encryption, compression, a separate journal device, extended attributes in their own inodes, or data in folder records;
- clusters larger than a block (`bigalloc`) or blocks that files share (`shared_blocks`);
- an incompatible or read-only feature this page does not list, or a checksum type other than CRC32C.

7-Zip 26.05 ignores every feature flag, so it reads a volume that needs recovery as its blocks stand, and it cannot open a `bigalloc` volume.

## Safety checks

The reader checks the whole layout before it writes anything, and it publishes nothing on failure:

- The superblock has its signature, a block size of 1 KiB to 64 KiB, a first data block that follows the boot block, and groups that fit a bitmap block. Its inode count equals the groups times the inodes per group, and its first inode is 11. The volume ends inside its image.
- Every checksum the volume keeps holds. Those are the checksums of the superblock, each group descriptor, each inode bitmap, each inode in use, each extent block and each folder block. A folder block without a checksum tail must be a hashed folder's index block.
- Each group's free inode count equals its bitmap, and no inode in use lies past the group's used inodes. Every inode in use has a type and at least one link.
- Each extent node has its signature, at most the entries that fit, the depth its parent gives and no more than 5 levels. Extents follow one another in logical order inside the range their index entry gives. Every block an extent or a block map names lies inside the volume.
- Every block a file, folder, link, journal or reserved inode uses, and every extent and indirect block that maps it, is claimed once across the volume, beside the superblock and group descriptors. So shared blocks, looping extent trees and looping block maps are refused, and no walk repeats.
- A folder is a whole number of blocks with no hole. Each folder record has a length of at least 12 bytes that ends inside its block and holds its name. A folder starts with its `.` and `..` records, naming itself and its parent as folders, and an inline folder names its parent. No other record is named `.` or `..`, and a record that names no inode holds no NUL in its name, as 7-Zip checks it.
- A record names an inode in use past the reserved inodes, of the type the record gives. A record of type 0, which names no type, matches any inode, as 7-Zip reads it. Each folder has exactly one name, so a folder that names its ancestor is refused. A folder in use that no folder names is refused.
- A file's link count equals its names, and a folder's equals two plus its subfolders, or one with `dir_nlink`.
- A pipe, socket or device holds no bytes, and a fast link is shorter than 60 bytes. Inline data lies inside its inode.
- Names pass the same checks as tar names. `..`, `.`, `/`, backslashes, `:` and NUL are refused, and so is a name that is not UTF-8. A folder holds no two entries of one name. A root entry named `[SYS]` or `[UNKNOWN]` is refused when the reader writes that folder.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Block size | 1 KiB to 64 KiB |
| Inodes in use | at most 1,000,016 |
| Folder records | at most 1,000,016 |
| Names | at most 64 MiB |
| Runs of blocks that files use | at most 16,777,216 |
| Extent tree depth | 5 levels |
| Folder depth | at most 1,024 |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader never holds the volume's bitmaps or inode tables. It reads one group at a time and keeps one block of metadata. It keeps one small entry per inode in use, one per folder record with the names in one buffer, and one per run of contiguous blocks a file uses. So memory follows the inodes, folders and fragments, not the volume size. On a volume of 998,000 empty files in 1,000 folders from `mke2fs -d`, `zipper test` peaked at 440 MiB of resident memory and `7zz t` at 265 MiB. Most of Zipper's share is the shared tree that every in-process reader keeps of the paths it writes. A run without a destination leaves the holes of a sparse file unread, so verifying a file that declares terabytes of holes reads only its blocks. `--threads` has no effect on ext volumes.

## Differences from 7-Zip

`tests/ext_corpus.rs` requires every image in `tests/data/ext-corpus` to extract to the tree `7zz x` extracted when the corpus was written, and to the tree 7-Zip extracts where it is installed. Where `mke2fs` is installed, it copies seeded source trees into volumes of eleven layouts, and each must extract as the source and 7-Zip give it. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip cannot open a volume with an inline folder. On other volumes it reads some inline files, and refuses the rest and every inline link as `Unsupported Method`, writing the other names of such an inode empty. Zipper reads them all, and the corpus checks its inline volume against the source tree `mke2fs` copied.
- 7-Zip refuses a block-mapped file whose pointer to an indirect block is zero before a block that follows, and writes it empty. Zipper reads it.
- 7-Zip checks no checksum, and reads a volume whose group free counts, bitmaps or link counts disagree, warning about some of them. Zipper refuses each.
- 7-Zip stops reading a folder block at a record whose length leaves the block, and drops the records after it. It writes a name that holds `/` or bytes that are not UTF-8. Zipper refuses each.
- 7-Zip extracts files that share blocks. Zipper refuses the volume.
- 7-Zip ignores an extent node's capacity field. Zipper refuses a node whose capacity does not fit its block, as the kernel does.
- 7-Zip writes an in-use inode no folder names whatever its type, and writes the entries of a folder no folder names at the top. Zipper writes only a regular file with one link this way, and refuses the rest.
- 7-Zip ignores feature flags. Zipper sends the features above to 7-Zip.
- 7-Zip sets a link's time. Zipper creates links without one, as its other readers do.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. A hard link whose first name the selection or strip leaves out is written from the next selected name of its inode. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[ext-fields]` in `verification/coverage.txt`:

- The CRC32C and CRC-16 tables equal a bit-at-a-time reference on every 2 bytes and seed.
- The superblock reader never panics on any 640 bytes. A layout it accepts has blocks of 1 KiB to 64 KiB, a volume below 2^64 bytes, inodes and descriptors that fit a block, and a group count whose inodes fit 32 bits.
- For every block size, descriptor size and group, with or without `meta_bg`, the group's descriptor lies inside its block.
- The group descriptor and inode readers never panic. An inode's extra fields end inside the inode on a 4-byte boundary.
- The time reader never panics.
- The extent header and entry readers never panic. An entry they accept lies inside its node, ends below 2^32 logical blocks and names a block below 2^48.
- The folder record reader never panics. A record it accepts is at least 12 bytes, ends inside its block and holds its name.
- The inline data reader never panics, and the value it finds lies inside the inode.

Unit tests compare the checksums with their published check values and a bit-at-a-time version, and the time reader with the times 7-Zip writes.

`tests/hostile_ext.rs` flips bytes of the superblock, group descriptors, inodes, extent and indirect blocks and folder blocks of the corpus volumes. On volumes with checksums, it flips each byte again with every checksum recomputed, so the checks behind them are reached. Zipper must refuse each damaged volume whenever 7-Zip refuses it. When 7-Zip extracts it, Zipper must extract the same tree or refuse it for one of the stricter checks above. The gate runs a sample. The full sweeps flip 1,500 bytes spread over the structures of each of eight images, and 3,000 of the inline image, and `cargo xtask verify-sync ext-fields` runs them. Crafted cases cover shared blocks, looping extent trees and block maps, folder loops, unsafe names and links, link counts, the features refused by name and cut images. `tests/data/ext-corpus/NOTICE.txt` names the tools that wrote the corpus.
