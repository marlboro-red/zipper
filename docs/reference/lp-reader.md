# In-process LP reader

Zipper reads Android logical partition images, the `super.img` of dynamic partitions, with its own Rust reader, in the tree `7zz x` writes. The default engine sends a file here when its LP geometry magic sits at byte 4096. A file that a ZIP, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ, compound file, WIM, CHM or 7z reader claims goes there first. `--zip-engine native` also reads these images in process, and `--zip-engine backend` hands them to 7-Zip. An Android sparse image of a `super.img` goes to the disk reader, which hands it to 7-Zip.

## What it writes

An LP image holds 4 KiB of reserved bytes, two copies of the geometry, the metadata slots and their backups, then the partition data. The metadata lists partitions, extents, partition groups and block devices. The reader writes one file for each partition, in table order:

- The file is named after the partition. Each byte of the name, up to the first zero byte or all 36, becomes the character of the same number, as 7-Zip reads it. A partition with no name takes its index in the table, such as `1`.
- A partition with at least one extent gains `.img`, so `system_a` writes `system_a.img`. A partition with no extents writes an empty file under its bare name, as 7-Zip does.
- The file holds each extent in order. A linear extent copies its sectors from the image, and a zero extent writes zeros.
- A `/` in a name makes a folder, as in 7-Zip. Two partitions of the same name write one file, the last one, as `7zz x -y` does.

The reader reads the primary geometry and the primary copy of the first metadata slot only, as 7-Zip does. A damaged primary copy is refused even when a backup holds, as 7-Zip refuses it.

## Checks

The reader opens an image only when 7-Zip opens it:

- The geometry has its magic, a size of 52 bytes and a matching SHA-256 checksum. Its metadata slot size is a positive multiple of 512 bytes, and it has at least one slot. Every slot and its backup lie inside the file.
- The metadata header has its magic and major version 10, with any minor version. It is 128 or 256 bytes long, and its SHA-256 checksum and the checksum of its tables match.
- The tables fit in the slot after the header. Each table has its entry size, 52, 24, 48 or 64 bytes, and lies inside the tables.
- The image holds exactly one block device. Its first logical sector lies after the metadata, and the file reaches its size and the largest maximum size of any partition group.
- Each partition names a group and extents that the tables hold.
- Each extent is linear or zero. A linear extent names block device 0 and lies inside the file. A zero extent names no target sector.

## Safety checks

- The output goes to a hidden staging folder and is published only when the whole image reads, as with every reader.
- Every check above holds before a byte is written. Each partition is charged against free space, or `--max-extracted-bytes`, before it is written, so a zero extent that declares terabytes is refused at once.
- The reader holds the metadata tables in memory, at most the slot size and at most 64 MiB, and copies partition data in 1 MiB pieces. Two partitions may share sectors, as 7-Zip allows.
- `--path`, `--include`, `--exclude`, `--strip-components` and `--strip-prefix` work as with every reader.

## Differences from 7-Zip

- A partition name that is not a plain relative path, such as one that starts with `/`, holds a `..` part, a `\` or a `:`, goes to 7-Zip under the default engine. 7-Zip rewrites such names. `--zip-engine native` refuses them.
- Metadata tables larger than 64 MiB are refused. 7-Zip reads them.
- An image whose geometry gives a logical block size that is not a multiple of 512 bytes extracts as in 7-Zip, without its warning.

## Limits

| Limit | Value |
| --- | --- |
| Block devices | exactly 1, as in 7-Zip |
| Metadata tables | at most the slot size and 64 MiB |
| Partitions | at most 1,000,000, the limit every tree reader shares |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Tests

No AOSP `lpmake` runs on this machine, so `tests/common/lp_writer.rs` writes every image from the liblp metadata format. `tests/lp_corpus.rs` writes A/B layouts, versions 10.0, 10.1 and 10.2, one and three slots, and a 3 MiB partition in many extents. It also writes shared and backward extents, extents inside the metadata, and the names 7-Zip writes as they are. Each extracts byte for byte as `7zz x` extracts it. Other tests check a partition's bytes directly, send names that leave the folder to 7-Zip, select one partition, and charge a zero extent of 512 TiB against the cap.

`tests/hostile_lp.rs` cuts a small image at every sector and around every boundary. It changes every geometry, header and table byte three ways and repairs the checksums, so each change reaches its field, and flips a byte in every 97 unrepaired. Each result is refused when 7-Zip refuses it, and otherwise extracts as 7-Zip extracts it or is refused for a reason listed above. A change that would make 7-Zip write more than 64 MiB of zeros is left out. Other tests give the tables, the slot size, the slot count and the partition count their largest values. Kani proofs under `[lp-fields]` in `verification/coverage.txt` cover the geometry, header, partition and extent readers.
