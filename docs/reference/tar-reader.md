# In-process tar reader

Zipper reads tar archives with its own Rust reader by default. The default engine, `--zip-engine auto`, decodes any gzip, zstd, xz, bzip2, lzip, LZ4 or `compress` around a file. It sends the file to the tar reader when the first 512 bytes are then a tar header with a valid checksum or the `ustar` marker. A header with the marker and a wrong checksum goes to the reader, which rejects it, so a damaged `.tar.gz` is never handed to 7-Zip. A file that starts with a ZIP record signature goes to the [ZIP reader](zip-reader.md) first. A compressed file that does not hold a tar goes to the [single compressed file reader](stream-reader.md), which writes the decompressed file.

A tar the reader rejects is never handed to 7-Zip. A compressed empty tar holds only zero blocks, so it goes to 7-Zip, which writes the inner `.tar`. When the reader lacks a feature, such as sparse files or names that are not UTF-8, the error names `--zip-engine backend`, and passing it is the user's decision. `--zip-engine native` also reads tar archives in process.

## Supported archives

The reader supports these features:

- Plain tar, and tar inside gzip, including several gzip members, zstd, including the skippable frame `pzstd` writes first, xz, bzip2, lzip, LZ4 and `compress` (`.tar.Z`).
- xz through Zipper's own reader in `src/lzma.rs`, not liblzma. It reads several streams in a row with zero padding between them, blocks with or without sizes in their headers, and the branch and Delta filters before LZMA2. It checks the CRC-32, CRC-64 or SHA-256 each block carries, and the index of each stream. liblzma decodes a stream whose check type it does not know without checking it. This reader refuses such a stream, which no xz writer produces.
- POSIX ustar with its name prefix, pax extended headers, GNU long names and long link targets, and old tar without the `ustar` marker.
- Base-256 sizes and times, as GNU tar writes for large values.
- Files, directories, symlinks that stay inside the destination on Unix, and hard links to an earlier file in the same archive.
- An old-style directory, written as a file entry whose name ends in `/`.
- A pax global header that only carries ignored keys, such as the `comment` that `git archive` writes.

The reader does not support these features:

- Sparse files, in the GNU `S` type or the pax `GNU.sparse` keys.
- Devices and FIFOs.
- GNU volume labels, multi-volume parts and dump directories.
- Names and link targets that are not UTF-8, including a pax `hdrcharset` of `BINARY`.

A pax header may set `path`, `linkpath`, `size` and `mtime`. The reader ignores `atime`, `ctime`, `uid`, `gid`, `uname`, `gname`, `comment`, `charset`, and every key that contains a dot, such as `LIBARCHIVE.xattr.*` and `SCHILY.*`. It rejects any other key, because a new standard key could change how an entry is read.

## Metadata

