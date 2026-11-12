# M13. Remaining codecs

Every method 7-Zip decodes inside ZIP, 7z and tar decodes in process.

Scope:

- ZIP methods Deflate64, BZip2, LZMA, Zstandard, xz and PPMd.
- 7z methods BZip2, PPMd and BCJ2, which the default engine sent to 7-Zip before this milestone.
- Tar inside bzip2, lzip, LZ4 and the old `compress` format (`.Z`).
- A single compressed file outside tar, such as `.gz`, `.bz2`, `.xz`, `.zst` or `.lzma`, which extracts to one file named from the archive.
- Each decoder stops exactly at its declared size and checks the CRC of every byte. A Rust crate is used only after the `zipper-review` skill checks it on hostile input.

Exit checks:

1. Every method row in the M10 matrix for ZIP, 7z and tar reads native, except ZIP's Shrink and Implode, which moved to M19.
2. Each codec has hostile tests for truncation, a stream longer than declared and a wrong CRC, where its format carries a length or a CRC. `compress` (`.Z`) carries neither, and the stream reference says so.
3. The scoreboard gains a row per new codec, compared with 7-Zip at one thread. lzip and LZ4, which 7-Zip does not read, are compared with bsdtar.

Done so far, by 5 November 2026:

- ZIP Deflate64, BZip2, LZMA, Zstandard, xz and PPMd.
- 7z BZip2, Deflate64, PPMd, BCJ2 with its own decoder, and Swap2 and Swap4.
- Tar and single files in bzip2, lzip, LZ4 with its own decoder, and `compress` with its own decoder.

The scoreboard rows came last, and M13 closed. ZIP's Shrink and Implode moved to M19 on 5 November 2026. No current tool writes them, and how Shrink's partial clear treats the code read just before it has to be settled by tests against 7-Zip first. A Shrink decoder and its tests were kept as a starting point, and [M19](m19-legacy-formats.md) now reads Shrink and Implode in process. The speed gaps M13 recorded have moved since, as the [roadmap](../roadmap.md#milestones) says.
