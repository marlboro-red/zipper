# In-process CHM reader

Zipper extracts Microsoft Compiled HTML Help files (`.chm`, ITSF) and Microsoft Help 2 files (`.hxs`, ITOLITLS) with its own Rust reader. The default engine and `--zip-engine native` send a file here when it starts with `ITSF` version 3 and a header of 0x60 bytes, or with `ITOLITLS` version 1 and a header table at 0x28. The default engine sends a Help 2 file whose CAOL record holds no ITSF record to 7-Zip instead. 7-Zip lists such a file as one text item, and `--zip-engine native` refuses it. `--zip-engine backend` hands every file to 7-Zip.

## Supported files

A CHM file is a header, a directory and a content area. The directory is a list of chunks. Listing chunks (`PMGL` in CHM, `AOLL` in Help 2) hold the entries, and index chunks (`PMGI`, `AOLI`) hold a search index, which the reader skips. Each entry names a section, an offset and a size. Section 0 is stored bytes in the content area. The other sections are streams whose method the `::DataSpace` storage names. The reader supports these features:

- The CHM header of version 3 and the Help 2 header with five header sections and a CAOL record of 0x50 bytes.
- Entries in section 0, and entries in LZX sections, with every window from 32 KiB to 2 MiB and every reset interval from 1 to 2^16 blocks of 32 KiB.
- The reset table of each LZX section. It gives where each block of 32 KiB starts in the compressed stream. A table may list one entry more than its blocks, as the Free Pascal compiler `chmcmd` writes it, and that entry must then be the compressed size.
- A section that holds no data, with an empty reset table, as `.chw` files hold it.

LZX is the CAB decoder in `src/lzx.rs`, in its CAB mode. CHM frames LZX as CAB does. Each block of 32 KiB is its own slice of input, and the decoder resets its tables, its window and its E8 translation at each reset interval. The reader decodes every block as 32 KiB, as 7-Zip decodes it, so a last block must hold a whole 32 KiB.

## What it writes

The reader writes the tree `7zz x` writes on macOS and Linux:

- Every entry whose name starts with `/` and is longer than `/` itself is written, without its leading `/`. That includes the `#SYSTEM`, `#IDXHDR`, `$OBJINST` and other `#` and `$` system files at the top. The `::DataSpace` storage and other names that do not start with `/` are left out.
- An entry whose name ends with `/` is a folder. Folders are created first, then files in the order of their section and offset, as 7-Zip writes them. Of two entries with one name, the one written second replaces the first, as `7zz x -y` leaves it.
- Files get mode `0o644` and folders `0o755`, limited by the umask. CHM stores no times, so files get the time of extraction, as 7-Zip writes them.

## Safety checks

The reader checks the whole layout before it publishes anything, and it publishes nothing on failure:

- The header sections and the size that header section 0 records must end inside the file. 7-Zip calls a file that ends early an unexpected end.
- The directory is read in order, one chunk after another, as 7-Zip reads it. The links between listing chunks are never followed, so a chunk that links to itself or to an earlier chunk cannot loop. Every chunk must fit inside the directory and inside the file.
- In a listing chunk, the entries must end exactly where the quickref area starts. The stored entry count must match, though a CHM listing may store 0 there, as Lazarus 9-26-2 writes it.
- An encoded integer takes at most nine bytes, so it cannot overflow. A name has 1 to 8,192 bytes.
- A Help 2 directory lists exactly as many entries as its header counts.
- The name list, the content, control data, span information and reset table of each section must exist. The control data must start with `LZXC` version 2 or 3, with an interval and a window that are powers of two. The reset table must be version 2 or 3 with 8-byte entries, start at offset 0 and never go backwards.
- An entry must name a section the name list holds. An entry in section 0 must end inside the file. An entry in an LZX section may not reach past the blocks its reset table lists. Two entries of one LZX section may not share a byte.
- Each LZX block must hold at least one byte and decode to exactly 32 KiB, and its stream must end exactly at its last byte, as `src/lzx.rs` checks it.
- A section with any transform other than LZX is refused, as 7-Zip refuses it. That covers the DES encryption of Microsoft Reader `.lit` files.
- Names pass the same checks as tar names. `..`, `.`, empty segments, backslashes and `:` are refused.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Where it differs from 7-Zip

When 7-Zip cannot read a file's storage, it lists the raw directory instead, `::DataSpace` names included, and reports an error. Zipper refuses such a file. Zipper is also stricter than 7-Zip in these cases. Each refuses a file that 7-Zip extracts, and none has been seen in a real file:

- A directory chunk that is neither a listing nor an index. 7-Zip skips it and loses its entries.
- A directory chunk larger than 1 MiB, or an LZX block that packs more than 1 MiB. 7-Zip reads a block of up to 1 GiB.
- A name with a NUL byte, which 7-Zip cuts at the NUL, and a name that is not UTF-8.
- A name that the shared name check refuses, which 7-Zip rewrites.
- Two names that differ only in case or Unicode normalization, on a filesystem that ignores them. 7-Zip keeps the second.
- A file and a folder of one name. 7-Zip replaces the empty folder with the file.
- A section name in the name list that is not valid UTF-16.
- A name list, control data or Help 2 transform list that 7-Zip reads past 1 MiB.
- Two entries of one LZX section that share a byte, where 7-Zip's check skips the first entry of a section and the second starts in an earlier reset interval than the first ends in.
- An offset or size whose sum passes 2^64, which 7-Zip wraps.
- A Help 2 CAOL record that is neither 0x2C nor 0x50 bytes.

7-Zip finds a Help 2 signature anywhere in the first 256 KiB. Zipper recognizes one only at the start of the file, so a Help 2 file behind other data is not read as Help 2.

## Limits

| Limit | Value |
| --- | --- |
| Directory entries | at most 1,000,000, as the shared tree allows |
| Names | at most 64 MiB in total |
| Directory chunk | at most 1 MiB |
| Name list, control data and transform list | the first 1 MiB is read |
| Packed LZX block | at most 1 MiB |
| Decoder memory | one LZX window of up to 2 MiB, one block of output and one packed block |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader holds one directory chunk at a time. It keeps each entry that starts with `/` as 40 bytes and its name in one buffer, and each `::` entry in a map, so its memory follows the directory, not the data. The reset table is checked in batches of 8,192 entries and read again one entry at a time during extraction, so it is never held whole.

Peak resident memory on files that fill the limits, written by `write_chm_at_the_documented_limits` in `tests/hostile_chm.rs`, is in this table. Each holds 998,000 files in 1,000 folders, with paths of about 63 bytes, so its directory is about 70 MB.

| File | `zipper list` | `zipper extract` | `7zz l` | `7zz x` | Shared tree alone |
| --- | ---: | ---: | ---: | ---: | ---: |
| 998,000 empty files | 479 MB | 443 MB | 170 MB | 170 MB | 345 MB |
| 998,000 files of 8 bytes in one LZX section | 479 MB | 445 MB | 170 MB | 170 MB | |

The last column is `zipper list` on a cpio archive of the same paths, which uses the same shared tree. The CHM reader's own share is about 135 MB, for its entries and their names. The rest is the shared tree, so on these files Zipper peaks near three times what 7-Zip does. `--threads` has no effect on CHM.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. The reader decodes every file, also when a selection leaves some out, so damage in a file the selection leaves out still refuses the extraction. 7-Zip decodes only the reset intervals that hold selected files. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while a file's data is written.

## Formal checks

Kani proves these rules under `[chm-fields]` in `verification/coverage.txt`:

- The CHM header reader never panics on any 0x60 bytes, and the end it gives lies past both header sections. The Help 2 header reader never panics on any 0x13C bytes, and its end lies past all five header sections. Its CAOL record is 0x2C or 0x50 bytes.
- The ITSP and IFCM directory headers keep a chunk size from their smallest to 1 MiB.
- An encoded integer read from any position in up to 10 bytes takes 1 to 9 bytes, ends inside its input at the first byte below 0x80 and fits 7 bits a byte.
- A CHM listing chunk of 32 bytes and a Help 2 listing chunk of 64 bytes never panic. Every name lies past the chunk header and before the quickref area, and the count the walk returns is the number of entries it gave and the count the chunk stores.
- A reset table header it accepts has exactly the size of its entries, and as many entries as 7-Zip's block count or one more. The offset check accepts exactly the tables that start at 0, never go back and, with the extra entry, end at the compressed size.
- A block it reads is never empty, packs at most 1 MiB and lies inside its reset table.
- The LZX control data keeps an interval and a window that are exact powers of two.

`tests/chm_corpus.rs` extracts the `chmcmd` corpus, 56 generated CHM files and 28 generated Help 2 files, and compares each with its source and with `7zz x`. The generated files cover every window, intervals of 1 to 2^16 blocks, empty files, deep paths, up to 120 files and files that cross reset boundaries. The test damages 1,500 CHM and 800 Help 2 files at random in a header, the directory, a system file or an LZX block. It also changes every header and directory byte of two small files three ways. Zipper must refuse each one exactly when 7-Zip refuses it, or for a reason the list above gives, and must otherwise write the same tree. `tests/hostile_chm.rs` holds one case per refusal in the safety checks, each of which must leave nothing behind.

An adversarial review built about 1,450 crafted and mutated CHM and Help 2 files in Python and ran them against all three engines and 7-Zip. It found no file that Zipper extracts differently from `7zz x`, accepts where 7-Zip refuses, or leaves output behind for. It found that many sections over one LZX stream each decode the same reset interval again, as 7-Zip does. So 2,000 sections of one byte each decode 8 GB of LZX. The 1 MiB name list read caps that at about 37,000 sections.