The reader restores permission bits and times the way the [ZIP reader](zip-reader.md#supported-archives) does:

- Each file and directory gets its archive mode, masked to `0o777`. The umask limits it.
- Directory modes are applied after all files are written, deepest first. bsdtar lists a directory before its children, and GNU tar cannot then write into a read-only directory. Zipper can.
- Files get their modification time, with the nanoseconds of a pax `mtime`. Directory times are set after all files are written.
- Hard links share one file. Symlinks get no time or mode.

On Windows it restores no permissions, and it creates symlinks as the [ZIP reader](zip-reader.md#symlinks) does. File and directory times are restored as on Unix. It never restores owners, access times, extended attributes or ACLs.

## Safety checks

The reader checks the whole archive before it publishes anything, and it publishes nothing on failure:

- Every header checksum matches, as unsigned or signed bytes. A numeric field holds octal digits, then only spaces or NUL bytes, or a base-256 value. A size is never negative.
- Names pass the same checks as ZIP names after a leading `./` is removed. Absolute names, `..`, backslashes and NUL bytes are rejected.
- No entry sits inside a file or a link, and no entry reuses a name of another kind.
- A repeated name follows the last entry, as GNU tar, bsdtar and Python do. `tar -r` appends and overlapping arguments write such archives. A repeated directory merges and takes the last entry's mode and time. A repeated file replaces the earlier one, unless an earlier hard link names it.
- A symlink follows the [ZIP symlink rule](zip-reader.md#symlinks). A hard link names an earlier file entry. Links carry no data, and they are created after every file, so no file is written through a link.
- One entry gets at most one pax header, one GNU long name and one GNU long link. A pax path together with a GNU long name is rejected, because readers disagree on which wins. A pax header may not repeat a key.
- The archive ends with a zero block, optionally followed by another. Only zero bytes may follow the end marker, and a compressed stream must end cleanly with valid checksums. The reader decodes the whole stream to check this, as GNU tar does through `gzip`, so a stream of compressed zeros costs decoding time but no memory. Zero bytes after a gzip stream are rejected, as GNU tar reports them.
- Output must fit in free space, or under `--max-extracted-bytes`. Each entry is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Entries and implied directories | at most 1,000,000 |
| Names, implied directories and link targets | at most 64 MiB in total |
| One pax header, GNU long name or long link | at most 1 MiB |
| Largest xz, LZMA or lzip dictionary | 256 MiB, four times the 64 MiB of `xz -9e` |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

An LZMA or lzip stream whose header declares a larger dictionary is refused before decoding, with its size in the error. An xz stream is refused when one of its block headers declares a larger dictionary, before that block's dictionary is allocated. The xz limit falls between the same two dictionary sizes as liblzma's 272 MiB memory limit did, so the same streams pass. Each error names the largest dictionary accepted and points at `--zip-engine backend`.

One thread decodes and checks the stream. Four writer threads create and write the files, or the number `--threads` sets. A file larger than 1 MiB streams to one writer in order. On Windows, all files of one folder go to the same writer, because creating files in one folder from several threads is slower there. At most 16 MiB of file data waits between the decoder and the writers, so memory does not grow with file size or writer count. A repeated file name waits for every pending write before it replaces the file. A TLA+ model in `verification/tar-writer-pool/` checks every order of these steps, within a bound, for hangs, budget overruns, leaked chunks and lost write failures. The `zipper-verify-tar-pool` skill in `.claude/skills/` runs it.

## Extraction options

These options work the same way in every reader. [Usage](../usage.md#extract-into-an-existing-folder) shows them.

- `Existing::Overwrite`, or `zipper extract --overwrite`, extracts into an existing folder. The whole archive decodes into a hidden staging folder first. Each file then replaces the file of the same name by rename. A rename that fails undoes the ones before it, so the destination keeps what it held, as [usage](../usage.md#extract-into-an-existing-folder) describes. `tests/overwrite.rs` covers links, conflicts and failed renames in the destination.
- `Control` carries a progress callback and a cancel flag. `tests/control.rs` covers cancellation at any point. While the reader creates links after the last entry, it reports progress and checks the flag every 1,024 links.
- `paths`, `include` and `exclude`, or `--path`, `--include` and `--exclude`, select entries. The reader still parses and checks every header, and writes only the selected entries. A selected hard link whose target is left out is refused, and each `--path` must match an entry.
- `strip`, or `--strip-prefix` and `--strip-components`, places a folder's contents at the destination root, as [usage](../usage.md#extract-a-folder-at-the-output-root) describes. The reader checks the prefix after the last header, before it publishes. A repeated name replaces its earlier file only when both spellings land on one path, as `a/x` and `./a/x` do without a strip. Under `--strip-components`, a leading `./` counts as a segment, so the two spellings can land apart. Each then keeps its own file, and a spelling that lands on another entry's path is refused.
- `metadata: Metadata::ContentOnly`, or `--content-only`, writes contents and the execute bit only, as [usage](../usage.md#extract-contents-only) describes.

## Reference corpus

`tests/data/tar-corpus` holds 45 archives from three writers over three source trees:

- GNU tar 1.35 in its `gnu`, `oldgnu`, `pax`, `ustar` and `v7` formats.
- bsdtar 3.7.4 in its `ustar`, `pax`, `gnutar` and `v7tar` formats.
- Python 3.14 `tarfile` in its ustar, GNU and pax formats.
- gzip, multi-member gzip, zstd and xz versions from each writer, and lzip, LZ4 and `compress` around a Python pax tar. 7-Zip reads neither, so Python and GNU tar read them through the decoders in `scripts/make_stream_files.py`.

The short tree has empty files and directories, non-ASCII names, files of 512 and 513 bytes, and a read-only directory. The links tree has symlinks and hard links. The long tree has names and link targets past 100 bytes. GNU tar and Python refuse the long tree in ustar and v7, and `corpus.json` records each refusal.

`scripts/make_tar_corpus.py` extracts each archive with GNU tar, bsdtar, Python and 7-Zip, and records in `manifest.tsv` the tree that at least three of them produce identically. Modes and times are recorded where two of GNU tar, bsdtar and Python agree. `corpus.json` names each reader that disagreed:

- 7-Zip refuses symlinks whose targets start with `..`.
- 7-Zip truncates long names and long link targets in bsdtar's pax archives.
- GNU tar fails on bsdtar archives with a read-only directory, and sets some directory times before writing into them.

`tests/tar_corpus.rs` requires the reader to reproduce every manifest exactly. Regenerate the corpus on macOS with `/opt/homebrew/bin/python3.14 scripts/make_tar_corpus.py`, with GNU tar installed as `gtar`. Owners are written as 0 with empty names.

The [real-world comparison](../../benchmarks/realworld/README.md) also extracted 248 popular archives from package registries and GitHub with Zipper and default bsdtar. All 154 tar and 94 ZIP archives matched.

`tests/hostile_tar.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying.

## Mutation testing

`tests/tar_fuzz.rs` mutates the plain corpus tars with seeded bit flips, truncations and insertions. It also overwrites header fields with edge values such as `../escape`, base-256 numbers and other entry types, then repairs the header checksum so the parser goes past it. It inserts pax headers with hostile records, and gzips a quarter of the mutants. The reader extracts each mutant with one or four writers. Each case must either fail and leave nothing in the destination's parent, or succeed with verification agreeing and every file at its declared size. A panic fails the test.

The gate runs 1,000 cases. `ZIPPER_FUZZ_CASES` and `ZIPPER_FUZZ_SEED` set a longer run or replay a failure, as for the ZIP test. A run of 300,000 cases on 2026-11-03 found no failure, and 51,825 of its mutants extracted. Removing the name check made the test fail on case 181, which wrote outside the destination. This is random mutation, not coverage-guided fuzzing.
