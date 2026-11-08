# In-process NTFS reader

Zipper extracts NTFS volume images with its own Rust reader, in the tree `7zz x` writes. The default engine and `--zip-engine native` send a file here when its first sector is an NTFS boot sector that 7-Zip would open. A file that starts with a ZIP, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ, compound file, WIM or 7z signature goes to that reader first. `--zip-engine backend` hands every volume to 7-Zip.

The [disk reader](disk-reader.md) calls `zipper::native_ntfs::read_volume` with a reader bounded to the partition when a VHD, VHDX, MBR or GPT disk holds one NTFS partition. The reader seeks from the partition's first byte and takes its end as the volume's end.

## What it reads

The reader follows 7-Zip 26.05's NTFS handler, so it reads the MFT and not the folder indexes:

- The boot sector, with every check 7-Zip makes. The sector size is 512 to 4,096 bytes and a cluster at most 1 GiB.
- The MFT from the runs of record 0's unnamed data. The record size comes from the first record, a power of two from the sector size to 4 KiB.
- Each record's update sequence array, as 7-Zip checks it. Records whose signature 7-Zip skips, such as `BAAD` or a zeroed record, are skipped. A damaged record is refused even when it is not in use, as 7-Zip refuses it.
- Extension records. A record in use that names a base record in use joins it when the base has the sequence number it names. The data and names of a file that needs an attribute list are found this way, as 7-Zip finds them, and the attribute list itself is not read.
- Resident data, and non-resident data in runs over one or more pieces, with sparse runs.
- LZNT1 compression in units of 16 clusters. A unit with a hole is decoded, and a unit without one is read as stored.
- Bytes past a stream's initialized size, which read as zeros.

The `$INDEX_ROOT` and `$INDEX_ALLOCATION` folder indexes, `$Bitmap` and the security data are not read. A file's folder is the one its file name names, so a damaged index changes nothing, in Zipper and in 7-Zip.

## What it writes

The reader writes the tree `7zz x` writes on macOS and Linux:

- System files, records 0 to 15, in a folder `[SYSTEM]`, with the root folder's time. Their children, such as `$Extend/$Quota`, are written inside it too.
- A file whose folder record is missing, not a folder, not in use or of another sequence number in a folder `[LOST]`. A record in use without a name is named `[NONAME]-` and its record number. A system record without a name is shown only when it holds data.
- Every name of a record, except a DOS name that has a Win32 name in the same folder. A file with several names is a hard link, as `7zz x` links it.
- A `/` or `\` in a name becomes `_`.
- Files get the modification time of their standard information. A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. The umask limits both. Folders get mode `0o755` and their time, applied after all files are written.
- Links as the [WIM reader](wim-reader.md#what-it-writes) writes them. A relative symbolic link stays relative, with its backslashes turned into slashes. An absolute symbolic link or a junction is read against the volume's root and written as a relative link to the same place. A WSL link becomes a symbolic link to its target. Each link must stay inside the destination.

It never restores security descriptors, short names, object IDs, the hidden, system or archive attributes, or extended attributes.

## Alternate data streams

7-Zip writes a named data stream on macOS and Linux as a file named `file:stream`. The shared name check refuses a `:` in every reader. So Zipper refuses a volume whose file or folder outside `[SYSTEM]` has a named stream, as the WIM reader refuses one, and the error suggests `--zip-engine backend`. Data in a folder's unnamed stream counts as one, as 7-Zip writes it as `folder:`.

Every volume holds named streams in its system files, such as `$Secure:$SDS` and `$UpCase:$Info`, and Windows volumes hold `$Extend/$UsnJrnl:$J`. Zipper leaves out the named streams of system files and of the root folder, and writes the rest of `[SYSTEM]`. 7-Zip leaves out `$BadClus:$Bad`, and Zipper does too.

## Reparse points and encryption

A record with a reparse point is a link of the kinds above. A reparse point of any other tag, such as a deduplicated or cloud file, is refused by name, with the hint to use `--zip-engine backend`. A system record with a reparse point, a link folder with entries under it and reparse data outside its record are refused.

A file with the encrypted attribute holds EFS ciphertext, which 7-Zip writes as if it were the file's data. Zipper refuses it by name.

## Safety checks

The reader reads the whole MFT and plans every path before it writes anything, and it publishes nothing on failure. It refuses each volume that 7-Zip refuses to open, and each file 7-Zip fails to extract, so a volume with one bad file publishes nothing. It also refuses these:

- A run that ends past the volume. 7-Zip checks only its first cluster.
- Two streams that share a cluster, or one stream that reads a cluster twice. Hard links share a record and are not counted twice.
- A folder that lies inside itself, and an entry more than 256 folders deep. 7-Zip names such an entry `[TOO-LONG]`.
- Two entries of one name in one folder, after `/` becomes `_`. 7-Zip writes the second over the first.
- A folder with two names. 7-Zip writes its entries under the first only.
- A top-level entry named `[SYSTEM]`, or `[LOST]` when there are lost files. 7-Zip merges it with the folder it makes, so a file there can replace a system file.
- LZNT1 compression on clusters over 4 KiB, which NTFS does not write. It bounds a compression unit to 64 KiB.
- A name that is empty, is `.` or `..`, holds a NUL or is not valid UTF-16. 7-Zip cuts a name at its first NUL.
- A file name whose length passes the end of its attribute. 7-Zip compares the length in characters with the size in bytes, so it reads past the attribute.
- A file name or standard information stored outside its record.
- An MFT that starts outside the volume, a record size over 4 KiB, or more than 8,388,608 records.
- A piece of a stream that starts past its own end.

## Where Zipper reads more than 7-Zip

- Sparse runs in a stream that has the sparse flag and no compression unit. 7-Zip refuses each such file.
- Units of a stream with a compression unit and no compression flag, as ntfs-3g writes sparse files. 7-Zip decodes each unit with a hole as LZNT1. It refuses such a file, or writes it with other bytes when the unit happens to decode.
- Update sequence arrays with an entry for every 512 bytes on a volume with larger sectors. NTFS protects every 512 bytes whatever the sector size, as `mkntfs -s 4096` writes. 7-Zip expects one entry per sector and refuses such a volume.
- Named streams of system files, which Zipper leaves out and so never reads. 7-Zip fails the extraction when one of them is damaged.

`ntfscat` from ntfs-3g 2026.9.28 reads every file of the sparse and 4 KiB sector corpus volumes to the bytes Zipper writes.

## Limits

| Limit | Value |
| --- | --- |
| Records in the MFT | at most 8,388,608 |
| Record size | the sector size to 4 KiB |
| Entries | at most 1,000,000, as the shared tree allows |
| Names of records in use | at most 64 MiB in total |
| Runs of every stream written | at most 8,388,608 |
| Folder depth | 256 |
| Reparse data of one link | at most 16 KiB |
| Decoder memory | one compression unit of 16 clusters of at most 4 KiB, and its packed clusters |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

One pass over the MFT keeps 8 bytes for every record, and for each record in use its time, attributes and names. The runs of a file are read again from its records when it is written, and are not kept. The check that no two streams share a cluster keeps 16 bytes for each run until it ends, before the first file is written.

Peak resident memory on macOS, on volumes written by `write_volumes_at_the_documented_limits` in `tests/hostile_ntfs.rs`, is in this table.

| Volume | `zipper list` | `zipper extract` | `7zz l` | `7zz x` |
| --- | ---: | ---: | ---: | ---: |
| 998,000 empty files in 1,000 folders, a 1 GB MFT | 356 MB | 400 MB | 502 MB | 522 MB |
| One file of 512 MiB in 64 runs | | 4.5 MB | | 3.2 MB |

Most of the peak on the first volume is the shared tree, which keeps every path. The second shows that the data does not add to it.

## Differences from 7-Zip

`tests/ntfs_corpus.rs` requires every volume in `tests/data/ntfs-corpus` to extract to the tree recorded when it was written, and to the tree 7-Zip extracts where it is installed, apart from these differences:

- 7-Zip writes the named streams of system files, as `[SYSTEM]/$Secure:$SDS` and so on. Zipper does not write them.
- 7-Zip writes a named stream of any other file as `file:stream`. Zipper refuses the volume.
- 7-Zip writes an absolute link or a junction as an absolute path into the destination. Zipper writes a relative link to the same place.
- 7-Zip refuses a relative link with `..` in it and writes an empty file in its place. Zipper creates the link when it stays inside the destination.
- 7-Zip writes an encrypted file's ciphertext. Zipper refuses the volume.
- 7-Zip reads the sparse, ntfs-3g compression unit and 4 KiB sector volumes as the section above says. Zipper reads them as NTFS defines them.
- 7-Zip writes an entry deeper than 256 folders as `[TOO-LONG]`, the second of two entries of one name over the first, and the entries of a folder with two names under the first. Zipper refuses each.

The test writer in `tests/common/ntfs_writer.rs` also builds seeded volumes with stored, fragmented, compressed and extension record data, hard links and links, at clusters of 512 bytes to 4 KiB. Each must extract byte for byte to its source tree and to the tree 7-Zip extracts.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. A hard link whose first name the selection or strip leaves out is written from the next selected name of its record. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written. `--threads` has no effect on NTFS.

## Formal checks

Kani proves these rules under `[ntfs-fields]` in `verification/coverage.txt`:

- The boot sector reader never panics on any 512 bytes. A boot sector it accepts has a sector of 512 to 4,096 bytes, a cluster no larger than 1 GiB and a volume whose byte size fits 62 bits.
- The update sequence reader never panics on any record of 24 bytes in strides of 8, or of 64 bytes in one stride of 64. It changes only the last two bytes of each stride, to the bytes the array saved for it. A record number it accepts is the record's own or 0.
- The record header reader never panics on any 64 bytes, and keeps the attributes inside the record and aligned to 8 bytes.
- The attribute header reader never panics on any attribute of up to 88 bytes. An attribute it accepts is 24 bytes or more, a multiple of 8 and inside its bytes, with its name and value inside it.
- The run reader never panics on any run of up to 17 bytes, with fields of every width, any previous cluster, any room left in its piece and any volume size. A run it accepts is longer than 0 clusters, fits its piece and lies inside the volume, and has a hole only where holes are allowed.
- The run list reader never panics on any list of up to 4 bytes and any first and last cluster. The runs it accepts start at the piece's first cluster, fill it exactly and lie inside the volume.
- The file name reader, which reads the key of every index entry, never panics on any value of up to 72 bytes, and keeps the name inside the value. The standard information reader never panics either.

Unit tests in `src/lznt1.rs` decode 300 compressed units of every length back to their bytes. They compare the decoder with a separate port of 7-Zip's routine on 1,500 damaged streams and on 41,000 chunks of two tokens. Unit tests in `src/native_ntfs.rs` read back 20,000 random run lists.

`tests/hostile_ntfs.rs` builds its volumes with the test writer. It holds one case per refusal in the safety checks above, each of which must leave nothing beside the destination, both when extracting and when only verifying. Where 7-Zip is installed, it flips a bit in every third byte of the boot sector, every seventh byte of every record in use and 128 bytes of compressed data. Zipper must refuse each damaged volume 7-Zip refuses. It may refuse one 7-Zip reads only with a check this page lists as stricter, and two volumes both read must give the same tree. The one allowed difference there is a flipped compression flag, which Zipper follows and 7-Zip ignores.

`tests/data/ntfs-corpus/NOTICE.txt` names the source of the corpus. Every volume there was written by `mkntfs` and libntfs-3g from ntfs-3g 2026.9.28 on macOS. No sample there comes from Windows.
