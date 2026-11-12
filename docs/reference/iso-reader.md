# In-process ISO and UDF reader

Zipper extracts disc images with its own Rust reader, in the tree `7zz x` writes. It reads ISO 9660 with Joliet, Rock Ridge and El Torito boot images, and UDF 1.02 to 2.60 with metadata partitions. Zipper sends a file here when sector 16 holds a well-formed volume descriptor, or a UDF anchor at sector 256 of 2048 or 512 bytes. A file that starts with a ZIP, tar, cpio, `ar`, RPM, xar, CAB, compound file or 7z signature goes to that reader first.

## Which tree a disc gives

A hybrid disc holds two or three trees over the same files. 7-Zip 26.05 extracts one of them, and so does Zipper:

1. The UDF tree, when the disc has a UDF anchor at sector 256 and 7-Zip opens its file system.
2. Otherwise the last Joliet volume in the descriptor set, when one follows the primary volume.
3. Otherwise the primary volume, with Rock Ridge names, modes and links when its root holds a SUSP `SP` entry.

7-Zip gives up on a UDF tree, and opens the ISO 9660 tree instead, in these cases:

- An entry other than a file, a folder or a real-time file, such as a link or a device. `hdiutil makehybrid -udf` writes a link as such an entry.
- Extended allocation descriptors, an unknown partition map, a block size other than the sector size, or a volume sequence without a terminator.
- A damaged descriptor tag, a folder entry that its file identifier calls a file, or a folder that contains itself.

A disc with no ISO 9660 volume then fails, in 7-Zip and in Zipper. El Torito boot images extract only beside the ISO 9660 tree, because 7-Zip's UDF reader does not list them.

## Names

- ISO 9660 names drop a trailing `;1`, then one trailing `.`, so `README.TXT;1` is `README.TXT` and `EMPTY.;1` is `EMPTY`. Other versions, such as `;2`, stay.
- Joliet names are UCS-2 in big-endian order, and surrogate pairs join. They drop `;1` and a dot the same way.
- A Rock Ridge `NM` entry in the record's own system use area gives the name. It drops `;1` and a dot too.
- UDF names use compression 8 or 16. A name that is empty or only spaces becomes `[]`, as 7-Zip names it. UDF names keep `;1`.
- Each El Torito image is `[BOOT]/Boot-NoEmul.img`, with `NotBoot` when it is not bootable and `1.2M`, `1.44M`, `2.88M` or `HardDisk` for its media. A section entry's vendor text follows the media. With more than one image, each name starts with its number, as in `2-NotBoot-1.44M.img`.

## Metadata

