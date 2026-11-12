# In-process ZIP reader

Zipper reads ZIP archives with its own parallel Rust reader.
Zipper picks every reader from the file's leading bytes, and it never runs 7-Zip. A file that starts with a ZIP record signature goes to the ZIP reader. A tar, plain or inside gzip, zstd, xz or bzip2, goes to the [tar reader](tar-reader.md), and any other gzip, bzip2, xz or zstd file goes to the [single compressed file reader](stream-reader.md). A file that starts with the signature of 7z, RAR or CAB goes to that format's reader, which refuses it when it is damaged. A file named `*.001` is read with the volumes after it as one archive, as [split volumes](#split-volumes) describes. A `name.zip` with `name.z01` beside it is read as a [ZIP split set](#zip-split-sets). Every other file goes to the ZIP reader, which rejects anything that is not a ZIP it supports. ISO and UDF disc images go to the [ISO reader](iso-reader.md). A format Zipper does not read fails with `zipper::Refused`, as the [refused formats page](refused.md) lists.

A ZIP the reader rejects is refused, and no other reader tries it. The reader checks entries in order and stops at the first problem. An unsupported entry can therefore hide an unsafe one, and 7-Zip extracts some archives the reader refuses, such as names that collide on the filesystem. When the reader lacks a feature, such as PKWARE strong encryption or another compression method, the error names the feature.
The [usage guide](../usage.md#read-zip-archives-in-process) shows the commands.

## Supported archives

The reader supports these features:

- Stored and Deflate entries, and Deflate64, BZip2, LZMA, Zstandard, XZ and PPMd entries. A PPMd entry must end with the end marker 7-Zip writes. Deflate decodes with Zipper's own decoder in `src/inflate.rs`, which accepts and refuses the streams zlib does. XZ and LZMA decode with Zipper's own decoder in `src/lzma.rs`, the readers [tar](tar-reader.md#supported-archives) uses. Zstandard decodes with Zipper's own decoder in `src/zstandard.rs`, under the [tar reader's window limit](tar-reader.md#limits), and only the entry's first frame is read. An LZMA entry may end with an end marker or without one, and as in liblzma, `lc` plus `lp` above 4 is refused. Every method decodes to exactly its declared size, with its CRC checked and every compressed byte used. BZip2 decodes with Zipper's own decoder in `src/bzip2.rs`, one block at a time on the entry's thread. It accepts the padded code tables that libbzip2 accepts and the `bzip2` crate refused, up to the limit of 4 MiB of compressed bytes in a block. Each BZip2 entry reserves 13.7 MiB of the decoder budget while it decodes, or 17.1 MiB when at most four entries decode at once.
- Shrink (method 1) and Implode (method 6) entries, which PKZIP 1.x wrote. [Legacy methods](#legacy-methods) gives their rules.
- XZ entries decode with Zipper's own xz reader, as [tar's xz](tar-reader.md#supported-archives) does. An entry holds exactly one xz stream, so stream padding or a second stream after it is refused.
- Split sets, `name.z01` and on with `name.zip` last, as [ZIP split sets](#zip-split-sets) describes, and an archive that starts with the spanning signature, as Info-ZIP `zip -s` writes one that fits in one piece.
- Self-extracting archives, a stub followed by a ZIP, whether `zip -A` adjusted the offsets or not, as [self-extracting ZIP](#self-extracting-zip) describes.
- Conventional ZIP64. Info-ZIP also writes a ZIP64 end record when its input is piped, even when the regular end record holds every value. The reader then requires both records to agree. With ZIP64, every end record value that is not a sentinel must equal its ZIP64 value.
- File bytes and directories. A file with the FIFO type, which is how Info-ZIP `zip - -` stores piped input, is written as a regular file, as `unzip`, bsdtar, 7-Zip and Python write it. Device and socket entries are rejected.
- Symlinks that stay inside the destination, on Unix. See [Symlinks](#symlinks).
- Names stored as UTF-8, and other names decoded as code page 437. A name that is valid UTF-8 is read as UTF-8 even without the UTF-8 flag, because Info-ZIP and `ditto` store UTF-8 names without it. A name with the UTF-8 flag that is not valid UTF-8 is rejected, because Python and 7-Zip do not decode it as CP437.
- Entries encrypted with ZipCrypto or WinZip AES, with the password. See [Encrypted entries](#encrypted-entries).

The `zip` feature builds every method and both encryption schemes. A library build can name `zip-deflate` and only the method features it needs, as [Build only the formats you read](../usage.md#build-only-the-formats-you-read) lists. The reader then refuses an archive with an entry whose method or encryption the build leaves out. The error is `zipper::NotBuilt` naming the feature to enable, such as `zip-bzip2`, `zip-encryption` or `zstd`. The check runs on every central-directory entry before anything is written, so the refusal holds even when that entry is not selected, and nothing is published. `tests/zip_method_features.rs` checks each method and both schemes with and without their feature.

The reader does not support these features:

- PKWARE strong encryption and encrypted central directories.
- Symlinks that point outside the destination.
- Reduce (methods 2 to 5), Tokenize and other legacy methods. Neither Info-ZIP `unzip` on macOS, which is built without Reduce, nor 7-Zip reads Reduce, so no oracle can check a decoder.
- The last volume of a split set without the volumes before it.
- Ambiguous end records.
- A ZIP64 locator just before the end record that the end record does not use. bsdtar, 7-Zip and Python follow such a locator and can read a different directory.
- Data before the first record that the archive's offsets do not account for, or that holds another archive's records. See [self-extracting ZIP](#self-extracting-zip).

It restores Unix permission bits the way bsdtar does:

- Each file and directory gets its archive mode, masked to `0o777`, so setuid, setgid and sticky bits are never set. Archives made on DOS or Windows give files `0o664`, directories `0o775`, and read-only entries no write bits.
- The user's umask limits every mode, as it does for a new file. Info-ZIP `unzip` ignores the umask, so it can restore wider modes such as `666` or `777`.
- Directory modes are applied after all files are written, deepest first, so a read-only directory can still receive its children. A directory entry listed after its own contents still gets its mode, which bsdtar and `unzip` skip.

It restores modification times the way Info-ZIP `unzip` and bsdtar do, and reads the NTFS field as 7-Zip does:

- The extended timestamp field (`0x5455`) gives an exact UTC time. Info-ZIP and bsdtar write it.
- Otherwise the older Info-ZIP Unix field (`0x5855`), which `ditto` writes, gives an exact time.
- Otherwise the NTFS field (`0x000a`), which 7-Zip and other Windows tools write, gives an exact UTC time. `unzip` and bsdtar ignore it and use the DOS time, rounded to an even second.
- Otherwise the DOS date and time are read in the local time zone, with 2-second resolution. On Windows the local time zone's own rules for that date apply.
- A 32-bit exact time with its top bit set is read as signed or unsigned, whichever lies nearer the DOS time, so a 1969 file is not dated 2106.
- Invalid DOS fields, such as month 13, are normalized as bsdtar does. Setting a time is best effort.
- Directory times are set after all files are written.

On Windows it restores no permissions. It restores file and directory times as on Unix.

It never restores owners, access times, extended attributes, ACLs, file flags or macOS metadata.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 100,000 |
| Raw central-directory metadata | at most 64 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Name normalization

The reader writes each name with its stored bytes, as Info-ZIP `unzip`, Python and 7-Zip do. bsdtar on macOS rewrites names to Unicode NFD instead. APFS and NTFS ignore case, and APFS also ignores normalization, so two entries can name one file there:

- Two files that collide fail the extraction with an error naming the entry, and nothing is published.
- Two directories that collide merge into one, as in `unzip` and Python. The merged directory takes the spelling, mode and time of the first entry in the central directory.

## Symlinks

The reader is stricter with symlinks than Info-ZIP `unzip` and bsdtar, which create links to any target. A symlink entry is created only when all of these hold:

- The target is a relative UTF-8 path of at most 4,096 bytes, with no NUL byte and, on Windows, no backslash or drive letter.
- Every `..` in the target comes before its first name, as in `../../shared/lib`.
- Counted from the link's own directory, those `..` steps stay inside the destination.
- No other entry uses the link as a directory.

The `..` rule makes the check independent of how the filesystem compares names. A target such as `a/L/../x` climbs out of a link `a/L`, and a lexical check misses it when the filesystem matches `a/l` to `a/L` by ignoring case or Unicode normalization, as APFS does. A target that only descends after its `..` steps stays below the directory it reached, because every link it passes through obeys the same rule.

Links are created after every file is written, so no file is written through a link. An archive with any other symlink is rejected. On Windows, creating a symlink needs Developer Mode or administrator rights. Without them the archive is refused before anything is published. A link to a folder in the destination becomes a folder link, and a target with `\` or `:` is refused, because it could name a drive or a share. A target part is refused as an entry name is when it ends in a dot or a space or names a device such as `CON`. So is one that holds `<`, `>`, `"`, `|`, `?`, `*` or a character below U+0020. A link whose target is another link not yet created becomes a file link, so such a chain that ends at a folder does not open as a folder on Windows. Junctions are never created.

## Encrypted entries

The reader decrypts ZipCrypto and WinZip AES entries in process when it has the password. Pass `--password`, or set `password` in `ExtractOptions` or `native_zip::ReadOptions`. When a selected entry is encrypted and no password was given, the reader fails with `native_zip::PasswordNeeded`. It checks the destination first, so a destination it cannot use fails with that error instead, and it writes no entry before it fails. On Unix, when standard input is a terminal, the command line then asks for the password and tries again. Otherwise it refuses. `list` needs no password and refuses one, as `duplicates` does. `--password` with an archive Zipper does not decrypt, such as a tar archive, fails instead of being ignored. 7z archives decrypt in process, as the [7z reader](7z-reader.md) describes. An encrypted ZIP that is split into volumes or has a self-extractor in front is refused. The reader does not read those layouts.

ZipCrypto is the traditional PKWARE cipher from APPNOTE section 6.1. It is weak. A known-plaintext attack recovers its keys, so it protects nothing against a determined attacker. Zipper reads it only so that old archives open. Each entry starts with a 12-byte header whose last byte, once decrypted, must match a check byte. Without a data descriptor the check byte is the high byte of the CRC. With one it is the high byte of the DOS time, as Info-ZIP writes. A wrong password passes the check byte 1 time in 256, so the CRC of the decoded data is the real test.

WinZip AES uses AES-128, AES-192 or AES-256 in CTR mode with a little-endian counter that starts at 1. The keys come from PBKDF2 with HMAC-SHA1 and 1,000 iterations over the password and the entry's salt. A 2-byte verifier rejects most wrong passwords before any data is read. An HMAC-SHA1 code of 10 bytes covers every encrypted byte, so a wrong password that passes the verifier, 1 time in 65,536, fails there. AE-1 entries also keep the CRC, which the reader checks. AE-2 entries, which 7-Zip writes, store a zero CRC and rely on the code alone.

The reader refuses these encrypted entries:

- An AES extra field (`0x9901`) that appears twice or is not 7 bytes long.
- An AES field with a vendor version other than 1 or 2, a vendor other than `AE`, or a strength other than 1, 2 or 3.
- Method 99 without the AES field or without the encryption flag.
- A local AES field that differs from the central one, or a local encryption flag that differs from the central one.
- An entry too short to hold its salt, verifier and code, or its 12-byte ZipCrypto header.
- PKWARE strong encryption, flag bit 6. 7-Zip reads it, and Zipper does not.

7-Zip 26.05 extracts an entry whose AES field is longer than 7 bytes, has vendor version 3, appears twice, or is missing or different in the local header. Zipper refuses these, because readers that trust different copies of the field can decrypt one entry in different ways.

A wrong password fails the extraction before anything is published. When the ZipCrypto check byte or the AES verifier fails, the error says the password is wrong. When either passes but the data does not decode, its CRC differs or the AES code does not match, the error says the password is wrong or the entry is damaged. Decrypted bytes can reach the private staging folder before the CRC or the authentication code is checked, but the staging folder is removed and the destination keeps its contents.

The library keeps no copy of the password. The keys live in one heap allocation per entry, and these are zeroed when the entry ends:

- The ZipCrypto keys.
- The PBKDF2 output.
- The AES key schedule and the HMAC-SHA1 state.

Building and using them leaves copies on the worker's stack, so after key setup and again after each encrypted entry the reader overwrites 64 KiB of the stack below its own frame. Core dumps were taken while the rest of the archive was still extracting, on both engines and at 1 to 8 threads. They held none of the AES-256 round keys, the HMAC inner and outer states, the HMAC key or the PBKDF2 output. That is a check of this build on macOS, not a guarantee. A compiler that inlines differently or spills to another place could leave a copy the overwrite misses.

The command line holds the password in a buffer that is zeroed on exit. A typed password is read from the terminal one byte at a time, into a buffer that never grows. A password given with `--password` stays in the process arguments, where other users can see it and Zipper cannot clear it, and argument parsing may leave copies in freed memory. Errors and progress reports never contain the password, and `--password` takes values that start with a hyphen, so they never reach a usage error.

The header fields of an entry are not covered by the AES code. Suppose someone changes the method of an AE-2 entry to one whose output has the same size, such as stored instead of Deflate when the two sizes match. The entry then decodes without error to the wrong bytes, as in 7-Zip. AE-1 and ZipCrypto entries catch it with the CRC.

`tests/encrypted_zip.rs` extracts archives from 7-Zip and Info-ZIP with each scheme and several methods, and compares them with the tree `7zz x` gives. It flips every byte of an encrypted entry, the CRC and the code, and checks that each run fails with the destination unchanged and no staging folder left.

## Legacy methods

`src/shrink.rs` decodes Shrink and `src/implode.rs` decodes Implode, as APPNOTE 5.1 and 5.3 define them. Each holds a fixed table. Shrink keeps 8,192 entries and Implode an 8 KiB window, so neither takes decoder memory from the shared budget. A Shrink partial clear frees only the entries that are leaves, from a list kept as entries are added, so a stream of clears costs no more than the codes that built the table. Each stops at the entry's declared size, and the CRC and staging rules are the same as for other methods.

A Shrink stream follows 7-Zip's rules where Info-ZIP `unzip` and 7-Zip differ:

- Each code after the first takes the lowest free entry before it is expanded. A partial clear can free the code read just before it, so that entry can become its own prefix. The entry is accepted until a code names it, and then the stream is refused, as both tools refuse it.
- Zipper refuses a code that names a freed entry, a prefix that leads to one and a code once the table is full. `unzip` decodes the first two to wrong bytes.
- Zipper refuses a control code before the first byte, a control code other than 1 and 2, and a widening past 13 bits. `unzip` ignores unknown control codes.
- A partial clear frees every leaf of the table. `unzip` frees only the leaves below the last entry it filled since the previous clear. After a clear on a table that is not full, it can decode other bytes or refuse. Even when every clear finds the table full, an entry reused after a clear can survive above that point, and `unzip` then refuses or decodes other bytes. Zipper reads such streams as 7-Zip does. No real PKZIP archive in that state has been found to settle which tool PKZIP agrees with.
- After the declared size, only control codes may follow, then zero bits up to the next byte. 7-Zip also accepts one more code there, which `unzip` decodes as data, so Zipper refuses it.

An Implode entry reads its two or three Shannon-Fano trees, as general purpose bits 1 and 2 select. A tree must describe exactly its 256 or 64 symbols with a complete code. A match may reach before the start of the entry and reads zeros there. A match that runs past the declared size and a byte left after the last symbol are refused, as 7-Zip refuses them.

`tests/zip_legacy.rs` builds Shrink and Implode streams with the test encoders in `tests/common/legacy_encoder.rs` and checks each archive against `unzip` and 7-Zip 26 or newer before Zipper reads it. Random Shrink code streams with clears, widenings and entries that are their own prefix must decode or fail where 7-Zip does. Random Implode trees and matches must extract as both tools extract them. Three archives from the `zip` crate's tests, in `tests/data/zip-legacy`, must extract as both tools extract them. Kani proves the Shrink table and the Implode tree builder never panic, under `[zip-legacy-methods]` in `verification/coverage.txt`.

## Integrity checks

Both native ZIP modes run these checks before publishing any output:

- Decoded lengths and CRCs match the central directory.
- Each local header's name, method, CRC and sizes match its central-directory entry. For Implode, so do the general purpose bits that choose the window and the tree count, which `unzip` and 7-Zip read from the local header. With a data descriptor, a local CRC or size may be zero or `0xFFFFFFFF` instead.
- Local records tile the archive. The first starts at offset 0, and each ends exactly where the next record or the central directory starts.
- With a data descriptor, the bytes after the data are exactly one descriptor of 12, 16, 20 or 24 bytes, and its CRC and sizes match the central directory.
- The local encryption flag matches the central one, and any local ZIP64 sizes match the central directory.
- No local or central record has two ZIP64 extra fields. Readers disagree on which one wins.
- Each Deflate stream ends exactly at its compressed size, so no bytes can hide after it.
- A stored entry followed by a data descriptor does not contain a descriptor whose CRC and sizes match the bytes before it. A streaming reader would end the entry there. Descriptors that describe other data, as in a nested archive, are allowed.
- Names are unique and safe, and no file is also used as a directory.
- A Unicode path extra field (`0x7075`), local or central, names the entry as its stored name does. The `zip` crate renames an entry from the central field and some streaming readers use the local one, so a different name is rejected. The field may spell a CP437 stored name in UTF-8 when its CRC matches the stored bytes. Readers that honor the field and readers that decode CP437 then reach the same name. bsdtar and 7-Zip use neither on some archives, and they already disagree on CP437 names without a field. A field whose content is not UTF-8 is rejected. A field with a stale CRC must repeat the stored bytes, because readers that check the CRC ignore it.

`list` runs only the central-directory checks, including the central Unicode path and ZIP64 field checks, and the first-record check. It does not read local headers, so it does not detect bytes hidden between records or a Unicode path field present only in a local header. `test` and `extract` detect both. `extract --path` checks the local header and data descriptor of every entry, but decodes and checks the payload of selected entries only.

## Reference corpus

`tests/data/zip-corpus` holds 26 archives. Twenty-two come from these real ZIP writers:

- Info-ZIP `zip` 3.0, in default, stored, ZIP64, data-descriptor and piped modes, and with piped input.
- Info-ZIP `zip -s 64k`, as a split set of six volumes and as one piece.
- Info-ZIP `zip` after a 4 KiB stub, with and without `zip -A`.
- 7-Zip, with Deflate, Copy and timestamps.
- bsdtar, with Deflate and stored entries.
- macOS `ditto`.
- Python `zipfile`, seekable and streaming, with Deflate, stored and ZIP64.

Four more archives come from Info-ZIP `zip -y`, 7-Zip `-snl`, bsdtar and `ditto`, which store symlinks as links. Only `unzip` and bsdtar create links, so link targets are recorded where those two agree, and the other paths still need three readers.

The manifest also records each path's exact modification time where `unzip` and bsdtar both restore the source's own time under `TZ=UTC` and `TZ=Pacific/Auckland`. That covers the 14 Info-ZIP, bsdtar and `ditto` archives that the two can read. The other archives carry only DOS times, which depend on the time zone. `tests/cli.rs` checks those under `TZ=UTC` and `TZ=Asia/Tokyo`.
 The source tree has empty files and directories, non-ASCII names, and files at the decoder's 64 KiB and 192 KiB slice boundaries.

`scripts/make_zip_corpus.py` extracts each archive with Python `zipfile`, Info-ZIP `unzip` 6.0, bsdtar and 7-Zip, and records in `manifest.tsv` the tree that at least three of them produce identically. `corpus.json` names any reader that disagreed. Python decodes the non-ASCII names in the Info-ZIP and `ditto` archives as CP437, because those writers store UTF-8 names without the UTF-8 flag, so it dissents on those 9 archives. No reference reader but 7-Zip reads a split set, and Info-ZIP's `zip -s 0` join wrote a truncated archive on macOS. So the manifest of the split set records the tree 7-Zip extracts only where it equals the source tree.

`tests/zip_corpus.rs` requires the reader to reproduce every manifest exactly, on every operating system in CI. On Unix it also compares permission bits where `unzip` and bsdtar agree, which they do on 24 of the 26 archives. They disagree on the mode of Info-ZIP's piped input, and neither reads the split set. The modes were recorded under umask `022`, and the test masks them with the current umask. Python restores no modes and 7-Zip skips directory modes, so neither votes on modes. Regenerate the corpus on macOS with `python3 -I scripts/make_zip_corpus.py`. Add `--only` and archive names to rebuild those alone and keep the rest. The stub is generated bytes that start with `MZ` and hold no ZIP or 7z signature, because a real stub such as `unzipsfx` may not be redistributed. The readers open a self-extractor under an `.exe` name, because 7-Zip opens a file named `.zip` only from its first byte. There `unzip` exit status 1, a warning about the stub, counts as success. Java `jar` output is missing because no JDK was installed.

The [real-world comparison](../../benchmarks/realworld/README.md) also extracted 94 ZIP archives from PyPI, Maven, Go and GitHub with Zipper and default bsdtar. All matched, along with the 154 tar archives in the same set.

## Central directory

The reader reads the end record, the ZIP64 end record and locator and the central directory with its own code in `src/zip_directory.rs`, and the release build has no `zip` crate. A differential test lists every ZIP in the corpus, the parity set and `tests/data/encrypted-zip` with this code and with the `zip` crate. Both must give the same entries, or both must refuse, or one of Zipper's own checks that the `zip` crate lacks must refuse. The names, sizes and folders that `native_zip::list` gives, which `zipper list` prints, must match the `zip` crate's too. The test repeats this on 300 damaged copies of each archive. The corpus split set must list as the `zip` crate lists the same tree in one piece. An empty archive whose ZIP64 end record is read without a sentinel in the end record is refused, as the old reader and 7-Zip refuse it.

`every_real_world_zip_lists_as_the_zip_crate_lists_it_or_both_refuse` lists the [real-world set](../../benchmarks/realworld/README.md) the same way. It is ignored by default, because `scripts/realworld_diff.py fetch` downloads the set. Run it with `cargo test --release --lib every_real_world_zip -- --ignored`. Set `ZIPPER_REALWORLD_ARCHIVES` to the folder when it is not `target/realworld/archives`. On 8 November 2026 all 95 ZIPs there listed the same.

## Mutation testing

`tests/zip_fuzz.rs` mutates corpus archives with seeded bit flips, truncations, insertions and edge values written into header fields. Both engines extract every mutant. Each case must either fail and leave nothing in the destination's parent, or succeed with verification agreeing and exactly the declared bytes written. A panic fails the test.

The gate runs 1,000 cases. To run more, or to replay a reported failure:

```sh
ZIPPER_FUZZ_CASES=300000 cargo test --release --test zip_fuzz
ZIPPER_FUZZ_SEED=303 ZIPPER_FUZZ_CASES=1 cargo test --release --test zip_fuzz
```

A run of 300,000 cases on 2026-11-01 at this revision found no failure. In a 20,000-case sample, 3,588 of 40,000 extractions accepted their mutant, mostly through changed timestamps, attributes or comments. The rest were rejected. This is random mutation, not coverage-guided fuzzing. A coverage-guided `cargo fuzz` target needs a nightly toolchain and is not set up.

## Split volumes

Every reader reads an archive split into raw numbered volumes, such as `7z a -v1m` writes. The file named `name.001` opens with `name.002` and each next number as one stream. The reader picks itself from the joined stream's first bytes, so `a.7z.001`, `a.zip.001`, `a.tar.001`, `a.tar.gz.001` and `a.txt.xz.001` all extract in process. A single compressed file is named from the name without `.001`. Volume numbers have three digits and grow past 999 as 7-Zip writes them, so `name.1000` follows `name.999`.

`src/volumes.rs` finds the set when a `zipper extract`, `list` or `test` run starts. The calling thread keeps it for that run only, and passes it to every reader handle and LZMA2 decoding thread. Two runs never share a set, so a set replaced between runs is found again. Finding it reads each volume's metadata once and lists no folder. Each handle keeps at most one volume open. It opens a volume without blocking, so a volume replaced by a FIFO fails instead of hanging. It then checks that the volume is still a regular file with the size, modification time and identity found at the start. On Unix the identity also holds the device and inode, and on Windows it is the size and modification time alone. The change time is left out, because indexing, quarantine and backup tools change it on files they never write. A file rewritten in place at the same length with its modification time put back is not caught, as no check on the file's metadata can catch it. Every file the readers open through `src/volumes.rs`, a volume or a file read alone, opens without blocking and must be a regular file.

`name.002`, or any volume after it, opened by name is read alone, as 7-Zip reads it. When that read fails, the error says that the file is a volume of a split archive and names `name.001`. `name.000` is not a volume.

`name.001` itself must be a regular file. A missing or dangling `name.001`, or one that is a FIFO or device, fails with its full path before anything else is read.

The volumes form a set only when they are consistent. Every volume but the last must match the first one's size. The last may be shorter, or larger by fewer bytes than the number of volumes, which is how BSD `split -n` spreads a remainder. 7-Zip reads both. GNU `split -n` gives the remainder to the first volumes instead, so a middle volume is one byte short of the first. Such a set is refused as damaged. Join its volumes with `cat` and open the joined file. `name.001` is read alone, as an archive that is not split, in two cases only. Either no `name.002` exists, or `name.002` is the only other volume and is larger than `name.001` by two bytes or more, which suggests an unrelated file. So a two-volume set whose `name.002` is larger by exactly one byte is joined, as BSD `split -n 2` writes it. Every other inconsistency means the set is damaged. Then `name.001` is neither joined nor read alone, and the error leads with the volume at fault, named by its full path when the problem is the file itself:

- A gap in the numbering. Zipper looks for the 16 numbers after the first missing one. It asks the filesystem for each name, so a name that differs only in letter case or Unicode form counts where the filesystem treats it as the same name.
- A size 7-Zip and `split` never write. That is a volume larger than the first when more follow, or a last volume larger than the first by the number of volumes or more.
- An empty volume, or a volume shorter than the first that another volume follows.
- A volume that is not a file, or whose metadata cannot be read.

A consistent set is read as one archive, even when it is a false join, such as a complete ZIP or tar named `name.001` beside an unrelated, smaller `name.002`. That read fails, as 7-Zip's does, and the error says to rename a whole archive named `name.001` or open it by another name. Zipper never falls back to reading `name.001` alone. A first volume can be a whole archive by itself, such as a gzip, zstd or xz file of several members that splits at a member boundary. A fallback would then turn a damaged or missing volume, or a size refusal, into a truncated success. A set whose joined read succeeds is kept, so `log.gz.001` and an unrelated, smaller gzip `log.gz.002` extract as one gzip of two members, as `7zz x` does.

A single compressed file, such as gzip, bzip2, xz, zstd, LZ4 or `compress`, has no overall end or length. When its members end exactly at a volume boundary, missing trailing volumes leave a stream that decodes cleanly. So a set read by the single compressed file reader is refused unless its last volume is smaller than the first. A last volume as large as the first may hide a missing next volume. A last volume larger than the first may be an unrelated file, such as a whole gzip beside another one byte larger, because a stream that ends cleanly cannot show where the first file stopped. The `split -n` allowance for a larger last volume therefore applies only to ZIP, 7z and tar, which end with their own records. One case stays undetectable. A gzip, xz, zstd or LZ4 file of several members can lose the end of its last volume exactly at a member boundary. The volumes left still have a smaller last volume and decode cleanly, so the members present are extracted. Main reads the same bytes joined into one file the same way. A `name.001` that would be read alone is refused for the same reason, since it is a set whose last volume is full. A genuine set whose last volume happens to be full is refused too. To read it, join the volumes into one file, such as with `cat name.001 name.002 > name`, and open that file, or rename a lone `name.001` to a name without `.001`. ZIP, 7z and tar, plain or compressed, end with their own records, so their sets are not refused this way.

A `compress` (`.Z`) stream and an LZ4 legacy frame, as `lz4 -l` writes, have no checksum and no end mark. A truncated last volume still decodes to a shorter file without any error. So the single compressed file reader refuses a split set in either format, with the same advice to join the volumes with `cat`. This refusal applies to that reader only. A `.Z` set that holds a tar goes to the tar reader, which relies on the tar end-of-archive marker to catch truncation. As with a single `.Z` file, a corrupted byte is not detected, because `compress` has no checksum. The LZ4 reader refuses legacy frames in any case.

Inside a consistent set, a volume that changes while it is read fails. So does a 7z set that holds bytes past the end of its header, which catches an unrelated volume after a full last volume. A single 7z named `name.001` keeps 7-Zip's tolerance for trailing bytes. A file that a 7z reader opens again for another folder or decoding thread goes through the same non-blocking open, and it must keep the identity it had when it was first opened.

The readers' own checks catch the rest. Every failure while reading a joined set or a numbered file read alone gains one informational line after the reader's own message, such as "read as split volumes `a.zip.001` to `a.zip.014`". A damaged set is the exception, because nothing is read from it. Its error already leads with the volume at fault, so it gains no line. Damage can cause almost any error, including an unsafe path, a method, encryption, a collision or a selection that matches nothing, so those get the line too. Only errors whose type shows they have nothing to do with the volumes leave it out:

- a cancel;
- a destination that exists, is not a folder, or where the staging folder cannot be created;
- the `--max-extracted-bytes` cap or the free-space refusal;
- `zipper::NotBuilt`, for a method or encryption the build leaves out, so a library can still find it in the error.

The line never claims a volume is damaged. For a set, it names the first and last volume and says to rename a whole first volume. It adds that a damaged archive may have a missing next volume, a truncated last volume, or a volume that is repeated or out of order. A volume that holds the same bytes as an earlier one is named. That check hashes each volume once, compares a volume only with the first of its length and hash, and stops after reading 256 MiB. For a file read alone, the line says why. A volume that cannot be opened is named by its path.

7z, ZIP and compressed tar check their data, so a repeated or swapped volume fails. A plain tar has no checksum over file data, so a volume repeated or swapped inside one file's data extracts wrong bytes. 7-Zip reads such a set the same way. When the volumes fall on entry boundaries, a repeated volume can even extract a valid tar with an entry missing. The repeat scan cannot catch this after a successful read, because each reader publishes the extraction before it returns, so the scan runs only after a failure.

A library caller that calls one reader, such as `native_7z::read_archive`, on a `name.001` path gets the same reading, but finds the set again for each file it opens. Wrapping the call in `zipper::read_volumes` finds the set once and adds the volume line to a failure, as `extract_archive` does.

`tests/volumes.rs` splits every native format at sizes from 1 byte up past the whole archive and compares each tree with the whole archive's. It also extracts 7-Zip's own volumes of 7z, ZIP, tar, gzip tar and xz tar, from 1-byte to single volumes, and compares each tree with `7zz x`. Its hostile cases cover each failure above, and check that no staging folder is left. Two Kani proofs in `src/volumes.rs` cover the offset arithmetic. `a_layout_exists_exactly_when_its_total_fits` shows that a set is accepted exactly when its total size fits 64 bits, for any volume size, count and last size. `every_offset_maps_to_the_one_volume_and_position_that_hold_it` shows that every offset maps to the one volume and position that hold it, with no panic or overflow, for volume sizes and counts up to 255 and offsets up to 65,535. A wider bound ran for over 30 minutes without a verdict. `every_layout_locates_every_offset_as_a_linear_scan_does` compares the mapping with a linear scan on every small set.

## Self-extracting ZIP

A self-extracting ZIP is an executable stub followed by a ZIP. Info-ZIP `zip -A` adjusts every offset to count the stub. A plain `cat stub a.zip` leaves them counting from the ZIP. The reader accepts both without knowing the stub, by the rule Python `zipfile` and libarchive use. The central directory must end where the end record or ZIP64 end record starts. When it ends earlier, the gap is the number of bytes before the archive, and every offset moves by it. With ZIP64, the locator must point at the ZIP64 end record after the same move.

The bytes before the first record are then read and refused when the offsets cannot reconcile them, or when another reader may see a different archive:

- The offsets move by some bytes, but the first record starts past that point, so the offsets count only part of the prefix.
- The archive has no entries.
- The prefix is not the 4-byte spanning signature, and fails the stub check below. bsdtar and 7-Zip read a cpio, ar, ISO or xar archive at the start of a file, or a CAB or RAR inside an executable, instead of the ZIP after it. 7-Zip also searches a prefix for a ZIP local header or end record and opens the first archive it finds there. Python and bsdtar read the archive the end record names.

The stub check is shared by the ZIP and 7z readers. Its main rule allows by position, and a deny scan backs it up:

1. The stub must be a whole executable, and every file range its headers name must end at or before the archive's first byte. The furthest of them must end exactly there. 7-Zip opens whatever format starts an executable's overlay, the bytes after its own data. When it finds none there, it searches the file for any signature it knows, which includes UEFI firmware volumes, LHA, ARJ and formats no list can keep up with. 7-Zip also counts more of the file as the executable than its sections, such as a certificate table or debug data that a header names. So the check does not follow 7-Zip's rules one by one. It walks every range any header names and refuses the stub when one of them reaches into the archive or lies past its start.
   - A PE stub starts with `MZ`. The 32-bit pointer at offset `0x3C` must name an offset inside the stub that holds `PE\0\0`. The file header must declare 1 to 96 sections and an optional header with the PE32 or PE32+ magic number. The ranges are the section table, every section's raw data, every data directory and the raw data of every debug directory entry. The certificate table names a file offset. The other directories name addresses in memory, which are mapped to the file through the sections, or read as file offsets when no section holds them. Section raw data that overlaps another section's, or a section table that does not fit the stub, is refused.
   - An ELF stub starts with `\x7FELF`, a class and byte order of 1 or 2, and version 1. The ranges are the header, the program and section header tables, every program header of any type, `PT_NULL` included, and every section other than `NOBITS`.
   - A thin Mach-O stub starts with one of its four magic numbers. The ranges are the load commands and every range a load command names. These are `LC_SEGMENT` and `LC_SEGMENT_64` and their sections other than zero-fill ones, the `LC_SYMTAB` symbols and strings, and the `LC_DYSYMTAB` tables. They also include `LC_CODE_SIGNATURE` and the other link-edit data commands, `LC_DYLD_INFO`, `LC_ENCRYPTION_INFO`, `LC_TWOLEVEL_HINTS` and `LC_NOTE`. Other load commands name no file range and are skipped. A fat Mach-O stub starts with `\xCA\xFE\xBA\xBE` or `\xCA\xFE\xBA\xBF` and declares 1 to 30 architectures, which leaves out Java class files. Its ranges are its header and its slices. The headers inside a slice are not read.
   - A script starts with `#!` and has no overlay, so for a script the deny scan alone applies.
   - Ranges of size 0 name no bytes and are skipped.
   - Every table must fit its own record. A segment's section headers must fit inside its load command's `cmdsize`, and every load command inside the declared size of the load commands. ELF entry sizes must be the standard 32 and 40 bytes, or 56 and 64 bytes. A PE debug directory must hold a whole number of 28-byte entries. Anything else is refused as a header that is not whole.
   - Each range is checked as it is read, and the walk stops at the first one that reaches the archive. At most 65,536 ranges are walked, and a stub whose headers name more is refused. Real stubs name far fewer. The fat `/bin/ls`, `/usr/bin/true` and `unzipsfx` name 3, the thin arm64e `ls` names 21, a 32-bit Windows PE 13 and a .NET PE 7.
   - The ZIP's first record, or its spanning signature, must start where the furthest range ends. For a 7z, its signature must.
2. The whole stub must then hold no other format 7-Zip or bsdtar opens. That is a ZIP local header, central record, end record, ZIP64 end record or ZIP64 locator, starting anywhere in the stub. It is also a 7z signature, a CAB `MSCF` header, a RAR 4 or RAR 5 signature, or an NSIS installer signature. It is also an ISO 9660 or UDF volume descriptor one byte into any of sectors 16 to 31. Its identifier is `CD001`, `BEA01`, `NSR02`, `NSR03`, `TEA01` or `BOOT2`. Before a 7z, ZIP records in the stub are left to the overlay rule, since Python and bsdtar look for a ZIP only at the end of the file, which the 7z reader checks itself.

Any bytes between the executable's data and the archive are refused, even one, because 7-Zip reads a tar, cpio, ar, xar, ISO, UEFI or other archive placed there. Info-ZIP `zip -A` and `cat stub a.zip` put the archive right at the overlay. The real self-extractors tried put it there too. These were Info-ZIP's `unzipsfx`, `/usr/bin/true`, `/bin/ls` and two Windows PE files. An ISO whose system area starts with `MZ` fails the first rule, because it has no PE header. A PE stub that carries a CAB or RAR fails the second, because 7-Zip and bsdtar open the CAB or RAR inside it. An executable that holds one of these signatures, such as Info-ZIP `zip` or 7-Zip itself, is refused as a stub. The error names the header range that reaches the archive, the offset where the executable's data ends, or the offset of the signature at fault. Otherwise it says that the header is not whole. A signed PE whose certificate table ends where the archive starts passes, and 7-Zip reads that archive too. A certificate table after the archive, at the end of the file, leaves the ZIP end record short of the end, so it is refused.

The error names the offset at fault. Encrypted entries stay refused. Stubs are not limited in size, and the prefix is read once, 64 KiB at a time. A split set may hold such a prefix in its first volume, but its offsets may not move. An end record that leaves its values to ZIP64 records with no sentinel, as Info-ZIP's piped output does, is read only without a moved directory.

`tests/self_extracting.rs` puts PE stubs of 128 bytes to 70,000 bytes before eight corpus and parity archives, with offsets adjusted and not, and requires the tree the archive gives alone. Where installed, 7-Zip must extract the same tree and Python `zipfile` must list the same names. It also runs `zip -A` itself. Its hostile cases put a whole ZIP, a stub and a ZIP, an empty ZIP and a 7z before the archive, adjust offsets by part of the stub, and append an empty or encrypted ZIP. Others put every denied signature and volume descriptor in a PE stub, or use a stub whose header is not whole, such as `MZ` over an ISO's empty system area. Others put one byte, 512 bytes, a tar, cpio, LHA, ARJ, xar, UEFI or ISO header between a PE, ELF, thin or fat Mach-O stub and the archive. Each must fail and publish nothing. A self-extracting ZIP that stores a 7z extracts as the ZIP, as 7-Zip and Python read it. On Unix, `/usr/bin/true` followed by a ZIP or a 7z extracts, and refuses with one byte between. A PE certificate table or debug entry, an ELF `PT_NULL` segment, or Mach-O symbols that reach the archive are refused by name. The same ranges ending inside the stub pass. `tests/cli.rs` checks that a ZIP after a shell script extracts in process without reaching 7-Zip.

## ZIP split sets

Info-ZIP `zip -s` and WinZip split an archive into `name.z01`, `name.z02` and on, with `name.zip` last. The end record in `name.zip` names its own disk, and every offset counts from the start of the volume that holds it. Open `name.zip` to read the set in process.

`src/volumes.rs` finds the set when `name.zip`, in any letter case, has `name.z01` beside it. Only the ZIP reader joins it. Every other reader, and the sniffing that picks a reader, sees `name.zip` alone, so a `name.z01` crafted to make the joined bytes look like a 7z or a tar is never read as one. A `name.zip` with `name.z01` beside it always goes to the ZIP reader, because its first bytes may fall inside an entry. When the end record of `name.zip` names only one disk, the error says that `name.zip` looks whole and that the volumes beside it may not belong. When any read of the set fails and `name.zip` by itself is a 7z, tar or compressed file, the error leads with that. Either way it says to move the volumes aside to read `name.zip` alone. A `name.ZIP` looks for `name.Z01`. Numbers have two digits and grow past 99, so `name.z100` follows `name.z99`. The set is found once per run and read through the same non-blocking, identity-checked opens as [split volumes](#split-volumes), and `name.zip` must be a regular file. Volumes may differ in size, since removable media hold what fits. The set is damaged, and nothing is read, when any of these holds. The error names the volume at fault:

- A gap in the numbering. Zipper looks for the 16 numbers after the first missing one.
- A volume that is empty, is not a file, or whose metadata cannot be read.
- The volumes add up past 16 EiB.

Any `.z01`, `.z02` or other numbered volume opened by itself is refused. The error names `name.zip` and says to open it, or says that it is missing.

The reader then checks the set against the end record:

- The end record, or the ZIP64 end record, must name `name.zip` as the last disk. So the number of volumes must equal its disk count, and a stray `name.z01` beside a whole `name.zip` is refused.
- The ZIP64 locator must count the same disks and point inside a volume.
- The central directory must start inside the volume its disk number names, and end at the end record. It may run across volumes.
- Every entry's disk number must name a volume, and its offset must lie inside that volume. An offset is never read against the joined length.
- A disk number of `0xFFFF`, which defers to a ZIP64 field, is refused.

Each of these errors names the volume or disk and the offset. A disk number past the set is named by its number, such as `disk 10`. A `name.zip` whose end record names other disks, read without its earlier volumes, is refused with the advice to put them beside it.

Inside the joined volumes the reader applies every check a single ZIP gets. Records must follow each other without gaps or overlaps, and every local header must match its central entry. So a volume that is repeated, swapped, shifted or cut fails. The error gains the volume line, such as "read as split ZIP volumes `a.z01` to `a.zip`". It names a volume that holds the same bytes as an earlier one. The same repeat scan as for numbered volumes runs. A failure in an entry's data names the set, not the volume that holds the fault. Encrypted entries stay refused. An archive in one piece that starts with the spanning signature `PK\x07\x08` or `PK00` has its first record right after it, and every disk number zero.

`tests/zip_spanned.rs` extracts Info-ZIP split sets from the corpus and from `zip -s` at 64 KiB, 65 KiB and 100 KiB, stored, deflated and with ZIP64 records, with 1 and 4 threads. Each must match the unsplit archive's tree, and 7-Zip's tree when 7-Zip is installed. Its hostile cases cover a gap, a repeated, swapped or overlapping volume, and an extra or missing last volume. They also cover an empty or non-file volume, a volume opened alone, a stray `name.z01`, disk numbers and offsets outside their volume, and an encrypted set. Each must fail, name a volume, and publish nothing. 7-Zip writes no `.z01` sets, so Info-ZIP is the only writer covered. `every_offset_in_uneven_volumes_maps_to_the_volume_that_holds_it` in `src/volumes.rs` proves with Kani that every offset of three volumes of any sizes maps to the volume and position that hold it. `uneven_volumes_locate_every_offset_as_a_linear_scan_does` checks every set of up to four volumes of 1 to 4 bytes.

## Extraction options

These options work the same way in every reader. [Usage](../usage.md#extract-into-an-existing-folder) shows them.

- `Existing::Overwrite`, or `zipper extract --overwrite`, extracts into an existing folder. The whole archive decodes into a hidden staging folder first. Each file then replaces the file of the same name by rename. A rename that fails undoes the ones before it, so the destination keeps what it held, as [usage](../usage.md#extract-into-an-existing-folder) describes. A cancel seen before the last rename and the folder modes undoes them the same way, as [usage](../usage.md#extract-from-rust) describes. `tests/overwrite.rs` covers links, conflicts and failed renames in the destination.
- `Control` carries a progress callback and a cancel flag. `tests/control.rs` covers cancellation at any point.
- `paths`, `include` and `exclude`, or `--path`, `--include` and `--exclude`, select entries, as [usage](../usage.md#extract-selected-paths) describes. Every entry's local header is still checked. `tests/selection.rs` covers it.
- `strip`, or `--strip-prefix` and `--strip-components`, places a folder's contents at the destination root, as [usage](../usage.md#extract-a-folder-at-the-output-root) describes.
- `metadata: Metadata::ContentOnly`, or `--content-only`, writes contents and the execute bit only, as [usage](../usage.md#extract-contents-only) describes.

## Streaming

The reader streams every entry. Each worker owns one 1 MiB buffer. It reads compressed input into the first 480 KiB and decodes Deflate into the other 544 KiB with Zipper's own decoder in `src/inflate.rs`, which is safe Rust. That part also holds the last 32 KiB of output, the history that Deflate matches copy from, so the decoder allocates nothing. The 512 KiB beside the history hold a whole read of stored blocks. A read that ends inside a stored block therefore leaves in one write.
Streaming buffers scale with workers, not expanded file size.

## Memory model

Parsed metadata is shared across workers.
A streamed entry with a writer thread also holds five 1 MiB write buffers.
An XZ or LZMA entry's dictionary can use as much memory as the entry's size. Before starting one, a worker takes that size, at most 2 GiB, from a 2 GiB budget that all workers share. A PPMd entry takes 256 MiB. So all XZ, LZMA and PPMd decoders together use about 2 GiB at most, plus one entry larger than that. A worker waits at most one second for this memory. When the budget stays short, it records the entry and moves on, and the extraction decodes the recorded entries in another pass once the current one ends. Each pass starts with the whole budget free, so it always decodes at least one entry. A worker that waited without a limit, as the 7z reader's workers do, could hang a progress callback that runs Rayon work. Without any wait, a PPMd archive on a machine with more than 8 threads took 1.3 times as long. The [patience measurements](../../benchmarks/native-zip/decoder-memory-patience/README.md) record the evidence. An LZMA dictionary larger than its entry is cut to the entry's size, which decodes the same bytes.
A Deflate64 entry runs one decoder. It gets the last compressed byte only after the rest. A decoder that has already finished reads nothing more, so a stream with bytes after its end leaves that byte unread and is refused.

## Concurrency

Automatic native ZIP extraction on macOS uses at most four workers when both of these hold:

- The archive has at least 1,024 regular files.
- Its regular files average at most 64 KiB.

The limit avoids the measured filesystem contention from using every CPU.
An explicit `--threads N` overrides it.
Verification-only reads and archives of larger files keep their existing concurrency.
Windows uses one worker when the archive has at least 1,024 regular files of at most 16 KiB each and one folder holds 1,024 of them. Parallel creation in one folder was slower than serial there. Linux has no cap.
The [small-file cap record](../../benchmarks/native-zip/small-files-cap/README.md), the [small-file ZIP versus bsdtar measurements](../../benchmarks/native-zip/bsdtar-comparison.md) and the [Linux and Windows scoreboard](../../benchmarks/cross-platform/scoreboard-2026-11-02/README.md) record the evidence.

Workers take entries one at a time from the largest-first list, so no worker is left alone with a run of entries at the end. A worker reuses an idle reader and read buffer instead of creating new ones. The [one-entry-per-task measurements](../../benchmarks/native-zip/one-entry-per-task/README.md) record the evidence.

A streamed entry of at least 16 MiB gets a writer thread when the pool has more threads than entries with a payload. Every entry that is not a directory counts, including symlinks. The worker then decodes while the writer thread writes, so the number of busy threads never exceeds `--threads`. Spare threads are counted once, before extraction starts. `--threads 1` never gets a writer thread. The [write-behind measurements](../../benchmarks/native-zip/write-behind/README.md) record the evidence. A TLA+ model in `verification/zip-write-behind/` checks every order of the two threads' steps, within a bound, for hangs, lost or reordered buffers, a misreported error and unbounded buffers. The `zipper-verify-zip-write-behind` skill in `.claude/skills/` runs it.

## Input read-ahead

| Platform | Input buffer |
| --- | --- |
| Windows | starts at 8 KiB, grows to 256 KiB after sustained sequential reads |
| Other platforms | 8 KiB |

On Windows, moving seeks reset the read-ahead to 8 KiB.
The growth adds up to 248 KiB per active file reader.
The [platform-specific measurements](../../benchmarks/native-zip/adaptive-input/README.md) record the evidence.

## Output write size

On Windows, a writer thread writes its 1 MiB buffers in 256 KiB slices.
The [large-entry measurements](../../benchmarks/native-zip/windows-write-size/README.md) show why.
