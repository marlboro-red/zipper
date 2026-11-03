# In-process 7z reader

Zipper reads 7z archives with its own Rust reader by default. The default engine, `--zip-engine auto`, opens a file that starts with the 7z signature and reads its header first. When a folder uses a method the reader does not decode, or the header itself is encrypted, the archive goes to 7-Zip with the format pinned, before anything is extracted. Every other archive goes to the reader, including damaged and unsafe ones, so the reader refuses them rather than 7-Zip extracting them. `--zip-engine native` always uses the reader, and `--zip-engine backend` always uses 7-Zip.

## Supported archives

The reader decodes these methods:

- LZMA and LZMA2, through liblzma.
- Copy and Deflate.
- The x86 (BCJ), ARM, ARM Thumb, ARM64, RISC-V, PowerPC, SPARC and IA-64 branch filters, and the Delta filter, in front of LZMA or LZMA2.

It reads solid and non-solid archives, solid blocks, compressed and plain headers, and archives that 7-Zip and libarchive write. It extracts files, empty files, folders and symlinks that stay inside the destination. On Windows it creates links as the [ZIP reader](zip-reader.md#symlinks) does. An entry made on Windows and named `file:stream` is an NTFS alternate data stream, as 7-Zip stores with `-sns`. An entry made on Unix with a `:` in its name is refused as an unsafe path. A stream's file or folder must be in the archive. Its name must be plain, with no `$` at the start, no `/`, `\`, `:`, `<`, `>`, `"`, `|`, `?`, `*` or control character, and no trailing dot or space. Two streams of one file that differ only by case are refused, because NTFS ignores case. Zipper checks every stream and writes none by default. On Windows, `--streams` writes them onto their files after every file and link exists, up to 64 MiB in all, and keeps each file's time and read-only attribute. With `--overwrite`, a stream on a folder that already exists in the destination is not written. `--streams` with any other archive is an error.

The reader does not support these features:

- BCJ2, PPMd, BZip2, Deflate64 and the byte-swap filters. The default engine sends these archives to 7-Zip.
- Encrypted archives and encrypted headers. These go to 7-Zip too.
- Anti-items, which delete files during an update, external data and additional header streams.
- Windows reparse points, devices and FIFOs.
- Names with a backslash, which Windows reads as a separator and Unix as part of the name.

## Metadata

The reader restores permission bits and modification times the way the [ZIP reader](zip-reader.md#supported-archives) does. Unix modes come from the high 16 bits of the 7z attributes, which 7-Zip and libarchive write on Unix. An archive without them gives files the default mode, and read-only files mode `0o444` before the umask. Folder times and modes apply after their contents are written.

## Safety checks

The reader checks the whole archive before it publishes anything, and it publishes nothing on failure:

- Both header CRCs, every file CRC and every folder CRC.
- Names are safe archive paths, unique, and never used both as a file and as a folder. Symlink targets stay inside the destination and are created after every file.
- Packed streams lie before the header and do not overflow. Folder and file sizes agree, and a folder holds no data beyond its files, except LZMA1, which has no end marker and stops at its declared size. A folder with no output is refused, because 7-Zip never writes one.
- Counts are bounded by the bytes the header has left, and the header is at most 64 MiB, decoded or not. A header that lists the full 1,000,000 entries can still take a few hundred MiB of memory to hold.
- An LZMA dictionary is cut to the size of the folder's output, so a hostile dictionary size does not allocate memory. Decoders running at once share a 2 GiB memory budget.

`tests/hostile_7z.rs` crafts archives that break each check, and `tests/sevenz_fuzz.rs` mutates the corpus with repaired header CRCs.

## Extraction options

These options work the same way as in the other readers. [Usage](../usage.md#extract-into-an-existing-folder) shows them.

- `Existing::Overwrite`, or `zipper extract --overwrite`, extracts into an existing folder.
- `paths`, or `--path`, selects entries. Only folders that hold a selected file decode, and each decodes completely, because 7z has no way to skip into a folder and its CRC covers all of it.
- `Control` reports progress with both totals known, and stops on the cancel flag.

Folders decode in parallel on their own worker threads, up to `--threads`, largest first. A solid archive has one folder. When that folder uses LZMA2 alone and holds at least 8 MiB, the reader reads the chunk headers, splits the stream where it resets its dictionary, and decodes those segments on separate threads, in order. Segments in flight hold their packed and decoded bytes, at most 512 MiB per folder, so solid archives use more memory. Any other layout decodes on one thread. The [parallel segment measurements](../../benchmarks/native-7z/lzma2-segments/README.md) record the gain. A TLA+ model in `verification/7z-folder-workers/` checks every order of the workers' steps, within a bound, for hangs, budget overruns, folders decoded twice, and a publish after a corrupt folder or a cancel. The `zipper-verify-7z-workers` skill in `.claude/skills/` runs it.

## Reference corpus

`scripts/make_7z_corpus.py` writes 44 archives from 7-Zip and libarchive, over the short and long trees of the tar corpus and a tree of links. It covers each supported method, solid blocks, plain headers and non-solid archives. 7-Zip and bsdtar must extract the same tree, and `tests/sevenz_corpus.rs` checks the reader against it, including modes and times. Four more archives use BZip2, PPMd, BCJ2 and a password, and the test checks that the reader refuses each by name and that the default engine sends them to 7-Zip.