- A Rock Ridge `PX` entry gives the mode of files and folders, masked to `0o777` and limited by the umask. Without one, files get `0o644` and folders `0o755`. UDF entries get those defaults too, as 7-Zip gives them.
- Files and folders get the recording date of their ISO 9660 record, converted from its time zone. Rock Ridge `TF` times are ignored, as 7-Zip ignores them. UDF entries get their modification time.
- A Rock Ridge entry whose `PX` mode is a link becomes a symlink. Its target comes from its `SL` entry, or from its data when it has none, as `hdiutil` writes it. Links follow the [ZIP reader's rule](zip-reader.md#symlinks), so only links that stay inside the destination are created.

## Safety checks

The reader reads the whole directory tree before it writes anything, and publishes nothing on failure. It refuses each image that 7-Zip refuses, and these others:

- A folder reached twice, through a loop or from two parents. 7-Zip skips a loop in ISO 9660 and extracts a shared folder once for each parent.
- Folders nested more than 256 deep in ISO 9660. A UDF tree deeper than 1,024 gives way to ISO 9660, as in 7-Zip.
- An extent, a folder, a continuation area or a boot image that lies past the end of the image, or a UDF extent outside its partition.
- A record or descriptor whose fields disagree, such as a both-endian field whose halves differ. A record that crosses its sector or runs past its folder, nonzero bytes after a sector's last record, and a multi-extent file without its last extent are refused too.
- Interleaved files and records with extended attribute records. 7-Zip reads an interleaved file as if it were contiguous.
- Rock Ridge fields 7-Zip reads differently than they mean. That is a name or link that continues into a second entry, a repeated `NM`, `PX`, `SL` or `CE`, a relocated folder, or an `NM`, `PX` or `SL` entry kept in a continuation area. Continuation areas are followed to check them, at most 32 deep, and a loop among them is refused.
- A `PX` mode of another file type than its record, such as a device, or a link target longer than 4,096 bytes.
- A damaged El Torito catalog, entry or extension chain. 7-Zip reports such a catalog and extracts no boot image from it.
- A UDF file whose extents do not hold its size or are not recorded, which 7-Zip fails to extract. A reserved allocation descriptor type.
- A UDF folder whose extents overlap, and a folder of more than 1,000,000 entries, refused while it is read.
- Names with `/`, `\`, `:` or NUL, and names that are empty, `.` or `..` once a version and dot are dropped. Invalid UTF-8 outside Joliet and UDF, an unpaired surrogate, and a UDF name of unknown compression. Two entries of one name in one folder are refused, after the rules above, including a root folder named `[BOOT]` beside boot images.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

These images are refused with an error that names the reason.

- UDF with 512-byte sectors, and UDF with several logical volumes, which 7-Zip names by volume.
- ISO 9660 with blocks other than 2,048 bytes.

## Limits

| Limit | Value |
| --- | --- |
| Entries | 1,000,000, the limit of the tree the cpio and CAB readers share |
| Folder bytes read | 1 GiB in all, counting continuation areas |
| One UDF folder | 64 MiB |
| ISO 9660 folder depth | 256 |
| UDF folder depth | 1,024 |
| Volume descriptors | 256 |
| El Torito images | 64, each with at most 32 extension entries |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader keeps one node for each entry, with its name, mode, time and extents, and reads one folder at a time. So memory grows with the image's folders, not with its file data, and data is copied through a 1 MiB buffer. On a Joliet image of 998,000 empty files in 1,000 folders, `zipper test` peaked at 363 MB of resident memory and `7zz t` at 254 MB. Most of Zipper's share is the shared tree, which keeps each full path twice. On a UDF image of 99,900 files, `zipper test` peaked at 39 MB and `7zz t` at 35 MB. `--threads` has no effect on disc images.

## Differences from 7-Zip

`tests/iso_corpus.rs` requires every image in `tests/data/iso-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed, modes included. It also writes discs of every kind with `tests/common/iso_writer.rs` and compares each with 7-Zip. Those are ISO 9660 alone, with Joliet with and without versions, with Rock Ridge, and multi-extent files. They also cover UDF 1.02, 1.50, 2.01, 2.50 and 2.60 with short, long and inline descriptors, extended file entries and metadata partitions, hybrids of each, El Torito images of each media, and 100 random discs. Names that drop versions and dots, `[]` names and surrogate pairs are compared too, as are ten damaged UDF trees that must give way to ISO 9660 as they do in 7-Zip. Setting `ZIPPER_ISO_SAMPLES` to a folder compares every image in it the same way. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip extracts what it can from a damaged or truncated image and reports an error. Zipper refuses the image.
- 7-Zip refuses to create a link whose target holds `..`, even one that stays inside, and writes an empty file. Zipper creates the link.
- 7-Zip extracts a UDF file reached from two folders under both. Zipper does too, but refuses a folder reached from two folders.
- 7-Zip extracts a disc whose Rock Ridge fields it misreads, under names and modes that differ from what the fields say. Zipper refuses it.
- Zipper refuses names with `\` or `:`, and two entries of one name in one folder.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the names above. Progress counts each file and its bytes as they are written. The cancel flag is checked before each entry and while a file is copied.

## Formal checks

Kani proves these parts of the reader under `[iso-fields]` in `verification/coverage.txt`:

- The ISO 9660 directory record reader never panics on any record of up to 40 bytes. A record it accepts has a name of at least one byte and a system use area inside the record, and the record lies inside its bytes.
- The volume descriptor reader never panics on any 190 bytes. A primary or supplementary volume it accepts has a whole 34-byte root folder record.
- The SUSP entry reader never panics on any area of up to 16 bytes, and every name and link range it returns lies inside the area.
- The Rock Ridge link reader never panics on any 8 bytes of components, and its target is at most twice as long.
- The `SP` check, the El Torito validation and boot entry readers, and the recording date arithmetic never panic or overflow on any input.
- The UDF tag reader never panics on any 22 bytes, and a tag it accepts covers only bytes it was given. It checks the tag checksum and the CRC-16 of the descriptor.
- The UDF file entry reader never panics on any 232 bytes of either kind, and its allocation descriptors lie inside the block.
- The allocation descriptor reader never panics on any 32 bytes, and short or long descriptors fill their area exactly.
- The file identifier reader never panics on any 48 bytes, and its name lies inside its padded length. The partition map reader never panics on any 70 bytes, and the time zone of a UDF time stays within a day.

Exhaustive tests in `src/native_iso.rs` and `src/udf.rs` cover the rest:

- `every_small_folder_graph_walks_as_a_recursive_reference_does` runs the folder walk on every graph of 4 folders with up to 2 children each, at three depth limits, over 2,700,000 walks. Both the ISO 9660 and UDF readers walk with it. It must find each cycle, shared folder and depth overrun where a recursive reference finds it, and otherwise list the same nodes under the same parents.
- Tests check the CRC against its published value, every damaged byte and cut of a tag, name decoding and the version and dot rules.

`tests/hostile_iso.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts images at every 509th byte, and each cut must fail cleanly or extract the whole tree. It also damages every byte of the folders, the UDF file entries and the main descriptors, and each must fail cleanly or extract.

`tests/data/iso-corpus/NOTICE.txt` says how each image there was made. All come from `hdiutil makehybrid` on macOS. `xorriso` and `mkisofs` were not installed, so the corpus has no image from them. The generated discs cover the Rock Ridge and El Torito forms those tools write instead.
