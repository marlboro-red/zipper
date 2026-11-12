# In-process CAB reader

Zipper extracts Microsoft Cabinet files and cabinet sets with its own Rust reader. Zipper sends a file here when it starts with the CAB signature `MSCF` and four zero bytes. A file that starts with a ZIP, tar, cpio, `ar` or RPM signature goes to that reader first.

## Supported cabinets

A cabinet is a header, a table of folders, a table of files and the data blocks of each folder. A folder is one compressed stream, and each file is a range of bytes in one folder. The reader supports these features:

- Stored folders, and MSZIP folders. An MSZIP block is `CK` and a raw deflate stream, which decodes with the last 32 KiB of the folder as its dictionary.
- Quantum folders with a window level of 0 to 21. The format names levels 10 to 21, and 7-Zip 26.05 also reads levels 0 to 9, so Zipper does too. A level below 15 still gets a 32 KiB window, one data block. Quantum is an LZ77 coder whose literals, match lengths and match distances pass through adaptive models and an arithmetic coder. The decoder in `src/quantum.rs` is Zipper's own safe Rust, written from 7-Zip's decoder behaviour. Its models carry over from one data block to the next in a folder, and its arithmetic coder starts again in each block.
- Several folders in one cabinet, and the reserved areas a header may give its header, its folders and its data blocks.
- Data block checksums. A block whose checksum field is zero is not checked, as the format allows.
- Names with the UTF-8 flag. A name without the flag must be ASCII or valid UTF-8. A backslash separates folders, as Windows writes it.
- Records with the folder attribute, which create folders.
- Bytes after the end of the cabinet, where a signed cabinet keeps its signature. The reader ignores them, as 7-Zip does.
- Cabinet sets, as the next section describes.

A cabinet with an LZX folder is refused whole with `CAB folder is compressed with LZX, which Zipper does not decode`, and nothing is written. The LZX decoder was removed on 12 November 2026 for licence reasons, as the [refused formats page](refused.md) records.

## Cabinet sets

A cabinet set is a chain of cabinets. Each header names the cabinet before it and the cabinet after it, and a folder may continue from the end of one cabinet into the start of the next. Open the set by its first cabinet, as `7zz x -tcab first.cab` does, with the other cabinets beside it. The reader then works as follows:

- It opens each next cabinet by the name its previous cabinet gives, in the same folder as the first. It reads the header and tables of every cabinet before it writes anything.
- Every cabinet must carry the set ID of the first and the cabinet number one above the cabinet before it, and must name a previous cabinet. A cabinet that names itself, or a chain that loops back, fails this check.
- A file record with folder `0xFFFD` or `0xFFFF` lies in the first folder of its cabinet, and one with `0xFFFE` in the last. The first folder of a cabinet continues the last folder of the cabinet before exactly when a record says so, as 7-Zip reads it. It must use the same compression method and window.
- A data block whose uncompressed size is 0 continues in the next cabinet. Its packed bytes are joined with the first block of the next part of its folder, and each part must pass its own checksum. The joined block packs at most 38,912 bytes.
- A file that appears in two cabinets with the same folder, offset, size and name is extracted once.
- One reader is open at a time. A cabinet is reopened only if its length, modification time and file identity are those it had when its tables were read. A rewrite in place that keeps all three is not detected.

The reader refuses these sets, and publishes nothing:

- A missing, unreadable or cut cabinet. The error names it.
- A cabinet whose set ID or number does not follow, or that names no previous cabinet.
- A file that continues from a cabinet before the first, or into a cabinet after the last.
- Two cabinets that disagree on whether a folder continues from one into the other, or a continued folder with another method.
- A record with folder `0xFFFF` in a cabinet with more than one folder.
- A block of size 0 that is not the last block of its cabinet's part of a folder, or whose folder does not continue.
- A cabinet other than the first, opened on its own. The error names the cabinet before it. 7-Zip reads the whole set from any cabinet in it.

The reader refuses these cabinets:

- A cabinet set that the previous section refuses.
- A cabinet whose previous or next cabinet name is empty, `.` or `..`, or holds `/`, `\`, `:` or NUL. A name that Windows reads as a device is refused on every system, so a set reads the same everywhere. Those names are `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM` or `LPT` followed by one digit or by `¹`, `²` or `³`. Case does not matter, and an extension or trailing dots and spaces do not change the result, so `con.cab` and `COM1.txt.` are refused. So the reader opens no cabinet by a path outside the folder of the first. A symbolic link in that folder is followed, as 7-Zip follows it, and its target must still pass the set checks. A next cabinet name must also be UTF-8.
- A Quantum folder whose window level is above 21, a folder compressed with LZX, or a folder with a method above 3.
- Two files that share bytes of their folder.
- A name that is neither flagged UTF-8 nor ASCII or UTF-8.

## Metadata

The reader restores what 7-Zip 26.05 restores on macOS and Linux:

- A file with the read-only attribute gets mode `0o444`, and every other file `0o644`. The umask limits both. On Windows, `0o444` sets the read-only attribute.
- Files get their DOS date and time, read as local time, in two-second steps. A date or time that is out of range leaves the file's time as it was written.
- Folder records get mode `0o755` and their DOS time, applied after all files are written.

It never restores the hidden, system or archive attributes, or the execute attribute.

## Safety checks

The reader checks the whole cabinet before it publishes anything, and it publishes nothing on failure:

- The header starts with the signature, has zero reserved fields and no unknown flags, and gives a cabinet size of at least 36 bytes. The file holds at least that many bytes. The file table starts inside the cabinet.
- The folder and file tables fit in the cabinet before they are read. Every record, name and data block lies inside it.
- Every file names a folder the cabinet has. A file or folder record sorts as 7-Zip sorts it, and a record that repeats the folder, offset, size and name of the record before it is dropped.
- Names pass the same checks as tar names after backslashes become slashes. Absolute names, `..`, drive letters and empty names are rejected, and a name ends at its NUL.
- No entry sits inside a file, and no entry reuses a name of another kind. A repeated file replaces the earlier one. A folder record carries no data.
- Each data block decodes to between 1 byte and 32 KiB, and only the last block of a folder may be shorter than 32 KiB. Its packed size is at most 38,912 bytes, and its checksum matches.
- A stored block packs exactly as many bytes as it holds. An MSZIP block starts with `CK`, and its deflate stream ends with a final block exactly at the block's last byte and size.
- A Quantum block follows 7-Zip's checks. It holds at least the 2 bytes of its first code. A match stays inside its data block and reaches no byte before the start of the folder. The block ends with at least 2 and at most 9 zero bits after the last bit the decoder reads, so its last byte is its last byte of data.
- A folder is decoded only until its last file ends, as 7-Zip decodes it, and a folder that no file uses is not read. Every block that is read is checked as above. So the work a cabinet costs is bounded by the data its files need, and a cabinet of blocks no file uses costs nothing. A file that ends past its folder's data is refused.
- Each data block that is read lies outside the byte ranges of the blocks earlier folders read. So folders cannot share blocks, and no block is decoded twice.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Folders and files | at most 65,535 each, as the header's 16-bit counts allow, in one cabinet and in a whole set |
| Cabinets in a set | at most 65,536, as the 16-bit cabinet numbers allow |
| One name | at most 8,192 bytes |
| All names | at most 64 MiB, with the previous and next cabinet names of a set |
| Data blocks in a folder | the 16-bit count the folder declares |
| One data block | 32 KiB decoded, 38,912 bytes packed |
| Decoder memory | at most 4 MiB, a Quantum window of 32 KiB to 2 MiB with 4 KiB of models, or the 32 KiB MSZIP history |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader keeps the file table in memory, with each name, and frees each name once the file is written. The shared tree that the cpio reader also uses keeps two more copies, one to find repeated names and one for the list of entries. On a cabinet of 65,535 empty files whose names total 62 MiB, `zipper list` peaked at 154 MB of resident memory, and `7zz t` at 147 MB. It decodes one folder at a time and writes each file in folder order through a 1 MiB buffer. The block buffers and each decoder are allocated when a folder first reads a block. One Quantum decoder is reused from folder to folder, its window grown to the largest one seen. The reader drops the Quantum window before an MSZIP or stored folder, so the 4 MiB budget holds across folders of mixed methods. So many folders cost no more than one. So memory does not grow with file size. `--threads` has no effect on CAB.

## Differences from 7-Zip

`tests/cab_corpus.rs` requires every CAB in `tests/data/cab-corpus` to extract to the tree 7-Zip extracted when it was written, and to the tree 7-Zip extracts where it is installed. It also writes 400 CABs with stored and MSZIP folders, and compares each with 7-Zip. It does the same for 220 Quantum CABs of every window level from 0 to 21. It damages 660 more Quantum CABs, by flipping a bit, cutting or lengthening a block or changing its size. Zipper must refuse each one exactly when 7-Zip refuses it, and must otherwise extract the same tree. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip extracts two files that name the same bytes of a folder. Zipper refuses the cabinet.
- 7-Zip reads a name without the UTF-8 flag in the system code page. Zipper refuses a name that is not ASCII or UTF-8.
- 7-Zip reads data blocks past a folder's declared count, for folders of more than 65,535 blocks that some Microsoft cabinets hold. Zipper decodes exactly the declared count and refuses a file that needs more.
- 7-Zip reads folders whose data blocks overlap. Zipper refuses them.
- 7-Zip removes a leading `\` and `..` from a name and writes the file. Zipper refuses the cabinet.
- 7-Zip joins a data block of size 0 with the next block anywhere in a cabinet. Zipper joins it only across cabinets, at the end of a continued folder.
- 7-Zip reads a whole cabinet set from any cabinet in it. Zipper reads a set only from its first cabinet.
- 7-Zip warns about a missing cabinet and extracts what it can. Zipper refuses the set.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on names with slashes. Progress counts each file and its bytes as they are written. The cancel flag is checked between files and between the data blocks a file skips over.

## Formal checks

Kani proves that the header, folder, file record and data block header readers never panic on any bytes. A header it accepts keeps the file table inside the cabinet. A Quantum folder it accepts has a window its decoder takes within the 4 MiB budget, no folder it accepts is compressed with LZX, and a data block it accepts fits the block buffers. It also proves that the checksum never panics on any short block and reserved area, and that the DOS time fields stay in range. For cabinet sets, Kani proves three more rules. A cabinet follows another only with its set ID, the next cabinet number without overflow and the previous cabinet flag. Every file record's folder field names a folder inside its cabinet, the first for a record continued from before and the last for one continued into the next. A block joined across cabinets stays inside the 38,912-byte block buffer. The proofs are under `[cab-fields]` in `verification/coverage.txt`.

`tests/cab_sets.rs` writes cabinet sets with the test writer in `tests/common/cab_writer.rs`, since no CAB tool on macOS or Linux writes them. It cuts folders of every method at random packed bytes, at block boundaries and inside one block across three cabinets. Each of 172 sets must extract to its source files byte for byte and to the tree `7zz x -tcab` extracts from the first cabinet. It also extracts one file that spans cabinets alone, and it holds one case for each refusal in the cabinet set section. Each refused set must leave nothing beside the destination. Where 7-Zip reads the set, as with a block split inside one cabinet, the test records that 7-Zip extracts it.

The Quantum decoder has four Kani proofs under `[quantum]`. Each model picks a symbol inside its totals and keeps them falling, through every rescale and reorder, for models of up to 5 symbols. After the arithmetic coder narrows to a symbol's interval, renormalizing keeps its code inside its range, with at most 16 new bits. The divisions that pick the interval are too heavy for Kani, which ran for over 12 minutes on them. So the decoder checks that the code lies inside the interval it picked, and `a_threshold_always_picks_an_interval_that_holds_the_code` checks that this holds for every total, about 800 ranges and six codes each. Every match length is 5 to 259 bytes, and every distance a model's slots give stays inside the window. The bit reader never panics and counts every bit it reads. A test encoder in `tests/common/quantum_encoder.rs` writes Quantum blocks from a seeded LZ77 parse or from a token list. Unit tests in `src/quantum.rs` decode 240 encoded inputs back at every window and drive every model through a reorder. They decode every 1 and 2 byte block without a panic and refuse every cut block, and they check each refusal above with a crafted block.

`tests/hostile_cab.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts stored, MSZIP and Quantum cabinets at every byte before the cabinet's end, and flips bits in every byte. Where 7-Zip is installed, it also requires that damage past a folder's last file is accepted by both. `lzx_folders_are_refused_by_name_with_nothing_written` refuses a lone LZX folder and an LZX folder after an MSZIP one.

`tests/data/cab-corpus/NOTICE.txt` names the source and license of the sample taken from the `cab` crate. The rest of the corpus comes from `write_the_generated_corpus` in `tests/cab_corpus.rs`, including one Quantum CAB for each window level from 10 to 21. No sample there comes from Microsoft's own `makecab`.

No adversarial review has run crafted cabinets against this reader yet. Three attempts stopped before running anything, because a safety check blocked the scripts that build the cabinets. Two reviews read the code instead and raised eight points. `tests/hostile_cab.rs` settles each one with a test against the release reader and, where it applies, against 7-Zip. The review of the Quantum decoder did run crafted cabinets, built in Rust with the test helpers. It ran 4,358 damaged Quantum cabinets and 180 crafted ones against both engines and 7-Zip. It found two problems, and tests now cover both fixes. Two windows could be held at once, past the budget, and Zipper refused windows below 2^10 that 7-Zip reads. The review of cabinet sets also built its cabinets in Rust. It ran about 30 crafted sets against both engines and 7-Zip, including split blocks, renamed and repeated halves, FIFOs, links, a cabinet replaced during extraction and 12,000 cabinets under a limit of 40 open files. It found one problem. Previous cabinet names were held without being counted toward the 64 MiB of names, and a test now covers the fix.
