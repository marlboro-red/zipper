# In-process executable reader

Zipper extracts executables and object files with its own Rust reader, in the tree `7zz x` writes. It reads PE images, COFF objects, UEFI TE images, ELF files of both classes and byte orders, Mach-O files and Mach-O universal (`Mub`) files. Zipper sends a file here when it starts with `MZ`, `VZ`, `\x7FELF`, a Mach-O magic or `CA FE BA BE 00 00 00` and parses whole with no data after it. A COFF object has no signature, so it is tried last, after every other format.

## Routing

An executable is often the stub of a self-extracting archive. A file that parses whole goes to this reader before tar and the others. A file that starts as an executable but does not parse whole goes first to the 7z reader, which finds a 7z after a stub. A file that ends in a ZIP end record goes to the ZIP reader, as the [self-extracting rules](zip-reader.md) describe. Any other file that starts as an executable comes back here, so a damaged executable gets this reader's refusal. A Mach-O universal file with the `B9 FA F1 0E` signature is refused with `Zipper does not read Mach-O universal files with the B9 FA F1 0E signature`.

## What it writes

Each form writes the files that `7zz x` writes on macOS and Linux.

- A PE image writes each section by its name, cut to its virtual size. The `.rsrc` section becomes its resources, and the `.reloc` section that the relocation directory starts is cut to the directory's size. The certificate table is `CERTIFICATE`, each debug entry whose data lies outside every section is `.debugN`, and a COFF symbol table is `COFF_SYMBOLS`.
- A COFF object writes each section and `COFF_SYMBOLS`.
- A TE image writes each section, with its data moved by the size the header says was stripped.
- An ELF file writes each section by its name, read one byte to one character, with `NULL` for the first and `ERROR` when the name table cannot give one. A file with no sections writes each segment but `PT_PHDR`, numbered from `0`.
- A Mach-O file writes each section as its segment name followed by its section name, and each segment without sections by its own name. A zero-fill section writes no bytes. A symbol table that lies outside the segments read before it writes `_SYMTAB_TABLE` and `_SYMTAB_STRINGS`.
- A Mach-O universal file writes each slice whole, named after the archive without a `.mub` extension, a dot, and the CPU name 7-Zip gives, such as `ls.x64` or `ls.arm64e`.

Resources go under `.rsrc`, by type and name, with a folder per language when a resource after the first one that is not a version names another language. Types with a name in the format, such as `ICON` or `MANIFEST`, use it. These resources are converted as 7-Zip converts them:

- String tables become one UTF-16 `string.txt` for each language, numbered as 7-Zip numbers them. A string table that does not parse leaves every string table raw.
- A version resource becomes the UTF-16 `version.txt` text 7-Zip writes, with its fixed fields named.
- An icon with a 40-byte header, one plane, 1, 4, 8, 24 or 32 bits and no compression gains an icon directory and an `.ico` name.
- A bitmap with a 40-byte header and one plane gains a file header and a `.bmp` name, unless it is compressed and gives no size for its pixels.

The bytes of the `.rsrc` section past both the last resource and its virtual size, rounded up to a file alignment that is a power of two, become `.rsrc_1`. They start right after the last resource when a byte before that point is not zero. A section whose resource directory does not parse stays one raw file, as 7-Zip leaves it.

An item with no name takes the archive's name without its extension, or with `~` added when the name has none. Sections, the certificate, debug data and symbols of a PE image or COFF object get the header's time stamp as their modification time. Every other item gets the archive file's modification time, as 7-Zip gives an item that stores no time. Files get mode `0o644` and folders `0o755`, limited by the umask.

## Safety checks

The reader reads only headers, tables and the resource directory before it publishes anything. It publishes nothing on failure.

- Every form but COFF must end exactly where its headers say it does. 7-Zip opens an executable followed by other data only as an archive after a stub, and refuses it otherwise. A COFF object may have data after it, as 7-Zip allows.
- Every range an item names must lie inside the file. 7-Zip lists such an item and fails to extract it.
- A PE header pointer must lie past the MZ header, with the `PE` signature, a PE32 or PE32+ optional header and at most 16 data directories inside it. A debug directory may hold at most 16 entries, and one inside a section must be a whole number of them.
- A COFF object must name a machine 7-Zip knows, have no optional header, point at its symbols and have its symbols and string table inside the file.
- A TE image must name a machine and subsystem 7-Zip knows, hold sections, place their data after its section table and keep its data directories below 2^28 bytes.
- An ELF file must be version 1 with zero identification padding, header and table entry sizes of its class, and table offsets past the header and aligned to its word. A section entry size must be at most 1023 bytes, or 2^31 - 1 in the first section.
- A Mach-O file must hold 1 to 2^21 load commands in at most 16 MiB. Each command's size must be a multiple of 8, or 4 in a 32-bit file. Segment, symbol table, UUID, version and build version commands must have their exact sizes.
- A Mach-O universal file must hold 1 to 8 slices of known CPU types, with alignments up to 2^31 and subtypes without bits 10 to 15 set. No slice may start inside the header or share bytes with another.
- A PE resource directory must be 4-byte aligned, with named entries first as it counts them, and its tables must not share a byte. Data outside the section fails, apart from a version or string table, which is left out as 7-Zip leaves it out.
- Names pass the same checks as tar names. `..`, `.`, empty segments, backslashes and `:` are refused.
- Output must fit in free space, or under `--max-extracted-bytes`. Each file is charged before its data is written.

