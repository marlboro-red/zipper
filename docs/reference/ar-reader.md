# In-process ar reader

Zipper reads `ar` archives, such as static libraries and Debian packages, with its own Rust reader. Zipper sends a file here when it starts with `!<arch>` and a newline. A file that starts with a ZIP, tar, cpio or 7z signature goes to that reader first.

## Supported archives

The reader supports these features:

- System V and GNU names, which end with `/`, and the GNU long name table `//` with its `/offset` references.
- BSD names, where `#1/` and a length put the name at the start of the member's data. Apple's `ar` and `llvm-ar --format=darwin` write these.
- Short names without a trailing `/`, as older writers spell them.
- A member of odd size, padded to an even offset. The pad byte may be missing at the end of the file.

The reader leaves out the symbol tables a linker reads, which are the GNU `/` and `/SYM64/` members, the Windows `/<ECSYMBOLS>/` member and the BSD `__.SYMDEF` members. It refuses thin archives, which start with `!<thin>` and name files outside the archive, and any other special member whose name starts with `/`.

## Debian packages

A Debian package is an `ar` archive whose first member is named `debian-binary`, written without GNU's trailing `/`. 7-Zip treats such an archive as a package, hides `debian-binary`, and opens its one member whose name starts with `data.tar.`. Zipper follows the same rule, and also takes a plain `data.tar` member, as `dpkg` does:

- When the package has exactly one member named `data.tar` or `data.tar.*`, the reader extracts the tar inside it with the [tar reader](tar-reader.md). Every option applies to the tar's paths, and every tar check applies. `control.tar.*` and the other members are not extracted.
- The member must hold a tar the tar reader finds. That is a plain tar or a tar inside gzip, xz, zstd, bzip2, lzip, LZ4 or `compress`. A `data.tar.lzma` member is refused, because raw LZMA has no signature.
- When the package has no such member, or more than one, the reader extracts the members other than `debian-binary` as files.

The tar reader reads the data member in place, at its offset inside the package, so no copy is written. Its staging folder, output limits, cancel checks and the Windows Mark of the Web all come from the package itself.

## Metadata

- Each file gets the mode its header gives, masked to `0o777`, or `0o644` when the field is blank. The umask limits it.
- Each file gets the modification time its header gives, in whole seconds, unless the field is blank.
- A name with `/` in it, from a BSD or GNU long name, creates its folders with the default mode.

Owners and groups are not restored.

## Safety checks

The reader checks the whole archive before it publishes anything, and it publishes nothing on failure:

- Every header ends with the two bytes `` ` `` and newline. The size field holds decimal digits followed only by spaces. The time field holds decimal digits and the mode field octal digits, or only spaces.
- Every member lies inside the file. The reader checks each size against the file's length before it reads a byte of the member, so a declared size never sets an allocation.
- A GNU long name table appears once and before any name that uses it. It is at most 16 MiB. Each long name lies inside it, ends with a newline or NUL, and is at most 8,192 bytes. The reader checks the length before it copies the name, so many members that name one long entry cost no more than one name each.
- A BSD name length is at most 8,192 bytes and fits inside its member. Trailing NUL bytes of the name are removed.
- Names pass the same checks as tar names after a leading `./` is removed. Absolute names, `..`, drive letters, backslashes, NUL bytes and empty names are rejected. Names that are not UTF-8 are refused.
- No member sits inside another member's name. A repeated name follows the last member, as `ar x` and 7-Zip do. Static libraries often repeat an object name.
- Output must fit in free space, or under `--max-extracted-bytes`. Each member is charged before its data is written.

An `ar` archive has no end marker. A file cut exactly between two members, or just before the last pad byte, reads as a shorter archive, as it does in GNU `ar` and 7-Zip. A cut anywhere else is refused.

## Limits

| Limit | Value |
| --- | --- |
| Members, including symbol tables | at most 1,000,000 |
| GNU long name table | at most 16 MiB |
| Names and implied directories | at most 64 MiB in total |
| One GNU or BSD name | at most 8,192 bytes |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader passes over the archive twice. The first pass reads only the headers and the long name table, to decide whether the file is a Debian package. Both passes check the cancel flag at every member, and both step through the file within its read buffer, so a run of empty members costs no seek. The second pass extracts. One thread writes each member in archive order through a 1 MiB buffer. `--threads` has no effect on `ar` itself. A package's tar uses the tar reader's writer threads, and a bzip2 tar decodes its blocks on up to one thread fewer than `--threads`.

## Differences from 7-Zip

`tests/parity.rs` requires the tree of the GNU archive in the [parity corpus](parity.md) to match 7-Zip's. `tests/hostile_ar.rs` compares GNU, BSD and System V archives and nine package layouts with 7-Zip where it is installed. The reader differs from 7-Zip 26.05 on purpose in these cases:

- 7-Zip writes a GNU symbol table as a text file named `1.txt` that lists each symbol, and a BSD `__.SYMDEF` member either as a raw file or as `1.txt`. Zipper writes neither, because a linker index is not a file of the archive.
- 7-Zip writes a package's data member decompressed, as `data.tar`. Zipper extracts the files inside it, as it does for a `.tar.xz`. The parity corpus reads 7-Zip's output the same way.
- 7-Zip writes a `data.tar.*` member that holds no tar as `data.tar`. Zipper refuses the package.
- 7-Zip writes a plain `data.tar` member as a file next to `control.tar.*`. Zipper extracts the files inside it, as `dpkg` does.

## Formal checks

Kani proves that the header reader never panics on any 60 bytes and that the sizes, times and modes it returns fit their fields. It proves that the name reader never panics on any 16 bytes, and that the long name lengths and offsets it returns fit their fields. It also proves that every size of up to ten digits reads back from its decimal field. The proofs are under `[ar-fields]` in `verification/coverage.txt`.

`tests/hostile_ar.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying.
