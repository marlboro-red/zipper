# In-process ARJ reader

Zipper extracts ARJ archives with its own Rust reader. The default engine and `--zip-engine native` send a file here when it starts with the ARJ signature `0x60 0xEA` and a main header whose CRC-32 matches. A file that starts with a ZIP, tar, cpio, `ar`, RPM, xar, CAB or LZH signature goes to that reader first. `--zip-engine backend` hands every ARJ to 7-Zip.

## Supported archives

An ARJ archive is a main header, then a header and packed data for each entry, then an empty header that ends it. Each header carries a CRC-32 and may be followed by extended headers. The reader supports these features:

- Methods 0, which stores, 1 to 3, and 4. Methods 1 to 3 are the LZSS and static Huffman coding of LZH's `-lh6-`, with a 26,624-byte history, and decode with the [LZH reader's](lzh-reader.md) decoder in `src/lzh.rs`. Method 4 is an LZ77 coder with fixed codes for literals, lengths and distances, decoded by Zipper's own safe Rust in `src/native_arj.rs`.
- Binary and text entries, which both extract as their bytes, and folder entries.
- Extended headers after the main header and after each entry header. The reader checks each one's CRC-32 and skips it.
- A CRC-32 for each file, checked against the bytes the reader writes, including for an empty file.
- Bytes after the header that ends the archive, which the reader ignores.

The reader refuses these archives, and publishes nothing:

- An entry with the garbled flag, which ARJ sets on an encrypted entry. Neither Zipper nor 7-Zip reads them.
- A multi-volume archive, whose main header has the volume flag, and an entry that continues into or from another volume. The error suggests `--zip-engine backend`.
- An entry of a file type other than binary, text or folder, such as a volume label or a chapter label.

## Names

A name may use `/` or `\` between folders. ARJ for MS-DOS writes `\`, and ARJ writes `/` when it translates paths. A name must be UTF-8, which includes ASCII. Names then pass the same checks as tar names, so absolute names, `..`, drive letters and empty names are refused.

## Metadata

- A file gets its DOS time, read as local time, in two-second steps. A date or time that is out of range leaves the file's time as it was written. A folder gets its time after all files are written.
- An entry written on MS-DOS or Windows with the read-only attribute gets mode `0o444`, and every other file `0o644`. An entry written on Unix gets the permission bits of its access mode. A folder gets `0o755` unless a Unix access mode gives another. The umask limits all of them.

The reader never restores owners or the hidden, system or archive attributes.

## Safety checks

The reader checks every entry before it publishes anything, and it publishes nothing on failure:

- Every header starts with the signature and holds 30 to 2,600 bytes. Its first header size is at least 30 and lies inside it. Its name and comment each end with a zero byte inside it, and its CRC-32 matches. The main header has the archive file type.
- Every extended header's CRC-32 matches.
- A method above 4 is refused, as are methods 8 and 9, which carry no data.
- A stored entry packs exactly as many bytes as it holds. A folder entry carries no data.
- The data of every entry lies inside the file, and the archive ends with its empty header.
- A method 1 to 3 stream follows the checks the LZH reader makes, with a 26,624-byte window. A method 4 match reaches no further back than the start of the file and ends inside the declared size. Every stream ends with the file, padded with zero bits to a whole byte, at exactly the packed size.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 1,000,000 |
| One header | 30 to 2,600 bytes, as ARJ allows |
| One name | at most 8,192 bytes, though a header holds at most 2,600 |
| All names | at most 64 MiB |
| One file | 4 GiB less one byte, as the 32-bit size fields allow |
| Decoder memory | a window of at most 32 KiB, with a 64 KiB input buffer and the tables |

The reader reads one entry at a time and keeps only the tree that the cpio reader also uses, with each name. That tree keeps two more copies of each name, one to find repeated names and one for the list of entries. On an archive of 300,000 empty files whose names total 63 MB, `zipper test` peaked at 186 MB of resident memory, and `7zz t` at 113 MB. It writes each file through a 1 MiB buffer, so memory does not grow with file size. `--threads` has no effect on ARJ.

## Differences from 7-Zip

No ARJ writer is installed here, so `tests/common/arj_writer.rs` writes the archives, with the encoder in `tests/common/lzh_encoder.rs`. `tests/arj_corpus.rs` requires every ARJ in `tests/data/arj-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. It writes 120 archives with every method, folders, text entries, comments and extended headers. Each must extract to its source files and to the tree 7-Zip extracts. It encodes 280 token streams for methods 1 to 4, including streams whose matches reach too far, and damages 300 more by flipping a bit, cutting or lengthening them or changing the declared size. Zipper must refuse each one exactly when 7-Zip refuses it, and must otherwise extract the same tree. No archive in the corpus comes from ARJ itself. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip ignores an extended header whose CRC-32 does not match. Zipper refuses it.
- 7-Zip stops at a header whose CRC-32 does not match and extracts the entries before it, with an error. Zipper refuses the archive.
- 7-Zip extracts the parts of a multi-volume archive as separate files. Zipper refuses the archive.
- 7-Zip extracts a volume or chapter label as a file. Zipper refuses it.
- 7-Zip treats `\` in a name as part of the file name on macOS and Linux. Zipper reads it as a separator, as ARJ for MS-DOS wrote it.
- 7-Zip removes a leading `/` and `..` from a name and writes the file. Zipper refuses the archive.
- 7-Zip reads a name in the system code page. Zipper refuses a name that is not UTF-8.
- 7-Zip copies a stored entry's packed bytes whatever size it declares. Zipper refuses a stored entry whose sizes differ.
- 7-Zip restores no Unix mode. Zipper restores the access mode of an entry written on Unix.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on names with slashes. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries.

## Formal checks

Kani proves that the header reader never panics on any header of up to 34 bytes. The name it returns starts after the first header size and ends with a zero byte inside the header. It proves that exactly methods 0 to 4 are accepted. The Huffman table proofs that methods 1 to 3 rely on are on the [LZH reader](lzh-reader.md#formal-checks) page. The proofs are under `[lzh-arj-fields]` in `verification/coverage.txt`.

`tests/hostile_arj.rs` holds one case per refused layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts an archive of every method at every byte, and each cut must be refused. It flips one bit in every byte and decodes 5,000 random streams without a panic.