## Where it differs from 7-Zip

Zipper is stricter than 7-Zip in these cases. Each refuses a file that 7-Zip extracts.

- A name that the shared name check refuses, such as `.`, `..`, one with a NUL, `\` or `:`, which 7-Zip rewrites.
- Two names that differ only in case or Unicode normalization, on a filesystem that ignores them. 7-Zip keeps the second.
- A resource name that is not valid UTF-16.
- An ELF section name longer than 4,096 bytes.
- A resource directory of more than 2^22 entries, converted text past 64 MiB, or names past 64 MiB in total.

7-Zip's own names for bytes that are not valid UTF-8 are kept. A PE or TE section name or a Mach-O name in such bytes takes the character U+EF00 plus the byte, or `_` in Mach-O, as 7-Zip writes it on macOS. 7-Zip on Linux writes the raw byte instead, and Zipper writes U+EF00 plus the byte on every system, so the tree is the same everywhere.

## Limits

| Limit | Value |
| --- | --- |
| Items | at most 1,000,000, as the shared tree allows |
| Names | at most 64 MiB in total |
| PE sections | 65,535, the section table the header counts |
| ELF sections and segments | 65,535 each |
| Mach-O load commands | 16 MiB |
| Mach-O universal slices | 8 |
| PE resource directory entries | 2^22 |
| Converted string and version text | 64 MiB in all, from resources of up to 16 MiB each |

The reader holds the section, segment or load command table, each item's name and range, and the converted resource text. It never holds a section's data. Peak resident memory on files that fill the limits is in this table. `7zz` is 7-Zip 26.05. Zipper before this reader sent these files to its ZIP reader, which refused them.

| File | Size | `zipper list` | `zipper extract` | Old `zipper list` | `7zz l` | `7zz x` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| PE image of 999,000 named resources in 999 types | 48 MB | 474 MB | 459 MB | 3 MB | 301 MB | 301 MB |
| ELF file of 65,535 sections with 54-byte names | 7.8 MB | 36 MB | 38 MB | 3 MB | 10 MB | 11 MB |
| PE image of 65,000 sections | 2.6 MB | 24 MB | 26 MB | 3 MB | 20 MB | 20 MB |

`write_executables_at_the_documented_limits` in `tests/hostile_exe.rs` writes these files. Most of the PE resource figure is the shared tree, which holds every path.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the paths `7zz x` writes. Progress counts each file and its bytes as they are written. The cancel flag is checked between items and while an item's data is written.

## Formal checks

Kani proves these rules under `[exe-fields]` in `verification/coverage.txt`:

- The MZ, COFF and section header readers never panic, the PE header pointer lies past the MZ header, and a section's data size is at most its raw size.
- An optional header of any length up to 240 bytes keeps at most 16 data directories, all inside its bytes, and leaves the rest empty.
- A TE header it accepts has sections, a known machine and subsystem and directories below 2^28 bytes.
- An ELF header it accepts has the header, program header and section header sizes of its class, version 1 and zero padding.
- A Mach-O header it accepts holds at least one command and no more than its 16 MiB of commands can hold.
- A Mach-O universal slice it accepts is never empty, ends below 2^64, has an alignment up to 2^31 and a CPU type and subtype 7-Zip accepts.
- The icon and bitmap header builders never panic on any 40 bytes, and an icon they accept is 1 to 256 pixels wide and uncompressed.

`tests/exe_corpus.rs` extracts the corpus in `tests/data/exe-corpus`, built by its `make_corpus.py` with LLVM, Apple clang and lipo, and 240 generated PE, COFF, TE, ELF, Mach-O and universal files. Each must match its recorded tree and the tree, modes and times `7zz x` writes, and each section, segment and slice must hold its source bytes. `tests/hostile_exe.rs` damages the corpus 480 times at random and cuts a small ELF at every length, and must refuse each one exactly when 7-Zip refuses it, or for a reason the list above gives. Its ignored tests run 12,000 damaged files and every cut of four more. Each refusal in the safety checks leaves nothing behind.
