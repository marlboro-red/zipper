# In-process APFS reader

Zipper extracts APFS containers with its own Rust reader, in `src/native_apfs.rs`. It reads a raw container, a container in the file system partition of a DMG image, and a container in the lone partition of a GPT disk. The default engine sends a raw container here when its superblock passes 7-Zip's check, and the [DMG reader](dmg-reader.md) and the [disk reader](disk-reader.md) send an APFS partition here. A container with an encrypted, sealed or snapshot volume, or a Fusion container, is refused by name, and the error suggests `--zip-engine backend`. The default engine sends such a container to 7-Zip. `--zip-engine backend` hands every container to 7-Zip.

## Supported containers

The reader follows the APFS Reference and checks each structure as 7-Zip 26.05 checks it:

- The container superblock at block 0, with its NXSB signature and Fletcher-64 checksum, and blocks of 4 KiB to 64 KiB.
- The checkpoint descriptor area. The newest superblock in it, by transaction id, whose checksum holds names the checkpoint to read. The checkpoint maps of that checkpoint must hold their checksum and transaction id, and every mapping must lie in the checkpoint data area.
- The container object map, which maps each volume superblock, and each volume's object map, which maps the nodes of its file-system tree.
- B-trees of fixed and variable keys and values, of any height up to 64 levels, with physical child addresses in an object map and virtual ones in a file-system tree.
- Every volume the container lists, in the order of its list.
- The inode, directory record, file extent, data stream, extended attribute, sibling link and sibling map records of each file-system tree.
- Plain directory keys and the hashed keys of case-insensitive and normalization-insensitive volumes. The reader takes the name from either key and does not check the hash.
- Files compressed through `com.apple.decmpfs`, stored or compressed with zlib or LZVN in the attribute, methods 3, 7 and 9, or in 64 KiB chunks of the `com.apple.ResourceFork` attribute, methods 4 and 8. These are the methods 7-Zip decodes. zlib uses the `flate2` crate, and LZVN Zipper's own decoder in `src/lzvn.rs`. A file compressed with another method, such as LZFSE in methods 11 and 12, is refused by name, as 7-Zip refuses it.

## What it writes

The reader writes each volume as `7zz x` writes it on macOS and Linux:

- One volume's files and folders are at the top of the destination. Several volumes are in folders named `Volume1`, `Volume2` and on, numbered in the order the container lists them, as 7-Zip names them. An empty volume still gets its folder.
- Names are read from UTF-8 and kept as the volume stores them. A `/` in a name becomes `_`. The root folder's own record is not written. The `private-dir` folder is written only when it holds entries, as 7-Zip writes it.
- A file's mode is applied when its mode says it is a regular file. A folder's mode is applied, with the owner's read, write and search rights added. Other entries, such as named pipes, become empty files of mode `0o666`. The umask limits all of them, and setuid, setgid and sticky bits are dropped.
- Files get their modification time, to the nanosecond where the file system keeps it. Folders get theirs after all files are written.
- A link becomes a link to the target its `com.apple.fs.symlink` attribute holds. Each link must stay inside the destination.
- Names that share an inode become hard links to the first of them that is written.
- A sparse file's holes are written as zeros. A cloned file is written from the data stream its inode names.
- A compressed file is written decompressed.

- On macOS, each extended attribute is restored, as the next section describes.

It never restores owners, flags, access, change or creation times, or ACLs.

## Extended attributes and resource forks

On macOS, the reader restores the extended attributes of each file, folder and link as macOS extended attributes. 7-Zip writes each one instead as a file named `file:name`, and Zipper's shared name check still refuses a `:` in every entry name. On Linux and Windows the reader skips them, and `metadata: Metadata::ContentOnly`, or `--content-only`, writes none. The user decided this on 8 November 2026.

- The reader never restores `com.apple.quarantine`, `com.apple.provenance`, `com.apple.macl`, `com.apple.rootless` or any name that starts with `com.apple.system.`, and skips them without a message. Each carries state of the machine that wrote the volume, such as a quarantine or sandbox mark.
- `com.apple.decmpfs` and `com.apple.ResourceFork` hold a compressed file's data, and `com.apple.fs.symlink` holds a link's target. None of them is restored as an attribute.
- Attributes are set in staging, after a file's data is written and before its mode and time are applied. A file whose mode lacks the owner's write right gets it while its attributes are set, and loses it again after. Its modification time is set last.
- A link's attributes are set on the link itself, never on its target, after every link is created.
- A folder that already exists in the destination keeps its own attributes when an extraction merges into it. A file that replaces one has only the archive's attributes.
- Each value counts toward `--max-extracted-bytes`, or free space, before it is written.
- On a destination that reports `ENOTSUP` for extended attributes, the reader skips them and keeps extracting. So it does where macOS keeps them in AppleDouble `._name` files, as on FAT and exFAT, which the reader finds with a probe file in staging before the first attribute. An AppleDouble file could replace an archive entry of that name. Any other failure to set one fails the extraction, and nothing is published.

