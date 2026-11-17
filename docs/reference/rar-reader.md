# In-process RAR reader

Zipper extracts RAR 5 archives and RAR 4 archives that are not solid with its own Rust reader and decoders. Zipper sends a file here when it starts with the RAR 5 signature `Rar!\x1A\x07\x01\x00` or the RAR 4 signature `Rar!\x1A\x07\x00`. Every RAR goes to the reader. RAR 5 archives encrypted with AES-256 are decrypted with a password, as [Encryption](#encryption) describes. RAR 5 and RAR 4 volume sets are read from their first volume, as [Volumes](#volumes) describes. An encrypted RAR 4 archive, a RAR 4 solid archive and a damaged first header are refused by the reader, with the reason in the error.

## Sources

The user decided on 4 November 2026 that Zipper reads RAR with its own decoder and that its authors never read the `unrar` source or 7-Zip's RAR code. Both carry unrar's licence restriction. The reader was written from these sources only:

- RARLAB's [RAR 5.0 archive format](https://www.rarlab.com/technote.htm) technote, for the container, the layout of its encryption records, and the volume flag, volume number, split flags and checksums of a volume set.
- libarchive's BSD licensed `archive_read_support_format_rar5.c`, for the RAR 5 compression, the filters and the details the technote leaves out. `src/rar5_lz.rs` follows it, and `LICENSE-libarchive` keeps its notice.
- libarchive's BSD licensed `archive_read_support_format_rar.c`, for the RAR 4 container, the RAR 2.9 compression, its PPMd blocks and the standard filters. `src/rar4.rs` and `src/rar4_lz.rs` follow it, and `LICENSE-libarchive` keeps its notice. It also names the RAR 4 main header flags for a volume, the first volume and the naming scheme, and the split flags of a file header.
- The `ppmd-rust` crate's PPMd variant H decoder with the range coder RAR uses, which Zipper's 7z and ZIP readers already depend on.
- libarchive's test archives, which `tests/data/rar-corpus` holds under the same notice.
- `7zz` and `bsdtar` run as black boxes on test archives, for choices neither source settles.
- RARLAB's `rar` 7.23 and 6.24 and `unrar` 7.23, run as black boxes to write and extract the corpus in `tests/data/rar-written`. They also settled the details of RAR 5 encryption the technote leaves out, as [Encryption](#encryption) lists, and the RAR 4 volume details libarchive leaves out, as [Volumes](#volumes) lists. The user approved them as test tools on 14 November 2026. Nobody read their source.
- The `aes`, `hmac`, `sha2` and `zeroize` crates, which the ZIP and 7z readers already use, for AES-256, HMAC-SHA256 and clearing keys.

## RAR 5 archives

A RAR 5 archive is the signature, a main header, a header for each file or service record, and an end of archive header. Each header carries a CRC-32 of its bytes. The reader supports these features:

- Stored entries and the five compression methods, with dictionaries from 128 KiB to 2 GiB. Compression version 0 and version 1 with the flag that marks a version 0 stream are read.
- The delta, E8, E8E9 and ARM filters.
- Solid archives, where each file continues the window, tables and distances of the file before it.
- Folders, Unix and Windows symlinks and hard links. A Windows symlink's `\` separators become `/`, and its target passes the same checks as a Unix one. A `\??\` or UNC target then starts with `/` and is refused. On macOS and Linux a drive target such as `C:\x` becomes the relative link `C:/x`, which stays inside the destination. Windows refuses it.
- CRC-32 and BLAKE2sp file checksums. BLAKE2sp is Zipper's own safe Rust in `src/blake2sp.rs`.
- AES-256 encrypted entries and encrypted headers, with the key-dependent checksums RAR writes for them, as [Encryption](#encryption) describes.
- Service headers, such as comments, quick open data and NTFS streams. The reader skips them.
- Bytes after the end of archive header, which the reader ignores, as RAR does.

The reader refuses these archives, and publishes nothing:

- A volume set that fails a check [Volumes](#volumes) lists.
- A self-extracting archive. The executable reader sees the PE file and refuses the data after it, as it does for every archive after an executable.
- Junctions and file copies.
- RAR 7 compression, a file of unknown size, and a compression method or host OS RAR 5 does not define.

## RAR 4 archives

A RAR 4 archive is the signature, a main header, then blocks for files, service records and old comments, each with a CRC-16 of its header. The reader supports these features:

- Stored entries, and entries compressed with RAR 2.9's LZ and Huffman coding, the algorithm RAR 3 and RAR 4 write for every method.
- PPMd variant H blocks that start a new model, mixed with LZ blocks.
- The standard filters RAR's virtual machine runs, recognized by their program's CRC-32 and length as libarchive recognizes them. These are delta, E8, E8E9, RGB and audio. The Itanium filter is refused with every other program, as bsdtar refuses it.
- Folders, Unix symlinks, DOS times with extended time fractions, Unix modes and the read-only attribute.
- An archive that ends without an end of archive header, as libarchive and 7-Zip read it.

The reader refuses these RAR 4 archives, and publishes nothing:

- A solid archive, or a solid entry. libarchive does not read them either, so nothing here could check the result.
- An encrypted archive or entry.
- A volume set that fails a check [Volumes](#volumes) lists.
- An entry compressed with the RAR 1.5 or RAR 2.0 algorithms.
- A PPMd block that continues the model before it, and a PPMd block that holds a filter. The `ppmd-rust` decoder has no way to restart its range coder on a kept model, so `test_read_format_rar_ppmd_lzss_conversion.rar` is refused. A debugging run that let it continue matched bsdtar's output on its first PPMd block of 32 KiB and the 40 MB of LZ data after it, by CRC-32.
- A filter program that is not one of the standard filters, since Zipper runs no RAR virtual machine.

A RAR 4 name is UTF-8, or RAR's Unicode encoding when its flag is set and the name holds a zero byte. A name that is not UTF-8, in a code page, is refused. RAR's Unicode encoding holds 16-bit units, so RAR 6.24 keeps only the low 16 bits of a character outside the Basic Multilingual Plane. The name `emoji 🎉.txt` extracts as `emoji \u{F389}.txt` through Zipper and through `unrar`. RAR 4 writes `\` between folders on every host, so the reader reads it as a separator.

## Encryption

The reader decrypts RAR 5 archives encrypted with AES-256, both file data that `rar -p` writes and headers that `rar -hp` writes, in stored, compressed and solid archives. Pass `--password`, or set `password` in `ExtractOptions` or `native_rar::ReadOptions`. `list` and `test` take `--password` for RAR too. Without a password, an archive with encrypted headers or with any encrypted entry fails with `native_rar::PasswordNeeded`, since the reader decodes every entry. On Unix, when standard input is a terminal, the command line then asks for the password and tries again. RAR 4 encryption is refused by name.

The technote gives the layout of the file encryption record and of the archive encryption header. Each holds a version, flags, a KDF count, a 16-byte salt and, when flag 0x0001 is set, a 12-byte password check value. The file record also holds a 16-byte IV. The technote says the key comes from PBKDF2 with 2^count rounds, and that flag 0x0002 makes the checksums depend on the key. It says each header after the encryption header starts with its own IV and is padded to 16 bytes. It leaves out how the key, the check value and the changed checksums are made, and points to the `unrar` source, which nobody read. Runs of `rar` and `unrar` 7.23 as black boxes, on archives whose password and contents were known, settled each of them:

- The key is PBKDF2 with HMAC-SHA256 of the UTF-8 password and the salt, over 2^count rounds, in one 32-byte block. File data is AES-256 in CBC mode from the record's IV, padded with zeros to 16 bytes, and the packed size counts the padding.
- The hash key is the PBKDF2 value after 2^count + 16 rounds. The check value is the value after 2^count + 32 rounds, folded to 8 bytes by XOR, then the first 4 bytes of the SHA-256 of those 8.
- With flag 0x0002 the stored CRC-32 is the HMAC-SHA256 of the CRC-32's 4 little-endian bytes under the hash key, folded to 4 bytes by XOR. The stored BLAKE2sp hash is the HMAC-SHA256 of the 32-byte hash under the hash key.
- `rar` and `unrar` keep only the first 127 characters of a longer password, and so does Zipper.

`tests/hostile_rar.rs` builds encrypted archives with the `pbkdf2` crate rather than Zipper's own key chain, and `unrar` must extract them to the same files Zipper does. Every archive `rar` wrote with `-p` and `-hp` must extract to its recorded tree, as the [corpus written by RAR](#corpus-written-by-rar) describes.

A password is checked in these ways, and a wrong one publishes nothing:

- When a record has a check value, the reader checks the password against it before it reads any byte of the entry. A wrong password fails with `the password is wrong for encrypted RAR 5 entry`, or `for the encrypted RAR 5 headers`. A check value whose last 4 bytes do not match the SHA-256 of its first 8 is damaged, and the archive is refused.
- Without a check value, a wrong password fails on the entry's CRC-32 or BLAKE2sp, or on the header CRC-32 of encrypted headers. The error then says the password is wrong or the entry or headers are damaged. An encrypted entry with no check value and no checksum is refused before it is read, since a wrong password could not be detected.
- Encrypted data that is not a whole number of 16-byte blocks is refused. A stored encrypted entry must pack its size rounded up to 16 bytes, and a compressed one may end with up to 15 bytes of padding after its last block.

A KDF count above 24 is refused before any key is derived. That is the cap the 7z reader puts on its own key derivation, and `rar` 7.23 writes 15. The reader derives a key once for each salt and count, and `rar` 7.23 wrote one salt for every file of an archive. An archive may ask for at most two derivations at the cap, 2 × (2^24 + 32) PBKDF2 rounds, which is about 1,000 derivations at a count of 15. It may also ask for at most 1,024 derivations in all, whatever their count, since each costs a fixed amount of work besides its rounds. Known keys are found in a hash map. So a hostile archive cannot hang extraction with one huge count or with many salts. The adversarial review built an 18 MB archive of 320,000 folder entries, each with its own salt at a count of 0. Before the second limit it cost 35 times the work of two derivations at the cap. The password, the keys and the hash key are cleared from memory after use.

## Volumes

`rar -v` splits an archive into volumes. Each volume is a whole archive with its own signature, main header and end of archive header, and a file that does not fit continues in the next volume. Zipper reads RAR 5 and RAR 4 sets, stored, compressed, solid and RAR 5 encrypted ones. A RAR 5 entry split across volumes may be compressed, encrypted or part of a solid stream. Pass the first volume. Zipper finds each volume after it by name and opens it when the volume before it ends.

- RAR 5 names volumes `name.part1.rar`, `name.part2.rar` and on. RAR 4 does too when its main header sets flag 0x0010. `rar` pads the number to the digits it expects the set to need, so a set of 4 volumes in the corpus starts with `name.part01.rar`. Zipper keeps the width and spelling of the first name. `rar 7.23 -vn` still writes RAR 5 volumes with these names.
- Older RAR 4 sets, from `rar -vn`, are named `name.rar`, `name.r00` to `name.r99`, then `name.s00` and on to `name.z99`. That is 901 volumes at most.
- Zipper refuses a volume after the first when it is named instead of the first, and names the first volume in the error. `unrar` starts from the first volume in that case. Zipper refuses instead, as it does for the second or third volume of a raw split archive.

Zipper checks each set in these ways, and publishes nothing when a check fails:

- Each volume after the first must exist and be a regular file, or the error names the missing volume.
- A RAR 5 volume carries its volume number in its main header, absent in the first. Volume N must give N - 1. A RAR 4 main header marks the first volume with flag 0x0100, and only the first volume may set it. A RAR 4 volume without the flag is refused when it is named as the archive, even when it is the first of its set.
- Every volume after the first must mark itself as a volume. It must agree with the first on whether the archive is solid, whether RAR 5 headers are encrypted, and which RAR 4 names it uses.
- An end of archive header that says more volumes follow is refused when the main header does not mark a volume. So is a volume number on a RAR 5 archive that is not a volume.
- Every RAR 4 volume of a set must end with its end of archive block, so a last volume cut between blocks is refused. When the block gives a volume number, it must be the volume's own.
- An entry that continues into the next volume must be the last block before the end of its volume, apart from service headers. The next volume must start with its continuation. The continuation must have the entry's name, size, method and host, and for RAR 5 its directory flag and encryption record. A continuation that nothing before it continues into is refused.
- Each part but the last carries the CRC-32 or BLAKE2sp of its own packed data, and the last part the checksum of the whole file. Zipper checks both kinds. A volume opened twice must not change between the two opens.
- An error in any volume after the first names that volume.

RAR 5 gives a set no identity, and RAR 4 none Zipper reads. So a volume of another set with the right number, flags and layout is caught only by the checksums of the entries that cross into it or lie in it. A whole entry inside such a volume extracts when its own checksum matches.

`rar` and `unrar` run as black boxes settled what the technote and libarchive leave out:

- A RAR 5 part that continues has its tweaked-checksum flag cleared, and its CRC-32 is the plain CRC-32 of its packed bytes. Every part of an encrypted entry repeats the same salt and IV, and the AES-CBC stream runs on across parts whose sizes are not whole blocks. Each volume of a set with encrypted headers repeats the same encryption header.
- A RAR 4 end of archive block of a volume sets flag 0x0001 when more volumes follow. It gives a CRC-32 of the volume's bytes before the block when flag 0x0002 is set, then a 16-bit volume number from 0 when flag 0x0008 is set. Zipper reads the number and does not check that CRC-32, since each header and file carries its own checksum.
- A RAR 4 part that continues carries the CRC-32 of its own packed bytes, and the last part the CRC-32 of the whole file.

At most 65,536 volumes are read, the most a RAR 4 end block can number. Zipper keeps the volume it reads headers from open. While it reads an entry that crosses volumes it opens one of the volumes after it at a time, plus the volume where the entry ends, so at most three volumes are open at once. For each crossing entry it keeps one small record per volume the entry crosses, and nothing for the volumes it has passed. On a set of 65,536 volumes holding one stored file split a byte per volume, `zipper test` peaked at 24 MB of resident memory.

## Names

A name is UTF-8 with `/` between folders, as RAR 5 writes it. Names then pass the same checks as tar names, so absolute names, `..`, backslashes, drive letters and empty names are refused. A file with a version record extracts as `[VER]/N/name`, as 7-Zip names it.

## Metadata

- A file gets its modification time, to the nanosecond when the time record carries it. RAR 5 times are UTC. A RAR 4 time is a DOS time, read as local time, with the extended time's fraction in 100 ns steps.
- A file written on Windows with the read-only attribute gets mode `0o444`, and every other file `0o644`. A folder gets `0o755`. An entry written on Unix gets the permission bits of its mode. The umask limits all of them.

The reader never restores owners, creation or access times, or the hidden, system or archive attributes.

## Safety checks

The reader checks every entry before it publishes anything, and it publishes nothing on failure:

- Every header holds 1 byte to 2 MiB, its size field is at most 3 bytes, and its CRC-32 matches. The first header is the main header, and the archive ends with its end of archive header.
- Every number is at most 10 bytes and fits 64 bits. Every field, extra record, name and link target lies inside its header.
- A stored entry packs exactly as many bytes as it holds. A folder carries no data.
- A compressed block's check byte matches, and the block lies inside its file's packed data. Tables are complete or under-full. A code that an under-full table leaves unused is refused when it is read.
- A match reaches no further back than the start of its stream or its dictionary. No literal or match writes past the declared size. A filter lies inside its file, after the filter before it, and spans 4 bytes to 4 MiB and at most half the dictionary.
- The packed data ends where the decoded file ends. Each file's CRC-32 and BLAKE2sp must match.
- Every RAR 4 header holds at least 7 bytes and its CRC-16 matches. Its fields, name, salt and extended time lie inside it, and its data lies inside the file.
- A RAR 4 match reaches no further back than the start of its file or the 4 MiB window. A RAR 4 filter lies inside its file, after the filter before it unless a program reset dropped it, and inside the 256 KiB filter memory, and its length register equals its block length.
- A RAR 4 archive may start new PPMd models only while their sizes stay inside the allowance in the limits below. The allowance is shared by every file in the archive. RAR 6.24 with `-mcT+` writes an archive this allowance refuses, as the [corpus written by RAR](#corpus-written-by-rar) shows. An adversarial review built an 8 KB archive that started a 256 MiB model 1,000 times for one byte of output, which took 3.9 s of CPU before the allowance. The review of the RAR 4 filters spread the restarts over 2,000 files of 1 byte, four to a file, in 162 KB. While each file had its own allowance, Zipper ran 137 G instructions on it and bsdtar 3.7 G.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.
- An encryption record or encryption header lies inside its header, names version 0 and a KDF count of at most 24, and carries a sound check value. Encrypted data is whole AES blocks, and the password is checked as [Encryption](#encryption) describes.
- A volume set passes the checks [Volumes](#volumes) lists.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 1,000,000 |
| One header | at most 2 MiB, as the technote allows |
| One name | at most 8,192 bytes |
| All names | at most 64 MiB |
| Dictionary | at most 2 GiB, the decoder budget the 7z reader also uses. RAR 4 uses a 4 MiB window, the RAR 3 maximum |
| PPMd model | at most 256 MiB, the most a RAR 4 block can ask for |
| PPMd models in one archive | 1 GiB of models, plus 64 bytes of model for each byte its compressed files decode, since each new model is filled with zeros |
| KDF count | at most 24, at most two derivations at 24 in one archive, and at most 1,024 derivations in all |
| Volumes | at most 65,536 in a set, and at most 901 with old RAR 4 names. At most three open at once |
| Decoder memory | the window, grown as the stream writes it up to the dictionary, one block of up to 16 MiB and one filter of up to 4 MiB. A RAR 4 window and its largest PPMd model fit the same 2 GiB budget |

Each RAR 5 file's dictionary is charged against a 2 GiB decoder budget, the one the 7z reader uses, before any window is allocated. A file that declares more is refused. An adversarial review built a 768 KiB archive with a 4 GiB dictionary and 1 GiB of zeros, which raised `zipper test` to 1.08 GB of resident memory before the budget. `tests/hostile_rar.rs` builds a 90 KB file that declares a 4 GiB dictionary and decodes to 328 MB. Before the budget `zipper test` extracted it at 541 MB of resident memory, and now the test requires a refusal that raises peak memory by less than 64 MiB. The window grows only as data arrives, so a small file with a large dictionary allocates no more than about twice its size. Growth uses a fallible allocation, so a window the machine cannot hold is an error, not an abort. The reader keeps only the tree that the cpio reader also uses, with each name. On a RAR 5 archive of 300,000 empty stored files whose names total 60 MB, `zipper test` peaked at 189 MB of resident memory, and `7zz t`, which read every RAR before this reader, at 133 MB. On the same files in RAR 4 they peaked at 173 MB and 133 MB. `--threads` has no effect on RAR.

## Corpus written by RAR

`scripts/make_rar_corpus.py` writes 67 archives into `tests/data/rar-written` with RARLAB's `rar` 7.23. RAR 7 no longer writes RAR 4, so RAR 6.24 writes the RAR 4 archives with `-ma4`, and one solid RAR 5 archive. The script builds each source tree, runs `rar`, extracts the archive with `unrar` 7.23 and stops unless `unrar` extracts the source tree. `manifest.json` records the writer, the command, the features, the password of an encrypted archive and either that tree or the reason Zipper gives when it refuses the archive. A volume set counts as one archive. The archives total 3.0 MB.

`tests/rar_written.rs` runs without any RAR tool, so it runs in CI. Every archive in the table below that Zipper reads must extract to its recorded tree. The check covers the bytes, folders, links and Unix modes, and RAR 5 times to the nanosecond. RAR 4 keeps DOS times in local time, so the manifest records no RAR 4 times. Each encrypted archive must extract with its recorded password, and fail with a wrong password, an empty one and none, leaving nothing each time. Each refused archive must fail with its recorded reason and leave nothing. Where `unrar` and 7-Zip are installed, every archive must also extract to the tree `unrar x -ver` extracts, with RAR 4 times compared too, and to the tree `7zz x -snld` extracts where 7-Zip decodes it. `unrar` must extract each refused archive with the password `zipper`, which shows the archive is sound. `tests/common/unrar.rs` finds `unrar` 7.23 or newer, and fails instead of skipping when `ZIPPER_PARITY_REQUIRE_UNRAR` is set. Every gate run outside CI sets it.

| Feature | RAR 5 archives | RAR 4 archives | Zipper |
| --- | ---: | ---: | --- |
| Methods `-m0` to `-m5`, one archive each | 6 | 6 | Extracts |
| Solid, with `-s` | 4 | 1 | Extracts RAR 5. Refuses RAR 4 with `Zipper does not read RAR 4 solid archives` |
| Dictionaries, 1, 4 and 16 MiB for RAR 5 and 64 KiB and 4 MiB for RAR 4, each with a match three quarters of the dictionary back | 3 | 2 | Extracts |
| x86 filter, chosen by `rar` and forced with `-mcE+` | 2 | 2 | Extracts |
| Delta filter, chosen by `rar` and forced with `-mc4D+` | 2 | 1 | Extracts |
| Audio and RGB filters, forced with `-mc4A+` and `-mcC+` and chosen by `rar` | none | 3 | Extracts |
| PPMd, forced with `-mcT+`, on the mixed tree | none | 1 | Extracts |
| PPMd, forced with `-mcT+`, on 120 text files of 200 to 900 bytes | none | 1 | Refuses with `RAR 4 PPMd models restart more often than their output justifies`. `unrar` extracts it |
| Long range and exhaustive search, `-mcx` | 1 | none | Extracts |
| BLAKE2sp checksums, `-htb` | 1 | none | Extracts |
| All three file times, `-tsm -tsc -tsa` | 1 | none | Extracts |
| Owner names, `-ow` | 1 | none | Extracts and ignores them |
| Quick open record, `-qo+` | 1 | none | Extracts and skips it |
| Archive comment, `-z` | 1 | 1 | Extracts and skips it |
| Recovery record, `-rr` | 1 | 1 | Extracts and skips it |
| Hard links, `-oh` | 1 | none | Extracts |
| File versions, `-ver`, with each file added twice | 1 | none | Extracts the old version as `[VER]/1/name` |
| File copies, `-oi` | 1 | none | Refuses with `is a FileCopy, which Zipper does not create` |
| Encrypted headers, `-hp`, stored, compressed, solid with 120 files, and with BLAKE2sp | 4 | 1 | Extracts RAR 5. Refuses RAR 4 with `archive has encrypted headers, which Zipper does not decrypt` |
| Encrypted data, `-p`, stored, compressed, solid, with BLAKE2sp, and with a password of 149 characters | 6 | 1 | Extracts RAR 5. Refuses RAR 4 with `is encrypted, which Zipper does not decrypt` |
| Volumes of a stored file, `-m0 -v24k`, two volumes each | 1 | 1 | Extracts |
| Volumes of the mixed tree, `-v10k`, with `-m3`, solid `-m5`, BLAKE2sp, encrypted data and solid with encrypted headers, four or five volumes each | 5 | 1 | Extracts |
| Volumes with old names, `-m3 -v10k -vn`, `.rar` and `.r00` to `.r02` | none | 1 | Extracts |

The archives for methods, solid `-m3` and `-m5`, search, checksums, times, owners, quick open, comments, recovery records, RAR 4 PPMd and RAR 4 solid hold a mixed tree. It has text, random and zero-filled files, an empty file, an empty folder and a deep folder. It has names in Latin, Japanese and emoji, a 200-byte name and a 600-byte path. It has relative links to a file and to a folder, and files with modes `0o755`, `0o444` and `0o600`. Each file and folder has a fixed time with nanoseconds. The RAR 4 mixed tree spells the emoji name as RAR 4 stores it. The archives `rar5-solid-m5.rar`, `rar5-solid-small-texts.rar`, `rar5-encrypted-solid.rar` and `rar5-encrypted-headers-solid.rar` also extract one selected path from late in their solid stream. So do the solid volume sets `rar5-volumes-solid.part01.rar` and `rar5-volumes-encrypted-headers.part01.rar`.

`tests/rar_written.rs` also damages each volume set in a copy. It must refuse the set with its last volume missing, its last volume cut by 1 byte, its second and third volumes swapped, and its second volume named instead of the first. Each error must name the volume at fault. It swaps in the second volume of another set and requires the error to name that volume and the reason. `tests/hostile_rar.rs` writes RAR 5 and RAR 4 sets with its own writers, which `unrar` must extract to the same files Zipper does. It then breaks one check of [Volumes](#volumes) at a time and requires the refusal that names it.

Plants in each filter Zipper decodes are caught by the corpus test. These are the RAR 4 RGB, audio, delta, E8 and E9 filters and the RAR 5 delta, E8 and E9 filters. Plants in the RAR 5 nanoseconds, the version names and the hard link are caught too. The corpus does not cover these cases:

- RAR 7 writes no ARM filter, since `rar` 7.23 has no switch for it. libarchive's corpus covers it.
- `rar` on macOS writes only the Unix host. libarchive's corpus covers archives written on Windows.
- A dictionary above 4 GiB, which RAR 7 writes in its own format, since `rar` shrinks the dictionary of a smaller file.
- Self-extracting archives and NTFS streams.

## Differences from 7-Zip

The Homebrew build of 7-Zip 26.05 has no RAR codec, so it lists every RAR but extracts only stored entries. Besides the corpus written by RAR, `tests/rar_corpus.rs` checks the archives of libarchive's BSD licensed test suite in `tests/data/rar-corpus`, recorded by `scripts/record_rar_corpus.py`. Each readable archive must extract to the tree `7zz x` extracts where 7-Zip decodes it, and to the tree bsdtar extracts where it does not. Each damaged or unsupported one must be refused. `tests/hostile_rar.rs` cuts the corpus archives at every byte and flips bits in every byte. It builds hostile RAR 5 and RAR 4 headers with its own writers, and reads the archives the adversarial reviews built in `tests/data/rar-hostile`. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip refuses a main header with bytes after its fields. Zipper skips them, as libarchive does.
- 7-Zip extracts a RAR 5 archive cut inside or before its end of archive header. Zipper refuses it. A RAR 4 archive cut between two blocks extracts the entries before the cut, as 7-Zip and libarchive extract it.
- bsdtar 3.7.4 extracts a RAR 5 filter longer than half the dictionary, and a solid file that declares a larger dictionary than its stream. Zipper refuses both, as libarchive's current source does. Zipper extracts every RAR 5 archive in the corpus written by RAR, so none of them holds either.
- 7-Zip reads a RAR 4 DOS time with the offset from UTC in force today. Zipper reads it with the offset in force on that date, as its ZIP and ARJ readers do, so a file written in another daylight saving season differs by an hour. The corpus records no RAR 4 times for that reason.
- An RGB filter with a stride below 4 reads bytes its block has not written yet. Zipper reads them as zeros. libarchive reads whatever an earlier filter left in its filter memory, so after another filter the two decode different files. Each refuses the other's file by its CRC-32. No archive written by RAR was at hand to show which reading RAR uses.
- libarchive refuses `test_read_format_rar_filter_incomplete_block.rar`, which its test calls a filter that is never decoded in full. Zipper extracts it, and the file's CRC-32 matches.

## Review of the RAR 4 filters

On 14 November 2026 an adversarial review attacked the RAR 4 container and each RAR 4 filter on its own. It read only Zipper's code, libarchive and this page, and ran bsdtar 3.7.4 as a black box. Zipper and libarchive know a standard filter only by its program's CRC-32 and length. So the review's archives carry forged programs, with the last 4 bytes chosen to give the CRC-32 and the first byte set to the program's check. bsdtar refuses under-full Huffman tables, so the archives use complete ones. The review built 110 archives. It found one failure, which is fixed, and one difference, which is recorded above.

| Filter or area | Cases tried | How Zipper handled them |
| --- | --- | --- |
| E8 | Blocks of 1, 4, 5, 0x3C000, 0x3C001, 0x40000 and 0x40001 bytes. Calls at the first and last positions, starts after 100 bytes and 258 bytes on, and negative addresses | Blocks of 4 bytes or fewer, or longer than the work memory, are refused. The rest decode as bsdtar decodes them |
| E8E9 | E8 and E9 calls in one block, and a block 258 bytes on | Decoded as bsdtar decodes them |
| Delta | 1 byte, and 3 and 128 channels over 0x1E000 bytes. 0, 129 and 2^32 − 1 channels, no registers, and 0x1E001 bytes | The first set decodes as bsdtar decodes it. The second is refused |
| RGB | Strides from 0 to 3 and equal to the block, byte offsets 0 to 2, 3 bytes and 0x1E000 bytes. A stride past the block, 2 bytes, byte offset 3 and 0x1E001 bytes | The first set decodes as bsdtar decodes it on fresh filter memory. The second is refused. A stride below 4 after another filter differs from libarchive, as above |
| Audio | 1, 2 and 128 channels, and 1 byte. 0 and 129 channels, and 0x1E001 bytes | The first set decodes as bsdtar decodes it. The second is refused |
| Itanium | The Itanium program's length and CRC-32, and other unknown programs | Refused as a program that is not a standard filter. bsdtar refuses it too |
| Block placement | Overlapping blocks, a block one byte past the size, a start number of 2^32 − 1 plus 258, a length register that disagrees, and a stream that ends inside a block | Refused, with nothing published |
| Block placement | Adjacent blocks, an old length reused, empty blocks, a block 5 MiB ahead, a block across the window's wrap, and a program reset after a block | Decoded as bsdtar decodes them, at 11.7 MB of peak memory |
| Window | A match copied from a filtered block, and a filter whose data the window might overwrite before it runs | The window keeps the unfiltered bytes. The decoder runs at most 1 MiB and one match ahead of its output, inside the 4 MiB window |
| Counts | 8,192 pending filters, 1,024 programs, and 0x1FC0 bytes of global data. One more of each | The first set extracts at 6 MB of peak memory. The second is refused |
| PPMd | 4 restarts of 256 MiB in one file, 5 in one file, and 4 in each of 2,000 files | 4 in one file extract and 5 are refused. The 2,000 files extracted before the fix, as the safety checks describe. Now the archive is refused |
| Container | A short header, a second marker, sizes past the file, 1 TiB declared, stored sizes that disagree, a short or long stream, an entry after the end header, and duplicate names | Each is refused, ignored after the end header, or extracted with the last duplicate winning. Nothing is published on a refusal |
| Names and links | Case and Unicode collisions, NUL, Unicode names that decode to `../x` or `/etc`, and links through `d`, `.` and `a/..` | Refused. A link to `d/x` stays inside the destination |

`tests/hostile_rar.rs` builds an archive for each filter with its own writer, which forges programs the same way. Thirteen archives must decode to the CRC-32 of the bytes bsdtar wrote for them. Twenty-three filter archives must be refused for the reason the table gives, as must 8,193 pending filters, an unknown program and PPMd restarts spread over two files. Plants of the filter checks are caught by these tests. They cover the bounds of each filter, the RGB predictor, the E8 file size, the E9 opcode, the audio weights, the 258-byte offset and the filter queue.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options). An entry the selection leaves out is still decoded, since a solid stream needs it. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries.

## Formal checks

Kani proves that the number reader never panics and ends on a byte without the high bit, within 10 bytes. It proves that the general header reader keeps its fields and extra area inside the header. The extra record reader keeps every link target inside its record, and the file header reader keeps the name inside its fields. Every dictionary the compression information gives lies from 128 KiB to 4 GiB. The encryption record reader reads only its own fields, keeps the KDF count at most 24 and reads an IV only from a file record. The block header reader reads only its own 3 to 5 bytes and gives a size below 16 MiB. The proofs are under `[rar-fields]` in `verification/coverage.txt`. Tests under `[rar5-lz]` check the decoder and BLAKE2sp. The Huffman tables decode every code as a bit-at-a-time walk does, and each filter undoes its encoding. A block writer in the tests builds literals, matches, repeats and filters, which must decode as a byte-at-a-time model does, and builds the matches, filters and sizes the decoder must refuse. BLAKE2sp matches Python's `hashlib` on every split. Tests under `[rar5-lz]` also check the key chain against the `pbkdf2` crate, the CBC reader against block-at-a-time encryption under every read split, the 127-character password and the derivation budget. The two corpus tests, `tests/rar_corpus.rs` and `tests/rar_written.rs`, and the hostile tests run under the same section and under `[rar4-lz]`. Kani proves that the RAR 5 main header reader gives a volume number exactly when its flag is set. Kani also proves that the RAR 4 file header reader keeps the name inside the header, and that the RAR 4 end block reader reads its volume number only from inside the block. Tests under `[rar5-lz]` check the volume names, through `name.z99` and past the 65,536-volume limit. The extended time reader gives at most 24 bits of 100 ns units, and a filter record's VM number reader reads no bit past its record. Tests under `[rar4-lz]` build RAR 2.9 streams with fixed-width tables, which must decode as a byte-at-a-time model does or be refused.
