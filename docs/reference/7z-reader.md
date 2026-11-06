# In-process 7z reader

Zipper reads 7z archives with its own Rust reader by default. The default engine, `--zip-engine auto`, opens a file that starts with the 7z signature, or a [self-extracting 7z](#self-extracting-7z), and reads its header first. When a folder uses a method the reader does not decode, or the header itself is encrypted, the archive goes to 7-Zip with the format pinned, before anything is extracted. Every other archive goes to the reader, including damaged and unsafe ones, so the reader refuses them rather than 7-Zip extracting them. `--zip-engine native` always uses the reader, and `--zip-engine backend` always uses 7-Zip.

## Supported archives

The reader decodes these methods:

- LZMA and LZMA2, through liblzma.
- Copy, Deflate, Deflate64, BZip2 and PPMd. A PPMd model counts its full size against the decoder budget, because the decoder allocates it at the start. A PPMd folder must end exactly at its packed size. Unlike 7-Zip, Zipper does not check that the PPMd range coder ends in a clean state, because the decoder crate keeps that state private. The folder's size and CRC still bound every byte written. Each BZip2 stream after the first must bring 4 KiB of output, as in the [single compressed file reader](stream-reader.md).
- Unused packed bytes after a Deflate or Deflate64 stream are not checked. The folder's size and CRC still bound every byte written.
- The x86 (BCJ), ARM, ARM Thumb, ARM64, RISC-V, PowerPC, SPARC and IA-64 branch filters, and the Delta filter, in front of LZMA or LZMA2. LZMA has no end marker, so the reader decodes it alone to the folder's size. It passes the result to the filters as stored LZMA2 chunks, whose end marker lets a branch filter release its last bytes.
- The Swap2 and Swap4 filters at the output end of any supported folder. Each reverses every whole group of 2 or 4 bytes and leaves a shorter tail as it is, as 7-Zip does. A swap with property bytes goes to 7-Zip, which refuses it.
- The unpack size of a coder inside a chain is not checked, so a chain whose inner sizes disagree with the folder's can be accepted where 7-Zip reports a data error. The folder's size and CRC still bound every byte written.
- BCJ2, the x86 filter with four input streams, decoded by Zipper's own port of the public-domain LZMA SDK decoder. Each of its four inputs is a chain of the methods above, or a packed stream read as is. Every coder and packed stream in the folder must feed exactly one input, so a folder holds at most 5 coders. Each input and each chain's decoder must end exactly where BCJ2 stops reading. As in 7-Zip, a branch marked as converted with fewer than 4 bytes of output left is refused. The four chains reserve their decoders and buffers together, and a BCJ2 folder that needs more than the 2 GiB budget goes to 7-Zip. BCJ2 decodes one byte at a time. The review measured it at about 4 times 7-Zip's time on one 28 MiB archive, and no benchmark record covers it yet.

It reads solid and non-solid archives, solid blocks, compressed and plain headers, and archives that 7-Zip and libarchive write. It extracts files, empty files, folders and symlinks that stay inside the destination. On Windows it creates links as the [ZIP reader](zip-reader.md#symlinks) does.

The reader does not support these features:

- More than 3 filters in front of LZMA or LZMA2, which liblzma cannot chain. The default engine sends these archives to 7-Zip.
- A byte-swap filter after another filter, a coder above BCJ2, other coders with several streams, and a PPMd model larger than the 2 GiB decoder budget. These go to 7-Zip too.
- Encrypted archives and encrypted headers. These go to 7-Zip too.
- Anti-items, which delete files during an update, external data and additional header streams.
- Windows reparse points, devices and FIFOs.
- Names with a backslash, which Windows reads as a separator and Unix as part of the name.

## Self-extracting 7z

A self-extracting 7z is an executable stub followed by a 7z archive. Every offset in a 7z counts from its signature, so the archive reads the same after any stub. The reader looks for the signature only when the file starts with `MZ`, `\x7FELF` or one of the Mach-O magic numbers. It reads the archive only when the stub before it passes the stub check in the [ZIP reader](zip-reader.md#self-extracting-zip), which both readers share. A stub that holds a ZIP record is not refused there. The file is read as a ZIP instead, as 7-Zip reads a self-extracting ZIP that stores a 7z. 7-Zip searches any file, but searching every unrecognized file would read 8 MiB of each gzip or ZIP self-extractor that reaches this reader.

The signature must start within the first 8 MiB, at offset 8,388,608 or before. 7-Zip 26.05 on macOS opens an archive whose signature starts at that offset and not one byte past it. Each signature found there counts only when its start header has version 0 and its CRC matches. When two signatures pass, the file holds two archives, and the error names both offsets. That includes a stored 7z inside the first 8 MiB of the archive itself. A start header that fails its check is refused as a 7z signature in the stub. The archive may be followed by other bytes, as a signed installer is. A ZIP end record in the last 64 KiB and 22 bytes of the file is refused, because Python and bsdtar would then read a ZIP. That includes a ZIP the 7z stores near its end. The ZIP reader refuses a 7z signature in a ZIP's prefix, so a file is never read as the other format. When the default engine sends a self-extracting 7z to 7-Zip, for a method or encryption the reader does not decode, it pins `-t7z`, so 7-Zip cannot open an archive in the stub instead. Only a stub that passed the check gets that far.

`7zz a -sfx` fails on macOS, which ships no SFX module. So `tests/self_extracting.rs` builds each self-extractor by hand, from a generated PE, ELF or Mach-O stub and six corpus and parity archives. That includes BCJ2, a plain header, links and bsdtar's output. Stubs run from 300 bytes to exactly 8 MiB. Each must extract the tree the archive gives alone, and the tree 7-Zip extracts when it is installed. A signature 1 byte past the limit, an archive after a shell script and two archives are refused. So are an archive followed by a ZIP, after a ZIP in the stub, or after a damaged start header. Each denied signature and a stub whose header is not whole are refused too. An encrypted 7z after a clean stub reaches 7-Zip with `-t7z`, and one after a stub that holds a CAB never reaches it.

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
- `paths`, `include` and `exclude`, or `--path`, `--include` and `--exclude`, select entries, as [usage](../usage.md#extract-selected-paths) describes. Only folders that hold a selected file decode, and each decodes completely, because 7z has no way to skip into a folder and its CRC covers all of it.
- `strip`, or `--strip-prefix` and `--strip-components`, places a folder's contents at the destination root, as [usage](../usage.md#extract-a-folder-at-the-output-root) describes.
- `metadata: Metadata::ContentOnly`, or `--content-only`, writes contents and the execute bit only, as [usage](../usage.md#extract-contents-only) describes.
- `Control` reports progress with both totals known, and stops on the cancel flag.

Folders decode in parallel on their own worker threads, up to `--threads`, largest first. A solid archive has one folder. When that folder uses LZMA2 alone and holds at least 8 MiB, the reader reads the chunk headers, splits the stream where it resets its dictionary, and decodes those segments on separate threads, in order. Segments in flight hold their packed and decoded bytes, at most 512 MiB per folder, so solid archives use more memory. Any other layout decodes on one thread. The [parallel segment measurements](../../benchmarks/native-7z/lzma2-segments/README.md) record the gain. A TLA+ model in `verification/7z-folder-workers/` checks every order of the workers' steps, within a bound, for hangs, budget overruns, folders decoded twice, and a publish after a corrupt folder or a cancel. The `zipper-verify-7z-workers` skill in `.claude/skills/` runs it.

## Reference corpus

`scripts/make_7z_corpus.py` writes 63 archives from 7-Zip and libarchive, over the short and long trees of the tar corpus and a tree of links. It covers each supported method, solid blocks, plain headers and non-solid archives. BCJ in front of LZMA2 and of LZMA, and BCJ2, also run over a tree of x86-like code, so that BCJ2's call and jump streams hold converted targets. BCJ2 appears both in 7-Zip's default layout and with every branch bound to its own LZMA coder. 7-Zip and bsdtar must extract the same tree, and `tests/sevenz_corpus.rs` checks the reader against it, including modes and times. Two more archives use a password and an LZMA setting liblzma refuses, and the test checks that the reader refuses each by name and that the default engine sends them to 7-Zip.