A resource fork is the `com.apple.ResourceFork` attribute. Zipper refuses a file whose resource fork is not compressed data, and the error names the file and suggests `--zip-engine backend`. It skips an empty resource fork, which `ditto --hfsCompression` leaves on a file it could not compress.

## Safety checks

The reader checks the layout before it writes each file, and it publishes nothing on failure:

- Every object it reads passes its Fletcher-64 checksum, is not newer than the checkpoint, and is the object its owner names. Its header must give the expected type, subtype and identifier, and physical or virtual storage as its tree uses.
- Every object address lies inside the container, and the container holds no more blocks than its image.
- A B-tree's root holds its node size, key and value sizes and storage. Each node's table of contents, keys and values lie inside the node. A child sits one level below its parent. No node is reached twice, so a loop is refused. The nodes and keys the walk visits must equal the counts its root gives.
- Object map identifiers rise strictly, with one version each. Each mapping names an object of one block, not encrypted, deleted or without a header.
- File-system records never fall in identifier. Inodes rise strictly. An attribute or data stream record follows its inode. Every key and value has the size its type requires, and every name ends at its only NUL.
- Each directory record names an inode the volume holds, of the type the record gives. A folder has exactly one name, and every inode but the root and private folders has one. Every entry lies under the root folder, folders nest at most 1,024 deep and never in a loop, and a folder holds no two entries of one name.
- A file's extents follow one another from offset 0, are whole blocks, cover its size and lie inside the container. No extent starts past the file's size.
- A compressed file has no data stream or extents of its own, and its decmpfs header's size matches its inode. Its decompressed data, chunk table and chunks stay within the sizes it declares, and a chunk decodes to exactly its block.
- Names pass the same checks as tar names. `..`, `.`, backslashes, `:` and NUL are refused, and so is a name that is not UTF-8.
- Every extended attribute's name is UTF-8, not empty, without a NUL and at most 127 bytes long, as macOS allows. An entry has at most 128 attributes, skipped ones included, and no two of one name. A value the reader restores holds at most 64 MiB. A volume that breaks any of these is refused by name on every platform, also where attributes are skipped.
- An attribute held as a data stream has extents that follow one another, cover its size and lie inside the container, checked on every platform.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Block size | 4 KiB to 64 KiB |
| Checkpoint descriptor area | at most 65,536 blocks |
| Volumes | at most 100, as the container format allows |
| Nodes in one B-tree | at most 16,777,216 |
| Keys in one B-tree | at most 67,108,864 |
| B-tree height | at most 64 levels |
| Inodes and directory records | at most 1,000,016 each per volume |
| File extents | at most 16,777,216 per volume |
| Names and folder paths | at most 64 MiB each |
| A decmpfs attribute held as a data stream | at most 64 KiB, as 7-Zip reads |
| Extended attributes of one entry | at most 128, skipped ones included |
| One restored attribute value | at most 64 MiB |
| Restored attributes | at most 1,000,000 per volume |
| Attributes of links, held until the links are created | at most 64 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader never holds a container's data. It walks each B-tree one node at a time, with one block for each level. It keeps one small entry per inode, directory record and file extent, with names in one buffer. It also keeps where each compressed file's or link's attribute and each restored attribute lies, and reads it again when it writes that entry. It reads one attribute value at a time, in pieces of 1 MiB, and holds it whole, since macOS sets a value in one call. On a container of 999,936 restored attributes from the test writer, `zipper extract` peaked at 33 MB of resident memory, the reader before this change at 8 MB and `7zz t` at 318 MB. On one with a value of 64 MiB and 1,024 values of 256 KiB, `zipper extract` peaked at 73 MB and `7zz t`, which streams each value, at 4 MB. So memory follows the file-system tree, not the container. It reads one volume's tree at a time. On a container of 998,000 empty files in 1,000 folders from the test writer, `zipper test` peaked at 599 MB of resident memory and `7zz t` at 827 MB. `--threads` has no effect on APFS.

## Differences from 7-Zip

