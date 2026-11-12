# M17. Disc and disk images

Disc images and Windows images extract in process.

Scope:

- ISO 9660 with Joliet and Rock Ridge, and UDF, choosing the same tree 7-Zip chooses.
- WIM with XPRESS, LZX and LZMS, and the NTFS alternate streams it stores, written on Windows behind an option.
- VHD and VHDX disks holding FAT or NTFS.
- DMG with its UDIF compression, and the HFS+ and APFS file systems inside, read by Zipper's own code. The user decided this on 8 November 2026, choosing it over leaving them to 7-Zip.

Exit checks:

1. The M10 rows for these formats read native or are recorded as decided against, with the reason.
2. Each image reader bounds memory by the image's directory size, not its data size.

On 12 November 2026 the NTFS, FAT, DMG, HFS+, APFS, LZX and LZMS code was removed for licence reasons, as [M32](m32-every-reader-under-zippers-licence.md) records. Zipper now refuses those file systems and images, and WIM images compressed with LZX or LZMS. The same day a second audit found the disk image reader a port of 7-Zip's LGPL code. So the VHD, VHDX, QCOW, VDI, VMDK, Android sparse, MBR and GPT reader was removed too. Every engine now refuses those images, and [M33](m33-clean-rewrites-of-the-copied-checks.md) tracks a clean rewrite. The disk reader reference page is gone, so the history below names it without a link. The history stays as it was otherwise.

Decided, on 8 November 2026:

- On macOS, the APFS and HFS+ readers restore extended attributes as real extended attributes, and skip them elsewhere. The user chose this over skipping them or writing 7-Zip's `file:name` files, and the shared name check keeps refusing `:`.

Done so far, by 8 November 2026:

- ISO 9660 with Joliet, Rock Ridge and El Torito, and UDF 1.02 to 2.60 with metadata partitions, read in process in the tree 7-Zip chooses, as the [ISO reader reference](../reference/iso-reader.md) describes. A UDF tree 7-Zip opens wins, then the last Joliet volume, then the primary volume. Every `hdiutil` image in the corpus and every generated disc extract as `7zz x` extracts them, modes included. `xorriso` and `mkisofs` were not installed, so no image from them was compared. The reader keeps one node for each entry and reads one folder at a time, so its memory follows the folders, not the data.
- WIM and ESD images read in process, with Zipper's own XPRESS and LZMS decoders and the LZX decoder in its WIM mode, as the [WIM reader reference](../reference/wim-reader.md) describes. Every corpus WIM from `wimlib-imagex` and 36 generated ones of every method and chunk size extract as `7zz x` extracts them, with hard links and links. A split WIM goes to 7-Zip. Alternate data streams are refused, because the shared name check refuses `:`, and the Windows option that writes them as NTFS streams is not built yet.
- Raw NTFS volumes read in process, in the tree 7-Zip writes, as its reader reference described, with LZNT1, sparse, fragmented and extension record data, hard links and links. Every corpus volume from `mkntfs` and libntfs-3g and seeded volumes from the test writer extract as `7zz x` extracts them, apart from the listed differences, and `ntfscat` reads the same bytes. The reader keeps a few bytes per MFT record and the names of records in use, so its memory follows the MFT, not the data. `read_volume` takes a bounded view of one partition for the VHD and VHDX readers.
- VHD and VHDX images, MBR and GPT disks and FAT12, FAT16 and FAT32 with long names read in process, as the disk reader reference described. Every corpus image from `mkfs.fat`, `mtools`, `sgdisk` and `qemu-img` and every writer image extracts as `7zz x` extracts it. A disk whose lone partition holds NTFS or another file system, and a differencing image, goes to 7-Zip. The reader reads the FAT and block tables in blocks, so its memory follows the folders, not the disk.
- QCOW, VDI, VMDK and Android sparse images read in process as containers of the disk reader, and HFS+ partitions on disks too, as the disk reader reference described. Every corpus image from `qemu-img`, `hdiutil` and the test writer extracts as `7zz x` extracts it, holding FAT, NTFS, HFS+ or APFS. So does every `qemu-img` subformat the reader supports. Every flip of every container structure extracts as 7-Zip extracts it, goes to 7-Zip, or is refused when 7-Zip refuses it or for a listed reason. 7-Zip 26.05 cannot open the stream-optimized layout `qemu-img` writes, so the corpus holds one from the test writer. An image with a backing file or parent disk goes to 7-Zip. The tables are read in pages, so memory follows the tables touched. On a 2 TiB Android sparse image of 4,194,314 chunks `zipper test` peaked at 4 MiB and `7zz t` at 323 MiB.
- DMG images and the HFS+ and HFSX volumes inside them read in process, with Zipper's own ADC, LZFSE and LZVN decoders, as its reader reference described. Every `hdiutil` image in the corpus, of six UDIF formats and both file systems, extracts as `7zz x` extracts it, apart from the listed differences. So do generated volumes with every chunk method, compressed files and partition maps. Hard links become links and resource forks are refused. On macOS, extended attributes are restored as extended attributes, and elsewhere skipped. A DMG holding another file system goes to 7-Zip. Memory follows the catalog. On 999,001 entries `zipper test` peaked at 665 MB and `7zz t` at 957 MB.
- APFS containers read in process, raw and inside DMG images and GPT disks, as its reader reference described, from the newest valid checkpoint through both object maps and the file-system tree. Every `hdiutil` image in the corpus, of seven formats with plain, case-sensitive and several volumes and a container past 4 GiB, extracts as `7zz x` extracts it, apart from the listed differences. So do the parity APFS image and DMGs and generated containers with every decmpfs method 7-Zip decodes. Hard links become links, sparse and cloned files keep their bytes, and on macOS extended attributes are restored as extended attributes. Encrypted, sealed and snapshot volumes and Fusion containers go to 7-Zip. Memory follows the file-system tree. On 998,000 files `zipper test` peaked at 599 MB and `7zz t` at 827 MB. The M10 rows `APFS`, `APM`, `Dmg` and `HFS` read native. Each restored attribute matches 7-Zip's `file:name` file byte for byte.

Built on 12 November 2026:

- The NTFS alternate data streams a WIM stores. `zipper extract --alternate-streams` writes each named stream of a file as an NTFS stream on Windows, in the staging folder after the file's data, as the [WIM reader reference](../reference/wim-reader.md#alternate-data-streams) describes. Stream names are checked on every platform. Without the option, and for extraction off Windows, a WIM with a named stream is still refused. `tests/wim_streams.rs` holds the checks. Its `on_windows` tests write streams and read them back, and they run only in the `Windows release candidate` workflow, which has not run them yet.

Left, on 12 November 2026:

- A run of the `Windows release candidate` workflow that passes the `on_windows` tests in `tests/wim_streams.rs`. Every other row of this milestone reads native or is refused for licence reasons in the parity matrix.
