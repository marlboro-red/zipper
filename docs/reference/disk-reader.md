# In-process disk image reader

Zipper extracts VHD and VHDX disk images, MBR and GPT partition tables and the FAT12, FAT16 and FAT32 file systems on them with its own Rust reader, in the tree `7zz x` writes. The default engine and `--zip-engine native` send a file here when it starts with a VHDX or VHD signature, a GPT, an MBR or a FAT boot sector. A file named `.vhd` that ends with a fixed VHD footer comes here too. The default engine sends a differencing VHD or VHDX, and a disk whose file system Zipper does not read, to 7-Zip. `--zip-engine native` refuses both and names the reason. `--zip-engine backend` hands every disk to 7-Zip.

## Supported images

The reader has three layers, and each partition is a bounded `Read + Seek` view of the disk below it. A file system reader takes one view, so the FAT reader and the [NTFS reader](ntfs-reader.md) plug in at the same place.

- Containers. A fixed VHD, a dynamic VHD with blocks of 512 KiB to 2 GiB, and a VHDX with blocks of 1 MiB to 256 MiB, fixed or dynamic, with 512 or 4096-byte logical sectors. A raw disk is its own container.
- Partition tables. An MBR with primary, extended and logical partitions, with 512 or 4096-byte sectors as 7-Zip detects them, and a GPT with 512 or 4096-byte sectors.
- File systems. FAT12, FAT16 and FAT32 with VFAT long names, NT lowercase flags, deleted entries, empty files, folders up to 256 deep and fragmented cluster chains. NTFS, through the NTFS reader's `read_volume`, with that reader's rules. APFS, through the [APFS reader](apfs-reader.md)'s `read_volume`, with that reader's rules.

## What it writes

The reader follows 7-Zip's rule for the main subfile. A disk whose partition table holds one partition, and a VHD or VHDX, is opened again inside, down to the file system. So a VHD holding an MBR with one FAT partition writes the files of that FAT volume.

- An NTFS volume writes the tree the NTFS reader writes for the same raw volume, and an APFS container the tree the APFS reader writes.
- A FAT volume writes its tree. Names come from the long name when its records and checksum match, as 7-Zip checks them, and otherwise from the short name with its NT lowercase flags. A `/` in a long name becomes `_`. A short name of spaces becomes `[NONAME]-N`, as 7-Zip names it.
- Files get their modification time, read as local time. A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. Folders get mode `0o755` and their time. The umask limits all of them.
- A disk with several partitions writes each partition as one file, as 7-Zip does. An MBR partition is named `N.fat`, `N.ntfs` or `N.img` by its type, and a gap that an extended partition or the end of the disk leaves is named `N`. A GPT partition is named `N.name.fat` or `N.name.img`. These files get mode `0o644` and no time.

It never restores the hidden, system or archive attributes, short names beside long ones, creation or access times, or the volume label.

## Partitions it sends to 7-Zip

The default engine sends the whole image to 7-Zip, and `--zip-engine native` refuses it with the reason, in these cases:

- The lone partition, or the disk inside a VHD or VHDX, holds exFAT, UDF or any other file system that is not FAT, NTFS or APFS.
- The lone partition holds an APFS container that the APFS reader leaves to 7-Zip.
- The lone partition holds no partition table or file system Zipper knows, such as one of zeros.
- A GPT partition name holds `/`, `\`, `:` or a control character, since 7-Zip writes it as a path.
- The image is a differencing VHD or VHDX, which 7-Zip reads with its parent disk.

A disk with several partitions extracts in process whatever they hold, since 7-Zip writes each one as a file without reading it.

## Safety checks

The reader checks the layout before it writes anything, and it publishes nothing on failure:

- A VHD footer and dynamic header have their signature, checksum, known type and reserved zeros. The dynamic footer copy at the end of the file repeats the one at its start. The block table and every block it names lie inside the file. A sector that the block bitmap marks absent must hold zeros, as 7-Zip checks it.
- A VHDX has both headers valid under CRC-32C, two identical region tables, a metadata region and a block table inside the file, and no metadata item it requires that Zipper does not know. A log with entries that were never replayed is refused. Every block table entry has a known state and no reserved bits, no block lies past 2^62, and no two blocks share a megabyte of the file.
- MBR entries have a known status, valid CHS fields, a size and a start that fit 32 bits, and each one starts past the one before. Extended partitions nest at most 128 deep and the table lists at most 128 items. GPT headers and partition tables have their CRC-32, at most 65,536 entries of 128 to 4096 bytes, and partitions that end after they start.
- A FAT boot sector passes 7-Zip's checks of the jump, sector size, cluster size of at most 256 KiB, FAT count, root folder size and cluster count. The FAT lies inside the volume and its first entry names the media.
- Every folder cluster is claimed once. So a folder chain that loops, a folder that names its own ancestor and two folders that share a cluster are refused, and the walk cannot loop. A folder is at most 2 MiB, as the FAT specification limits it.
- A file's chain holds exactly the clusters its size needs and then ends. A chain that ends early, runs past its size, leaves the volume or reaches a free cluster is refused. So is an empty file that names a cluster.
- Every file cluster is claimed once across the volume, and no file uses a folder's cluster. So a chain that loops and two files that share a cluster are refused.
- Names pass the same checks as tar names. `.`, `..`, `\`, `:` and NUL are refused, and so is a long name that is not valid UTF-16 and a short name outside ASCII. A folder that holds two entries of one name is refused.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries in one volume | at most 1,000,000, as the shared tree allows |
| Names of one volume | at most 64 MiB in total |
| One folder | at most 2 MiB, 65,536 records |
| Folder depth | at most 256 |
| MBR items | at most 128, with extended partitions 128 deep |
| GPT entries | at most 65,536 |
| Nesting of disks and partitions | at most 32 levels |
| VHD disk | at most 2 TiB |
| VHDX disk | at most 64 TiB |

The reader never holds a whole FAT or block table. It reads the FAT in 64 KiB blocks and keeps 4 of them, and it reads VHD and VHDX block tables in 4 KiB pages and keeps 8. It reads the FAT once when it opens the volume, as 7-Zip does, to refuse a volume that ends inside it. It keeps one node for each entry, with its name, and one bit for each cluster a file uses, in pages it allocates only where files lie. A VHDX keeps one bit for each megabyte its blocks use.

Peak resident memory on images at the limits, written by `write_disks_at_the_documented_limits` in `tests/hostile_disk.rs`, is in this table.

| Image | `zipper list` | `zipper extract` | `7zz l` | `7zz x` |
| --- | ---: | ---: | ---: | ---: |
| A 2 TiB VHDX with one FAT32 partition, a 256 MiB FAT and one file | 3 MiB | 3 MiB | 277 MiB | 277 MiB |
| 993,600 empty files with names of about 60 bytes, in 92 folders | 236 MiB | 261 MiB | 210 MiB | 209 MiB |

On the first image 7-Zip holds the whole FAT, and Zipper holds a few blocks of it. On the second the shared tree keeps every path, so Zipper peaks about a quarter above 7-Zip. `--threads` has no effect on disk images.

## Differences from 7-Zip

`tests/disk_corpus.rs` requires every image in `tests/data/disk-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. Where `mkfs.fat`, `mtools`, `sgdisk` and `qemu-img` are installed, it also formats and fills seeded volumes and wraps them in every container, and each must extract as 7-Zip extracts it. Writer images in every container, whole and with flipped bits, must extract as 7-Zip extracts them, or be refused when 7-Zip refuses them or for one of the reasons below. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip extracts two FAT files that share clusters. Zipper refuses the volume.
- 7-Zip extracts two entries of one name in a folder, the second over the first. Zipper refuses the folder.
- 7-Zip writes a name with `\` or `:`, a long name that is not valid UTF-16, and a short name outside ASCII in its OEM code page. Zipper refuses each.
- 7-Zip opens a dynamic VHD whose footer copy at the end differs from the one at the start, and looks for the footer elsewhere. Zipper refuses it.
- 7-Zip opens a VHDX whose two region tables differ, whose blocks overlap, or whose log was never replayed, and reports an error. Zipper refuses each.
- 7-Zip turns a FAT local time into UTC with the offset in force now. Zipper uses the offset in force on that date, as its ZIP, ARJ and LZH readers do, so a time in the other half of a daylight saving year differs by an hour.
- 7-Zip's choice of format can depend on the file name, for example whether a file with a fixed VHD footer is named `.vhd`. Zipper follows that rule for fixed VHD footers and reads every other signature by content.
- A FAT volume whose cluster count overflows 32 bits wraps in 7-Zip's arithmetic. Zipper refuses it.

7-Zip reports a headers warning for orphaned long name records, a long name whose checksum does not match and a short name of spaces, and still extracts. Zipper extracts the same names without a warning.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[disk-fields]` in `verification/coverage.txt`:

- The VHD footer reader never panics on any 512 bytes, and a footer it accepts has a known type, a disk of at most 2 TiB and a header offset that fits its type.
- The dynamic VHD header reader never panics on any 1024 bytes. A header it accepts has blocks of 512 KiB to 2 GiB whose bitmap covers every sector, and when its block count fits the disk, every offset of the disk lies in a listed block.
- The VHDX header, region and metadata item readers never panic. An accepted log is aligned to 1 MiB, a region ends before 2^64 and a metadata item lies inside its region.
- Every VHDX block table entry it accepts has a known state and an aligned offset. For every chunk ratio and every block of a disk of at most 64 TiB, the entry it reads lies inside the table and is never a sector bitmap entry.
- The MBR entry reader never panics on any 16 bytes, and a partition it accepts has blocks and ends within 32 bits.
- The GPT header and entry readers never panic for either sector size. A table they accept is at most 256 MiB and a partition ends after it starts, within 64 bits.
- The FAT boot sector reader never panics on any 512 bytes. In a volume it accepts, every valid cluster lies inside the volume's sectors and every FAT entry lies inside the FAT bytes the reader checked.
- The folder record reader never panics on any 32 bytes. A long name record it accepts has a sequence number of 1 to 20, and a short entry outside FAT32 names a 16-bit cluster.
- The long name reader never panics on 1 or 2 records, and a name it accepts ends at its terminator with no terminator or padding before it. The short name reader always gives at most 12 ASCII bytes.

`tests/hostile_disk.rs` builds its images with the test writer in `tests/common/disk_writer.rs`. It holds one case per refusal in the safety checks above, each of which must leave nothing beside the destination, both when extracting and when only verifying. It cuts a FAT12 volume at every sector, and flips a byte in every structure of a raw, VHD and VHDX disk. `tests/data/disk-corpus/NOTICE.txt` names the tools that wrote the corpus.
