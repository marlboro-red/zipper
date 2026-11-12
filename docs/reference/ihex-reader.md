# In-process Intel HEX reader

Zipper reads Intel HEX files with its own Rust reader, in the tree `7zz x` writes. Intel HEX has no signature, so the reader finds it from the content alone, as 7-Zip does. Zipper sends a file here when no reader with a signature claims it and its first records pass the check below. 7-Zip 26.05 was run as a black box to learn every rule on this page.

The reader is the `ihex` feature, part of `more-formats`. A library built without it still finds Intel HEX files. Such a build refuses one with `zipper::NotBuilt` naming `ihex` and writes nothing. `tests/feature_subset.rs` checks the refusal.

## How a file is found

A file counts as Intel HEX when it starts with `:` and its first three records pass these checks, or fewer than three end in an end-of-file record:

- Each record is a colon, a count, a 16-bit address, a type, the count's data bytes and a checksum, all as pairs of hex digits in either letter case. The bytes after the colon sum to zero modulo 256.
- Each of the first three records is a data record with at least one byte, or the end-of-file record.
- Each record is followed by at most 16 CR or LF bytes and then the next colon.

So a file whose first records set an extended address is not Intel HEX to 7-Zip, and Zipper does not open it either. Files from `objcopy -O ihex` for addresses of 64 KiB and above start with such a record, and both tools refuse them.

A file whose first records pass but whose other records fail goes on to the other readers, as 7-Zip goes on to other formats. So a UEFI firmware volume further into the file extracts, as in 7-Zip. When no other reader claims the file, the Intel HEX reader refuses it and gives its reason. Choosing the reader reads an Intel HEX file once to its end-of-file record, before the two passes below.

## What it writes

Records are read in file order. A data record joins the run of the record before it when it starts at the byte where that run ends. Otherwise it starts a new run. Each run becomes one file:

- One run takes the archive's name without its last extension, as 7-Zip names an item without a name of its own. `firmware.hex` writes `firmware`, `a.b.hex` writes `a.b`, and a name without an extension gains `~`.
- Two or more runs take the names `0`, `1`, `2` and on, in the order the runs start in the file.
- A file with no data records writes nothing.
- Each file takes the archive file's modified time, as 7-Zip gives an item that stores no time.

A record's address is added to the current base. An extended segment address record (type 02) sets the base to its value times 16, and an extended linear address record (type 04) sets it to its value times 65,536. The last one read sets the base, whichever type it is. A start address record (type 03 or 05) changes nothing. So:

- A record that jumps forward, backward or into a run written before starts a new run, as does an overlap. No two runs merge.
- A record that passes 64 KiB with no address record stays in its run. The record after it starts a new run, because its address falls back to the start of the segment.
- An address record that makes the next record start where the run ends keeps the run going.
- Addresses do not wrap. A run that ends at 4 GiB, or past 1 MiB with segment addresses, never continues at a lower address.

## Checks

The reader opens a file only when 7-Zip opens it:

- Every record parses as above and its checksum holds.
- A record's type is 00 to 05.
- A data record holds at least one byte.
- The end-of-file record (type 01) holds no data, and every record other than data has address 0.
- An extended address record holds 2 bytes, and a start address record holds 4.
- Records follow each other with at most 16 CR or LF bytes between them and nothing else. A space, a tab or a NUL byte is refused.
- The file ends with an end-of-file record. One LF, CR or CRLF after it belongs to it. After that the file may hold only padding, at most 64 KiB of it. Padding is any run of CR, LF, space and tab bytes, with at most one DOS end-of-file byte (0x1A) among them. Text editors and DOS tools leave such endings, and 7-Zip extracts these files, so Zipper extracts them too.

## Safety checks

- The output goes to a hidden staging folder and is published only when the whole file reads, as with every reader.
- The reader reads the file twice. The first pass checks every record and counts each run's size, keeping one 8-byte size per run and no data. The tree writer keeps each run's name, as it does for every reader. Each run is charged against free space, or `--max-extracted-bytes`, before it is written. The second pass writes the data straight to each run's file.
- `--path`, `--include`, `--exclude`, `--strip-components` and `--strip-prefix` work as with every reader.

## Differences from 7-Zip

- Any other data after the end-of-file record is refused, as is padding longer than 64 KiB. 7-Zip extracts such a file and warns that data follows the end of the archive.
- A file of more than 1,000,000 runs is refused. 7-Zip reads it.
- A `\` or `:` in the name of a single run's file becomes `_`, as for every name Zipper derives from the archive's name. 7-Zip keeps them on macOS and Linux.
- A file with no data records publishes an empty destination folder. 7-Zip creates none.
- 7-Zip holds every run in memory. Zipper holds no data, so a run of 64 MiB peaks at about 4 MB, where 7-Zip peaks at 137 MB. A file of 1,000,000 one-byte runs peaks at about 175 MB when Zipper extracts it and 216 MB when it lists or tests it, where 7-Zip peaks at 143 MB. Most of that is likely the tree writer's record of each entry and name, which every tree reader keeps, but it has not been profiled.

## Limits

| Limit | Value |
| --- | --- |
| Runs | at most 1,000,000, the limit every tree reader shares |
| Data bytes in a record | 255, the largest count |
| Line breaks between records | 16 CR or LF bytes, as in 7-Zip |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Tests

`tests/common/ihex_writer.rs` writes records of every type. `tests/ihex_corpus.rs` checks named files of every rule above, 3,000 random files of every record type, line break and damage, and files from `objcopy -O ihex` or `llvm-objcopy` at several sizes, addresses and start addresses. Each one is refused when 7-Zip refuses it, and otherwise extracts byte for byte as `7zz x` extracts it, or is refused for data after the end-of-file record that is not padding. Other tests check the names of one and of many runs, and that each file takes the archive's modified time. They also check that one LF, CR or CRLF after the end-of-file record extracts, and that blank lines, spaces, tabs and one 0x1A after it extract as 7-Zip extracts them.

`tests/hostile_ihex.rs` cuts a small file of every record type at every byte, sets each byte to ten other values and removes each byte, and judges each result against 7-Zip the same way. A file of three good records and one bad one, followed by a UEFI firmware volume, extracts the volume as 7-Zip does. Other tests refuse padding past 64 KiB, refuse 1,000,001 runs before writing a file and accept 1,000,000, refuse output past `--max-extracted-bytes`, and check that nothing is published after a refusal. Kani proofs under `[ihex-fields]` in `verification/coverage.txt` cover the record reader and the step that places each record, and an exhaustive test covers every pair of hex digits.
