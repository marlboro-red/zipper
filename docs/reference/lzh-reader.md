# In-process LZH reader

Zipper extracts LZH archives, also called LHA archives, with its own Rust reader. The default engine and `--zip-engine native` send a file here when its first header names a method such as `-lh5-` at offset 2 and a header level of 0 to 3. A file that starts with a ZIP, tar, cpio, `ar`, RPM, xar or CAB signature goes to that reader first. `--zip-engine backend` hands every LZH to 7-Zip.

## Supported archives

An LZH archive is a list of entries, each a header and its packed data, ended by a zero byte or by the end of the file. The reader supports these features:

- Header levels 0, 1, 2 and 3. Level 0 and 1 headers carry a checksum of their bytes. Level 2 and 3 headers carry a CRC-16 of the whole header in an extended header, and the reader checks it when it is there.
- Extended headers for the file name, the folder name with `0xFF` between folders, the Unix mode and the Unix time, and the Unix area that LHa for UNIX writes in a level 0 header. Other extended headers are skipped.
- Methods `-lh0-` and `-lz4-`, which store, `-lh4-`, `-lh5-`, `-lh6-` and `-lh7-`, and `-lhd-`, which marks a folder. The four `-lh?-` methods are LZSS with static Huffman codes and windows of 4, 8, 32 and 64 KiB. The decoder in `src/lzh.rs` is Zipper's own safe Rust, written from 7-Zip's decoder behaviour, and ARJ's methods 1 to 3 use it too.
- A CRC-16 for each file, checked against the bytes the reader writes, including for an empty file.
- Symbolic links, which LHa for UNIX writes as a `-lhd-` entry named `link|target` with a link mode.
- The level 2 header of LHA for OS-9/68k, whose size leaves out its own 2 bytes, read as Lhasa reads it.
- Bytes after the zero byte that ends the archive, which the reader ignores.

## Names