`tests/apfs_corpus.rs` requires every image in `tests/data/apfs-corpus` to extract to the tree recorded when it was written, and to the tree 7-Zip extracts where it is installed. It does the same for the parity APFS image and DMG images, and for generated containers that are also checked against the bytes they were written from. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes each extended attribute of a file or link as a `file:name` file, with a `:` or `/` in the attribute's name written as `_`. Zipper restores it as an extended attribute on macOS and skips it elsewhere. 7-Zip writes no attribute of a folder, and Zipper restores them. 7-Zip also writes the names Zipper never restores.
- Zipper refuses a resource fork that is not compressed data, which 7-Zip writes as a `file:com.apple.ResourceFork` file.
- 7-Zip reads the superblock at block 0 and nothing of the checkpoint area. Zipper reads the newest valid checkpoint, which is the one at block 0 in a container that was unmounted cleanly. Zipper also refuses a container whose newest checkpoint map is damaged, which 7-Zip never reads.
- 7-Zip writes each name of a hard link as a copy of its data. Zipper writes hard links to one copy.
- 7-Zip fills a sparse file's holes with the bytes of the container's first block. Zipper writes zeros.
- 7-Zip fails on a cloned file whose data stream another inode owns, and writes it empty. Zipper writes its data.
- 7-Zip reports a data error for an empty resource fork stream and writes it as an empty `file:com.apple.ResourceFork`. Zipper skips it.
- 7-Zip refuses a relative link with `..` in it, such as `../hello.txt`, and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip writes an inode without a directory record at the top of the destination as `nodeN.name`, and a record whose inode is missing or whose folder the volume lacks at the top too. Zipper refuses each.
- 7-Zip stops reading a volume's object map at the first object with two versions, which snapshots leave, and it checks a sealed volume's hashes. Zipper refuses snapshot and sealed volumes, and the default engine sends them to 7-Zip.
- 7-Zip checks the superblock's checksum over its first 4 KiB, so it opens only containers of 4 KiB blocks. Zipper checks the whole block.
- 7-Zip does not check an object's identifier, storage or transaction id against its owner, or that a node lies one level below its parent. Zipper refuses an object that disagrees, though its checksum must still hold for such a container to exist.

On macOS, `tests/apfs_corpus.rs` compares each restored attribute with the bytes of 7-Zip's `file:name` file, and with the bytes a generated container was written from. Those containers carry attributes on files, folders, links and hard links, and values of up to 3 MiB held as data streams. They also hold a read-only file, a name of 127 bytes and every name that is never restored. Another test checks that `Metadata::ContentOnly` writes none, that a selection and a strip keep them, and that an extraction into an existing folder keeps that folder's own.

`tests/hostile_apfs.rs` flips a bit in bytes of the container superblock, the checkpoint area, the object maps and their nodes, and the file-system tree's nodes. It does the same to the volume superblock and the file extents, and cuts the container at many lengths. Zipper must refuse each damaged container whenever 7-Zip refuses it. When 7-Zip extracts it, Zipper must extract the same tree or refuse it for one of the stricter checks above. The gate runs a sample of each sweep. The full sweeps flip every byte of each object's first 128 bytes and of the superblock's fields, and every 7th to 17th byte elsewhere. They are ignored tests that `cargo xtask release-checks` runs under `[apfs-fields]`. Crafted cases cover B-tree loops in both kinds of tree, nodes outside the container, bad checksums and extents past the end. They also cover records that disagree with their inodes, unsafe names and links, deep and looping folders, repeated names, and each refusal by name. Each attribute rule above has a crafted container, refused with nothing published.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes, with `VolumeN` first when the container has several volumes. A hard link whose first name the selection or strip leaves out is written from the next selected name of its inode. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[apfs-fields]` in `verification/coverage.txt`:

- The Fletcher-64 checksum equals a word-at-a-time reference on every input of up to 3 words. The object header reader never panics on any block of up to 36 bytes, and accepts one only when its checksum matches.
- The container superblock reader never panics on any 984 bytes. A superblock it accepts has a block size of 4 KiB to 64 KiB, at most 2^62 bytes and 100 volumes, and checkpoint areas inside the container. Every block of its checkpoint lies inside the descriptor area.
- The checkpoint map reader never panics, and its mappings fit its block. A mapping it accepts names whole blocks inside the checkpoint data area.
- The object map and volume superblock readers never panic.
- The B-tree node reader never panics on any node of up to 128 bytes. Every key and value of an entry it accepts lies between the table of contents and the end of the values.
- The directory record, extended attribute and file extent readers never panic on any key of up to 16 bytes and value of up to 52. A name they accept lies inside its key and ends at its only NUL. The inode reader never panics on any value of up to 116 bytes, and its extended fields stay inside the inode.

Unit tests compare the checksum with a word-at-a-time version on every length within two words of each 1,024-word run edge up to 3,079 words, and on 64 KiB of `0xFF` bytes.

`tests/data/apfs-corpus/NOTICE.txt` names the source of the corpus.
