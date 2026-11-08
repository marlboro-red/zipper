# In-process disk image reader

Zipper extracts VHD, VHDX, QCOW, VDI, VMDK and Android sparse disk images with its own Rust reader, in the tree `7zz x` writes. The same reader reads MBR and GPT partition tables and the FAT12, FAT16 and FAT32 file systems on them. The default engine and `--zip-engine native` send a file here when it starts with a container's signature, a GPT, an MBR or a FAT boot sector. The containers are VHDX, VHD, QCOW, VDI, VMDK sparse extents and Android sparse images. A file named `.vhd` that ends with a fixed VHD footer comes here too, and so does a file named `.vmdk` that starts with `# Disk DescriptorFile`. The default engine sends a differencing image, and a disk whose file system Zipper does not read, to 7-Zip. `--zip-engine native` refuses both and names the reason. `--zip-engine backend` hands every disk to 7-Zip.

## Supported images

The reader has three layers, and each partition is a bounded `Read + Seek` view of the disk below it. A file system reader takes one view, so the FAT reader and the [NTFS reader](ntfs-reader.md) plug in at the same place.

- Containers. Each one maps the disk's bytes to places in its files and hands the next layer the same view a raw disk gives.
  - A fixed VHD, a dynamic VHD with blocks of 512 KiB to 2 GiB, and a VHDX with blocks of 1 MiB to 256 MiB, fixed or dynamic, with 512 or 4096-byte logical sectors.
  - QCOW version 1, 2 and 3 with clusters of 512 bytes to 2 MiB, or to 64 KiB in version 1. Zero clusters, snapshots and any reference count width are read, and so are clusters compressed with zlib in versions 2 and 3.
  - VDI version 1 images, dynamic, static and undo, with the 1 MiB blocks 7-Zip reads.
  - VMDK hosted sparse extents with grains of 1 to 65,536 sectors, stream-optimized extents in the layout 7-Zip reads, and descriptor files that join `FLAT`, `VMFS` and `SPARSE` extents from the descriptor's folder.
  - Android sparse images with raw, fill, don't-care and CRC-32 chunks.
  - A raw disk, which is its own container.
