# M9. Native 7z reader

`zipper extract` reads `.7z` in process, so 7-Zip is no longer needed for the archives applications most often ship.

Scope:

- A 7z container parser with the M1 safety contract, covering the header, packed streams, folders and substreams. Done in the [7z reader reference](../reference/7z-reader.md).
- LZMA and LZMA2 through liblzma, plus the BCJ x86 and ARM64 filters. Other codecs and encrypted archives are refused with a clear error. Done, with Copy, Deflate, Delta and every liblzma branch filter. The default engine sends the rest to 7-Zip.
- The M8 options, including selected folders, progress and cancellation. Done.
- A reference corpus from 7-Zip and libarchive, and hostile 7z tests. Done in `tests/sevenz_corpus.rs`, `tests/hostile_7z.rs` and `tests/sevenz_fuzz.rs`.

Exit checks:

1. The default engine extracts every corpus 7z archive in process on all three operating systems, with the same tree as 7-Zip and bsdtar.
2. Every hostile 7z layout has a test that failed before its fix, and the adversarial review finds no escape.
3. `docs/reference/` documents the 7z reader.

Since M9 closed, M21 replaced liblzma with Zipper's own LZMA, LZMA2 and xz decoder. M13 added BZip2, PPMd and BCJ2, M14 encrypted archives and M15 split ones, so the default engine no longer sends any of them to 7-Zip. Their rows in the [parity matrix](../reference/parity.md) read native. [M20](m20-zipper-without-7-zip.md) then removed the 7-Zip backend, so a 7z method the reader does not decode is refused by name.
