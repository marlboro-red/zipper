# In-process MSI and compound file reader

Zipper extracts Windows Installer packages with its own Rust reader, in the tree `7zz x` writes. An installer is a Compound File Binary file, the format of [MS-CFB], and the reader handles every such file the same way. That includes installer patches and merge modules, and documents such as `.doc`, `.xls`, `.ppt` and `.msg`. The default engine sends a file here when it starts with the signature `D0 CF 11 E0 A1 B1 1A E1`. A file that starts with a ZIP, tar, cpio, `ar`, RPM or CAB signature goes to that reader first. `--zip-engine backend` hands every compound file to 7-Zip.

[MS-CFB]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cfb/53989ce4-7b05-4f8d-829b-d08d6148375b

## What it extracts

7-Zip 26.05 lists a compound file's streams as files and its storages as folders. It opens one stream in place of the whole file in this case:

- A stream name holds a unit of the MSI name encoding, from `0x3800` to `0x4840`. That marks the file as an installer.
- Exactly one stream name ends in `.cab` or in `exe`, in any case after decoding.

That stream is the installer's main subfile, and `7zz x` extracts the archive inside it. Zipper does the same:

- When the main subfile starts with the CAB signature, Zipper extracts that CAB with its [CAB reader](cab-reader.md). The files, folders, times and attributes are the CAB's.
- When it does not, 7-Zip may open it as some other format. The default engine hands the file to 7-Zip, and `--zip-engine native` refuses it by name.
- Every other compound file extracts its streams and storages. An installer with two CAB streams, or a CAB and an `exe` stream, is one of them, as in 7-Zip.

Names follow 7-Zip's rules:

- A unit from `0x3800` to `0x483F` holds one or two characters of the set `0-9`, `A-Z`, `a-z`, `.` and `_`. The unit `0x4840` is `!`, which starts the name of an installer table.
- A unit below `0x20` becomes its number in brackets, so `\x05SummaryInformation` is `[5]SummaryInformation`.
- A slash becomes `_`, and an empty name is `[]`. Other units are UTF-16, and surrogate pairs join.
- The root storage is not a folder of its own.

## Metadata

A storage gets the modification time its directory entry holds, as a folder. A stream gets that time when it is not zero. Real files leave stream times zero. Files get mode `0o644` and folders `0o755`, which the umask limits.

## Safety checks

The reader checks the whole file before it writes anything, and publishes nothing on failure. It refuses each file that 7-Zip's compound file reader refuses, and these others:

- The header has the signature, minor version `0x3E`, version 3 with 512-byte sectors or version 4 with 4096-byte sectors, and 64-byte mini sectors. Its reserved fields are zero, and its mini stream cutoff is 4096. A version 3 file gives no directory sector count, and a version 4 file gives one.
- The FAT, DIFAT and mini FAT have no more sectors than the file holds. Every sector the reader reads for them, and every directory sector, lies whole inside the file.
- The DIFAT chain lists exactly the FAT sectors and then free entries. Each FAT sector is listed once and marked as a FAT sector.
- No two FAT links reach one sector, and no chain starts at a sector a link reaches. So every chain from a start ends without a cycle. The FAT state of each sector matches its use, and a sector past the end of the file is free.
- Each stream's chain has exactly the sectors its size needs, then ends. Its bytes lie inside the file, and the bytes of a mini stream lie inside the mini stream. Two streams never share a mini sector.
- The first directory entry is the root, and no other entry is. A stream has no children, and an empty stream starts no chain. An unused entry has no links and no data.
- The directory tree reaches each used entry exactly once. A link outside the directory, a loop and a link to an unused entry are refused. Storages nest at most 256 deep.
- Names pass the same checks as tar names after the decoding above. A name with `\`, `:` or NUL, `.` and `..` are refused, and so is an unpaired surrogate.
- Of two entries of one name and kind in one storage, the second one written stays. A stream and a storage of one name are refused.
- Output must fit in free space, or under `--max-extracted-bytes`. Each stream is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| File size | the FAT's reach, as 7-Zip allows, and at most 2 GiB with 512-byte sectors |
| Directory entries | at most 1,048,576 |
| Storage depth | 256 |
| One stream | the 32-bit size of version 3, or the FAT's reach in version 4 |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader keeps the FAT, the mini FAT and the directory in memory. The FAT holds 4 bytes, with 1 more for its check, for each sector in the file, and the reader allocates no FAT entry past the end of the file. The mini FAT and the directory hold no more than the file's sectors for them. On a file of 999,999 empty streams, `zipper test` peaked at 366 MB of resident memory and `7zz t` at 174 MB. About 120 MB of that is the directory and its walk, and the rest is the shared tree the cpio and CAB readers also use. It writes each stream through a 1 MiB buffer and reads runs of adjacent sectors together, so memory does not grow with stream size. `--threads` has no effect on compound files.

To extract an installer's CAB, the reader copies the CAB stream into a temporary file. That file sits beside the destination, or in the system temporary folder when Zipper only lists or tests. The copy must fit in free space there, and it is deleted when extraction ends. On Windows it carries the installer's Mark of the Web. The CAB reader then reads the copy, with every option the caller gave.

## Differences from 7-Zip

`tests/msi_corpus.rs` requires every file in `tests/data/msi-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. It also writes 160 compound files and compares each with 7-Zip. Those have both versions, mini and regular streams, nested storages, scattered sectors and embedded CABs. One more file needs a DIFAT. Setting `ZIPPER_MSI_SAMPLES` to a folder compares every file in it the same way. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip extracts the streams it can read from a truncated file, and reports an error for the rest. Zipper refuses the file.
- 7-Zip accepts a FAT that allocates sectors past the end of the file when no stream reads them. Zipper refuses it.
- 7-Zip extracts two streams that share mini sectors. Zipper refuses the file.
- 7-Zip extracts an item nested more than 256 storages deep under a `[LONG_PATH]` folder. Zipper refuses the file.
- 7-Zip extracts names with `\` or `:`, and replaces `.`, `..` and unpaired surrogates. Zipper refuses them.
- 7-Zip sets a stream's file time to the zero time of 1601, which macOS shows as 1677. Zipper leaves the time it was written.
- When the main subfile is a CAB that 7-Zip cannot open, 7-Zip extracts the streams instead. Zipper refuses the file with the CAB reader's error. A CAB the CAB reader refuses, such as a cabinet set, is refused too, and the error suggests `--zip-engine backend`.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the decoded names. For an installer's CAB they apply to the CAB's files. Progress counts each stream and its bytes as they are written. The cancel flag is checked between streams and while the CAB stream is copied.

## Formal checks

Kani proves four parts of the reader under `[msi-fields]` in `verification/coverage.txt`:

- The header reader never panics on any 512 bytes. A header it accepts has 512 or 4096-byte sectors, and its FAT and mini FAT sizes fit 32 bits.
- The directory entry reader never panics on any 128 bytes. An entry it accepts has a name of at most 31 units with no NUL inside it, and a size its FAT can address.
- The chain walk never panics on any table of up to 4 entries and any start and length up to 5. A chain it accepts visits only distinct sectors inside the table.
- The entry check claims only a free sector, the entry's own start.

Exhaustive tests in `src/native_msi.rs` cover the rest:

- `every_short_chain_is_accepted_exactly_when_it_ends_without_a_cycle` compares the chain walk with a reference that tracks every sector it visits. It covers every table of up to 4 entries, every start and every length up to 5.
- `every_small_fat_the_allocation_checks_accept_has_chains_that_end_apart` runs the FAT checks on every FAT of up to 4 entries and every set of chain starts. Every chain they accept ends, and no two chains share a sector.
- `every_small_directory_walks_as_a_recursive_reference_does` compares the directory walk with a recursive reference on every directory of up to 3 entries and 300,000 random ones of 4 to 7.
- `every_name_unit_decodes_as_7zip_decodes_it` checks each UTF-16 unit against a separate port of 7-Zip's decoder.

`tests/hostile_msi.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts files at every byte, and each cut must fail cleanly or extract the whole tree. It also damages metadata bytes, and each must fail cleanly or extract.

`tests/data/msi-corpus/NOTICE.txt` names the source and license of each sample. Two installers there come from `wixl` in msitools, and the documents come from olefile and Apache POI. The rest comes from `write_the_generated_corpus` in `tests/msi_corpus.rs`. Before the reader was committed, it also matched `7zz x` on 15 public installers and patches that are not in the corpus. Those include PuTTY, Python 2.7 and Visual C++ runtime installers, two `.msp` patches, and a version 4 installer with one CAB.