- Partition tables. An MBR with primary, extended and logical partitions, with 512 or 4096-byte sectors as 7-Zip detects them, and a GPT with 512 or 4096-byte sectors.
- File systems. FAT12, FAT16 and FAT32 with VFAT long names, NT lowercase flags, deleted entries, empty files, folders up to 256 deep and fragmented cluster chains. NTFS, through the [NTFS reader](ntfs-reader.md), with its rules. HFS+, through the [DMG reader's HFS+ code](dmg-reader.md), and APFS, through the [APFS reader](apfs-reader.md), with their rules.

## What it writes

The reader follows 7-Zip's rule for the main subfile. A disk whose partition table holds one partition, and every container, is opened again inside, down to the file system. So a QCOW image holding an MBR with one FAT partition writes the files of that FAT volume.

- An NTFS volume writes the tree the NTFS reader writes for the same raw volume, an HFS+ volume the tree the DMG reader writes, and an APFS container the tree the APFS reader writes.
- A FAT volume writes its tree. Names come from the long name when its records and checksum match, as 7-Zip checks them, and otherwise from the short name with its NT lowercase flags. A `/` in a long name becomes `_`. A short name of spaces becomes `[NONAME]-N`, as 7-Zip names it.
- Files get their modification time, read as local time. A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. Folders get mode `0o755` and their time. The umask limits all of them.
- A disk with several partitions writes each partition as one file, as 7-Zip does. An MBR partition is named `N.fat`, `N.ntfs` or `N.img` by its type, and a gap that an extended partition or the end of the disk leaves is named `N`. A GPT partition is named `N.name.fat` or `N.name.img`. These files get mode `0o644` and no time.

It never restores the hidden, system or archive attributes, short names beside long ones, creation or access times, or the volume label.

## Images it sends to 7-Zip

The default engine sends the whole image to 7-Zip, and `--zip-engine native` refuses it with the reason, in these cases:

- The lone partition, or the disk inside a container, holds exFAT, UDF or any other file system that is not FAT, NTFS, HFS+ or APFS.
- The lone partition holds an APFS container that the APFS reader leaves to 7-Zip.
- The lone partition holds no partition table or file system Zipper knows, such as one of zeros.
- A GPT partition name holds `/`, `\`, `:` or a control character, since 7-Zip writes it as a path.
- The image is a differencing VHD or VHDX, which 7-Zip reads with its parent disk.
- The image is a QCOW image with a backing file, a VDI of the differencing type or with a parent UUID, or a VMDK whose descriptor names a parent CID.
- A VMDK descriptor names an extent type other than `FLAT`, `VMFS`, `SPARSE` and `ZERO`.

A disk with several partitions extracts in process whatever they hold, since 7-Zip writes each one as a file without reading it.

## Images it refuses by name

7-Zip 26.05 does not read these either, so both engines refuse them and name the feature:

- A QCOW image that is encrypted, keeps its data in an external file, uses extended second-level entries or is marked corrupt.
- A QCOW image that compresses a cluster with zstd, and a QCOW version 1 image that compresses a cluster.
- A VDI whose blocks are not 1 MiB or whose sectors are not 512 bytes.
- A VMDK sparse extent whose grain tables do not hold 512 entries, and a compressed one that is not version 3 with both the compressed and marker flags and Deflate.
- A stream-optimized VMDK whose grain directory or a grain table lacks its marker. `qemu-img convert -O vmdk -o subformat=streamOptimized` writes this layout, and 7-Zip cannot open it.
- A VMDK descriptor that names a `ZERO` extent.

## Safety checks

The reader checks the layout before it writes anything, and it publishes nothing on failure:

- A VHD footer and dynamic header have their signature, checksum, known type and reserved zeros. The dynamic footer copy at the end of the file repeats the one at its start. The block table and every block it names lie inside the file. A sector that the block bitmap marks absent must hold zeros, as 7-Zip checks it.
- A VHDX has both headers valid under CRC-32C, two identical region tables, a metadata region and a block table inside the file, and no metadata item it requires that Zipper does not know. A log with entries that were never replayed is refused. Every block table entry has a known state and no reserved bits, no block lies past 2^62, and no two blocks share a megabyte of the file.
- A QCOW header has a known version, cluster size, compression type and incompatible features. Its first-level table covers the disk and lies inside the file with its reference count table, as 7-Zip checks them. The reader reads every first-level entry and every entry of each second-level table it names, as 7-Zip does, before the disk is read. Each table and cluster lies inside the file, has no reserved bits and starts on a cluster. A compressed cluster's sectors end inside the file.
- No two QCOW tables or clusters share a cluster of the file, and no compressed cluster shares one with a table or an uncompressed cluster. Compressed clusters may share a cluster with each other, as `qemu-img` packs them.
- A VDI header has its signature, version 1, a header that ends before the block map, a block map that ends before the data, and a block count that covers the disk. The data holds every block the header counts as allocated. Every map entry names an allocated block, and no two entries name the same one.
- A VMDK sparse extent has its signature, a version of at most 3, its newline test when the flag asks for one and a power-of-two grain. Its embedded descriptor lies inside the file and passes the descriptor grammar below. The grain directory and every grain table and grain lie inside the file, and every grain lies past the extent's metadata, as 7-Zip checks it.
- No two VMDK tables or grains share bytes of the file. A grain that does not start on a grain boundary claims both grains of the file it touches.
- A stream-optimized extent has a marker of the right size before its grain directory and each grain table. Each compressed grain names its own place in the disk, holds a zlib stream that ends exactly at its size and decodes to the whole grain. When the header places the grain directory at the end, the footer marker, a footer that repeats the header's layout and the end-of-stream marker close the file.
- A VMDK descriptor starts with `# Disk DescriptorFile`. Every line before the first NUL is blank, a `#` comment at its start, or a setting with `=` and no quote before it. Any other line is an extent of access, decimal sectors, type and quoted file name, as 7-Zip reads them. A flat extent ends inside its file, and a sparse extent's capacity equals the descriptor's size. An extent file must be one plain name in the descriptor's folder, without `/`, `\`, `:`, `.` or `..`.
- An Android sparse header has its magic, major version 1, the header sizes 28 and 12 and a block size that is a multiple of 4. Every chunk has a known type, a zero reserved field, the size its type needs and lies inside the file. A CRC-32 chunk covers no blocks. The chunks cover exactly the blocks the header counts. 7-Zip ignores the CRC-32 values, and Zipper does too.
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
| QCOW disk | less than 2^56 bytes, with at most 4,194,304 first-level entries |
| VDI disk | less than 2^56 bytes |
| VMDK disk | less than 2^56 bytes, with at most 4,096 extents and a descriptor of at most 1 MiB |
| Android sparse block | a multiple of 4 bytes, at most 256 MiB |

The reader never holds a whole FAT or block table. It reads the FAT in 64 KiB blocks and keeps 4 of them, and it reads VHD, VHDX, QCOW, VDI and VMDK tables in 4 KiB pages and keeps 8. It reads every table once when it opens the image, in batches of 64 KiB, to refuse one that names a place outside the file or twice. A QCOW image keeps one bit for each cluster its tables name, and a VDI one bit for each allocated block. A VMDK keeps one bit for each grain and for each sector of its tables and compressed grains. These bits sit in pages allocated only where they lie. A compressed QCOW cluster or VMDK grain is decoded whole, and the last one stays held. An Android sparse image keeps 16 bytes for every 256 chunks, and decodes the 256 chunk headers around a read. The reader reads the FAT once when it opens the volume, as 7-Zip does, to refuse a volume that ends inside it. It keeps one node for each entry, with its name, and one bit for each cluster a file uses, in pages it allocates only where files lie. A VHDX keeps one bit for each megabyte its blocks use.

Peak resident memory on images at the limits, written by `write_disks_at_the_documented_limits` in `tests/hostile_disk.rs`, is in this table.

| Image | `zipper list` | `zipper extract` | `7zz l` | `7zz x` |
| --- | ---: | ---: | ---: | ---: |
| A 2 TiB VHDX with one FAT32 partition, a 256 MiB FAT and one file | 3 MiB | 3 MiB | 277 MiB | 277 MiB |
| 993,600 empty files with names of about 60 bytes, in 92 folders | 236 MiB | 261 MiB | 210 MiB | 209 MiB |

Peak resident memory of `zipper test` and `7zz t` on container images at the limits, from the same test, is in this table. Each image holds an MBR with one FAT32 partition and one small file.

| Image | `zipper test` | `7zz t` |
| --- | ---: | ---: |
| A 128 GiB QCOW with 512-byte clusters and 4,194,304 first-level entries | 4 MiB | 67 MiB |
| A 2 TiB dynamic VDI | 4 MiB | 267 MiB |
| A 512 GiB VMDK sparse extent with 64 KiB grains | 4 MiB | 100 MiB |
| A 2 TiB Android sparse image of 4,194,314 chunks | 4 MiB | 323 MiB |

7-Zip holds the whole FAT and the whole block or chunk table, and Zipper holds a few pages of each. `--threads` has no effect on disk images.

## Differences from 7-Zip

`tests/disk_corpus.rs` requires every image in `tests/data/disk-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. Where `mkfs.fat`, `mtools`, `sgdisk` and `qemu-img` are installed, it also formats and fills seeded volumes and wraps them in every container, and each must extract as 7-Zip extracts it. Where `qemu-img` is installed, it converts the corpus disks to QCOW, VDI and VMDK images of each subformat and option the reader supports. Each must extract as 7-Zip extracts it. Writer images in every container, whole and with flipped bits, must extract as 7-Zip extracts them, or be refused when 7-Zip refuses them or for one of the reasons below. A flip that leaves a disk Zipper sends to 7-Zip counts as a match, since the default engine then writes 7-Zip's tree. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip extracts two FAT files that share clusters. Zipper refuses the volume.
- 7-Zip extracts two entries of one name in a folder, the second over the first. Zipper refuses the folder.
- 7-Zip writes a name with `\` or `:`, a long name that is not valid UTF-16, and a short name outside ASCII in its OEM code page. Zipper refuses each.
- 7-Zip opens a dynamic VHD whose footer copy at the end differs from the one at the start, and looks for the footer elsewhere. Zipper refuses it.
- 7-Zip opens a VHDX whose two region tables differ, whose blocks overlap, or whose log was never replayed, and reports an error. Zipper refuses each.
- 7-Zip opens QCOW, VDI and VMDK images whose tables name one place of the file twice, or a table's place for data. Zipper refuses each.
- 7-Zip reads a QCOW cluster that does not start on a cluster of the file, and an image with incompatible features it does not know. It opens a disk larger than its first-level table covers or than 2^56 bytes. Zipper refuses each.
- 7-Zip opens a VMDK whose grain tables name a grain past the end of the file where the disk is never read, and a compressed grain whose marker names another place. Zipper refuses each.
- 7-Zip reads VDI blocks with extra data as if the extra data were block data. Zipper refuses them.
- 7-Zip opens a VMDK descriptor whose flat extent is longer than its file, whose sparse extent file is not a sparse extent, or whose extent sits in a subfolder. Zipper refuses each.
- 7-Zip turns a FAT local time into UTC with the offset in force now. Zipper uses the offset in force on that date, as its ZIP, ARJ and LZH readers do, so a time in the other half of a daylight saving year differs by an hour.
- 7-Zip's choice of format can depend on the file name, for example whether a file with a fixed VHD footer is named `.vhd`. Zipper follows that rule for fixed VHD footers and VMDK descriptors and reads every other signature by content.
- A FAT volume whose cluster count overflows 32 bits wraps in 7-Zip's arithmetic. Zipper refuses it.

7-Zip reports a headers warning for orphaned long name records, a long name whose checksum does not match and a short name of spaces, and still extracts. Zipper extracts the same names without a warning. 7-Zip writes the extended attributes of HFS+ and APFS files and the named streams of NTFS system files as `file:name` files, which those readers leave out, as their pages say.

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

Kani proves these rules under `[disk-containers]`:

- The QCOW header reader never panics on any 112 bytes. A header it accepts has a disk below 2^56 bytes and a cluster of 512 bytes to 2 MiB. In versions 2 and 3 its first-level table holds at most 4,194,304 entries and covers the disk.
- The QCOW table entry readers never panic on any 64-bit entry, cluster size and version. A cluster they accept lies below 2^56, starts on a 512-byte boundary in versions 2 and 3, and a compressed one spans at most two clusters.
- The VDI header reader never panics on any header. In an image it accepts, every offset of the disk has a block map entry, and the map ends before the data.
- The VMDK header reader never panics on any 512 bytes. In an extent it accepts, every sector of the disk has a grain directory and grain table entry, and a compressed extent names Deflate. The grain entry and marker readers never panic.
- The Android sparse header and chunk readers never panic. A chunk they accept holds exactly the bytes its type needs, and a CRC-32 chunk covers no blocks.

`tests/hostile_disk.rs` builds its images with the test writer in `tests/common/disk_writer.rs`. It holds one case per refusal in the safety checks above, each of which must leave nothing beside the destination, both when extracting and when only verifying. It cuts a FAT12 volume at every sector and each container at hundreds of places, and flips a byte in every structure of a raw, VHD, VHDX, QCOW, VDI, VMDK and Android sparse disk. `every_flip_in_every_container_structure_matches_7zip` in `tests/disk_corpus.rs` flips every byte of every container structure against 7-Zip, and `cargo xtask verify-sync disk-containers` runs it. `tests/data/disk-corpus/NOTICE.txt` names the tools that wrote the corpus.