The folder name and the file name join with `/`. A `\` separates folders too, as LHA for MS-DOS writes it, and one trailing separator is dropped, as 7-Zip drops it. A name ends at its first zero byte. A name must be UTF-8, which includes ASCII. Names then pass the same checks as tar names, so absolute names, `..`, drive letters and empty names are refused.

## Metadata

- A file gets the permission bits of its Unix mode, and `0o644` without one. A folder gets its mode, and `0o755` without one. The umask limits both.
- A level 2 or 3 header carries a Unix time. A level 0 or 1 header carries a DOS time, read as local time, unless a Unix area or a Unix time extended header replaces it.
- A link is created after every file is written. Its target must stay inside the destination.

The reader never restores owners, groups or MS-DOS attributes.

## Safety checks

The reader checks every entry before it publishes anything, and it publishes nothing on failure:

- A header must be whole, with a checksum or header CRC that matches. A level 1 header must end with its OS byte and the size of its first extended header. A level 2 header may pad its size by one byte, and a level 3 header may not pad it.
- Every extended header holds its type and size, and a mode, time or CRC holds its full value. An entry has at most 256 extended headers of at most 64 KiB each, at most one file name, folder name and header CRC, and its extended headers fit its header size. A level 1 entry's extended headers fit its packed size.
- An extended header for 64-bit sizes or for archives split across disks is refused.
- A method other than those above is refused, with the method named. That includes `-lh1-`, `-lh2-`, `-lh3-`, `-lzs-`, `-lz5-` and `-lhx-`, which 7-Zip 26.05 does not read either.
- A stored entry packs exactly as many bytes as it holds. A folder entry carries no data.
- The data of every entry lies inside the file.
- A compressed stream follows 7-Zip's checks. Each block holds at least one symbol. Each table names only symbols it has, and each Huffman table uses every code of up to 16 bits exactly once, unless it names one symbol and reads no bits. A run of zero lengths stays inside its table. A match reaches no further back than the window or the start of the file, and ends inside the declared size. The last block ends with the file, padded with zero bits to a whole byte, at exactly the packed size.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 1,000,000 |
| One name | at most 8,192 bytes, with its folder |
| All names | at most 64 MiB, with link targets |
| Extended headers | at most 256 per entry, each at most 64 KiB |
| One file | 4 GiB less one byte, as the 32-bit size fields allow |
| Decoder memory | a window of at most 64 KiB, with a 64 KiB input buffer and the tables |

The reader reads one entry at a time and keeps only the tree that the cpio reader also uses, with each name. That tree keeps two more copies of each name, one to find repeated names and one for the list of entries. On an archive of 300,000 empty files whose names total 63 MB, `zipper test` peaked at 186 MB of resident memory, and `7zz t` at 118 MB. It writes each file through a 1 MiB buffer, so memory does not grow with file size. `--threads` has no effect on LZH.

## Differences from 7-Zip

`tests/lzh_corpus.rs` requires every LZH in `tests/data/lzh-corpus` to extract to the tree recorded for it, and to the tree 7-Zip extracts where it is installed. The corpus holds archives that LHa for UNIX 1.14i wrote at header levels 0, 1 and 2 with every method it writes. It also holds 30 archives from Lhasa's test suite, written by 16 LHA tools. Their systems include MS-DOS, Windows, OS/2, AmigaOS, OS-9, Human68K, Atari TOS, classic Mac OS and MorphOS. The test also writes 120 archives with the test writer in `tests/common/lzh_writer.rs`, of every level 0 to 2 and method. Each must extract to its source files and to the tree 7-Zip extracts. It encodes 320 token streams with every table form, including tables 7-Zip refuses, and damages 300 more streams by flipping a bit, cutting or lengthening them or changing the declared size. Zipper must refuse each one exactly when 7-Zip refuses it, and must otherwise extract the same tree. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip reads no level 3 header. Zipper reads them, and the files of the two OS/2 samples match what Lhasa extracts.
- 7-Zip ignores a level 2 header's size and reads the data straight after the extended headers. So it misreads a header padded by one byte, as LHA for OS-9 writes it, and fails its CRC. Zipper reads the data after the padding.
- 7-Zip ignores the header CRC. Zipper refuses a header whose CRC does not match.
- 7-Zip writes a link entry as folders named after the link and its target. Zipper creates the link, and refuses one whose target leaves the destination.
- 7-Zip ignores Unix modes and the Unix area of a level 0 header. Zipper restores them.
- 7-Zip removes a leading `/` and `..` from a name and writes the file. Zipper refuses the archive. LHMelt and MacLHA start names with a separator when they keep the full path, so Zipper refuses those archives.
- 7-Zip reads a name in the system code page. Zipper refuses a name that is not UTF-8.
- 7-Zip copies a stored entry's packed bytes whatever size it declares. Zipper refuses a stored entry whose sizes differ.
- 7-Zip takes a header whose method is not `-l??-` as the end of the archive. Zipper refuses it.
- 7-Zip reads the size of a level 1 header's first extended header straight after its OS byte. Zipper refuses a level 1 header with bytes after its OS byte, where LHa for UNIX would skip them.
- 7-Zip starts a level 2 header whose first byte is zero. Zipper ends the archive at that byte, as LHa for UNIX does. LHa pads its headers so that no level 2 header starts with one.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on names with slashes. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries.

## Formal checks

Kani proves that the level 0 and 1 header reader never panics on any header of up to 30 bytes. Its name and CRC lie inside the header, and a level 1 header ends with its OS byte and first extended header size. It proves the level 2 and 3 readers and the extended header reader never panic, and that every value they return lies inside its bytes. Every method ID maps to a power of two window from 4 to 64 KiB. For every list of five code lengths, the Huffman table builder accepts exactly the complete codes of up to 16 bits. Every 16-bit input then decodes to a symbol whose length is the one it was given, and two inputs that share that code decode to the same symbol. The proofs are under `[lzh-arj-fields]` in `verification/coverage.txt`, with those of the [ARJ reader](arj-reader.md).

Unit tests in `src/lzh.rs` build every table of four lengths up to 5 bits and 200 wide tables and compare each with a reference that decodes one bit at a time. They check the CRC-16 against its published check value and a reference that works one bit at a time. `tests/hostile_lzh.rs` holds one case per refused layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts archives of every level at every byte, and each cut must be refused unless it falls between entries. It flips one bit in every byte and reads 20,000 random headers without a panic.

`tests/data/lzh-corpus/NOTICE.txt` names the source and licence of the Lhasa samples. The LHa for UNIX archives there come from `write_the_lha_corpus` in `tests/lzh_corpus.rs`, run with LHa for UNIX built from its source, since Homebrew has no `lha` that writes archives.
