# In-process xar reader

Zipper reads xar archives and flat macOS installer packages (`.pkg`) with its own Rust reader. Zipper sends a file here when it starts with `xar!`. A file that starts with a ZIP, tar, cpio, `ar` or RPM signature goes to that reader first.

A flat package is a xar archive. A product package holds a `Distribution` file and one folder per component, and a component holds `Bom`, `PackageInfo`, `Payload` and sometimes `Scripts`. The reader writes these files as they are stored, as `xar -x`, `pkgutil --expand` and `7zz x -txar` write them. It does not unpack the gzip or pbzx cpio inside a `Payload`.

## Supported archives

The reader supports these features:

- The version 1 header, with a table of contents checksum of none, SHA-1, MD5, SHA-256 or SHA-512. Algorithm 3 is SHA-256, as Apple's `xar` writes it, or the algorithm the header names after its 28 bytes.
- File data stored or encoded as `application/x-gzip` (zlib), `application/x-bzip2`, `application/x-xz` or `application/x-lzma`. LZMA data may be an xz stream or an LZMA stream with its 13-byte header. Zipper's own xz and LZMA decoders read both.
- Archived and extracted checksums in MD5, SHA-1, SHA-256 or SHA-512.
- Files, directories, symlinks that stay inside the destination, and hard links.
- A field given twice with the same value, as `productbuild` writes each name twice.

The reader does not support these features:

- Devices, FIFOs and sockets.
- Names stored in base64, names that are not UTF-8, and other data encodings.
- Signatures. The reader neither checks them nor needs them.
- Extended attributes, which it skips.

## Metadata

The reader restores permission bits and times the way the [cpio reader](cpio-reader.md#metadata) does. Each file and directory gets its `<mode>`, masked to `0o777`, and its `<mtime>` in whole seconds. A time that is not of the form `2026-11-07T14:12:25Z` is ignored. A file without a mode gets `0o644`, and a directory `0o755`. The reader never restores owners or the inode numbers the archive stores.

Like 7-Zip, the reader also writes the table of contents, inflated, as `[TOC].xml` at the top of the destination. A top-level archive entry of that name, in any case, takes its place.

## Safety checks

The reader checks the whole archive before it publishes anything, and it publishes nothing on failure:

- The header has the `xar!` magic, version 1, a size of at least 28 bytes and a known checksum algorithm. The table of contents lies inside the file and declares at most 256 MiB inflated. The reader checks both before it inflates anything.
- The table of contents inflates to exactly the length the header gives. It is checked during inflation, so a longer stream stops at the limit.
- The checksum of the compressed table of contents matches the one in the heap, and its `<checksum>` element names the header's algorithm and its digest length. An archive without a header algorithm may have no `<checksum>` element.
- The table of contents is well-formed XML with one `<xar>` root and one `<toc>`. A document type declaration, an entity XML does not define, a token longer than 1 MiB, a field longer than 8,192 bytes and nesting deeper than 256 elements are refused.
- A field given twice with different values, a second `<data>` and an attribute given twice on one element are refused. Apple's `xar` refuses a repeated attribute too.
- Every data range lies inside the file. Two ranges either match exactly or do not overlap, and none overlaps the checksum. `xar` stores identical files once, so files may share one range.
- Each file decodes to exactly its declared size, checked during decoding. Its archived checksum covers every byte of its range, and its extracted checksum covers its decoded bytes. A stored file's length equals its size.
- A name is one path segment. Empty names, `.`, `..`, `/` and NUL are refused, and the joined path passes the same checks as tar names. No entry sits inside a file or a link.
- A symlink target is at most 4,096 bytes and follows the [ZIP symlink rule](zip-reader.md#symlinks). Links are created after every file.
- A hard link names the id of a file. An entry that follows a file a hard link names cannot replace it. Two files with the same id are refused.
- Only a file carries data.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Limits

| Limit | Value |
| --- | --- |
| Inflated table of contents | at most 256 MiB |
| One XML token | at most 1 MiB |
| One field, such as a name or a checksum | at most 8,192 bytes |
| Nesting | at most 256 elements |
| Entries | at most 1,000,000, though the record budget below refuses 515,000 minimal entries and accepts 450,000 |
| Entry records, names, link targets and the paths of folders and linked files | at most 128 MiB, counting each record at twice its size |
| One path | at most 4,096 bytes |
| One symlink target | at most 4,096 bytes |
| xz dictionary | at most 256 MiB |
| LZMA decoder memory | at most 272 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader inflates and parses the table of contents in one pass and never holds it whole. One thread then reads each file's range in table of contents order, with one decoder at a time, and writes it through a 1 MiB buffer. `--threads` has no effect on xar.

## Differences from 7-Zip

`tests/xar_corpus.rs` requires the trees of real archives from `xar`, `pkgbuild` and `productbuild` to match `7zz x -txar` and `xar -x`. `tests/hostile_xar.rs` compares archives it writes with 7-Zip where it is installed. The reader differs from 7-Zip 26.05 in these cases:

- For a component package, `7zz x` without `-txar` opens the `Payload` and writes its cpio, decompressed, as `Payload~`. Zipper writes the package's files, as `7zz x -txar` does.
- 7-Zip hard links the files that share an `<inode>` number, so they share one file's data. In a `xar` hard link group, every name then comes out empty. Apple's signed packages give every file inode 0, so 7-Zip writes `Payload` under the names `Bom` and `PackageInfo` too. Zipper links only the files whose type says `hardlink`, as `xar -x` does.
- 7-Zip refuses `application/x-xz` and `application/x-lzma` data as an unsupported method. Zipper decodes it.
- 7-Zip writes a device, FIFO or socket as an empty file. Zipper refuses the archive.
- 7-Zip writes a symlink that leaves the destination as an empty file and reports an error. Zipper refuses the archive.

`xar -x` does not write `[TOC].xml`, and Apple's `xar` reads no LZMA data.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [tar reader](tar-reader.md#extraction-options). Selection matches `[TOC].xml` like any entry. A selected hard link whose target the selection or strip leaves out is refused. Progress counts each file and its bytes as they are written. The cancel flag is checked between entries and while data is copied.

## Formal checks

Kani proves that the header reader never panics on any 28 bytes, and that the header it accepts bounds the table of contents. It proves that the checksum algorithm reader never panics on any code and name of up to 8 bytes, and that each data range it accepts lies inside the file. The proofs are under `[xar-header]` in `verification/coverage.txt`. An exhaustive test checks the range overlap rule against a pairwise reference on every layout of up to four ranges.

`tests/hostile_xar.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts an archive at many points before its end.
