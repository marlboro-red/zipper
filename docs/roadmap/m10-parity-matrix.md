# M10. Parity matrix

One generated table shows what Zipper extracts natively, what it hands to 7-Zip and what it cannot extract. Each of M11 to M20 moves rows in it.

Scope:

- A script lists every format, method and filter that `7z i` reports, and every `7z x` switch that changes extraction.
- Each row records native, backend or missing, with the test or corpus archive that proves it.
- A corpus archive for each format, written by 7-Zip or the format's own tool, with its tree as 7-Zip extracts it.
- The table lives in `docs/reference/` and is generated, like `docs/results.md`.

Exit checks:

1. A gate step regenerates the table and fails when it differs from the committed copy.
2. Every native and partial row names a proof that passes in CI. Every backend row with a corpus archive extracts it through 7-Zip in CI.
3. The table counts the rows with no proof yet. The milestone that moves such a row adds its proof.

[M20](m20-zipper-without-7-zip.md) removed the 7-Zip backend on 13 November 2026, so no row reads backend now. A row Zipper does not read is `missing`, `dropped` or `refused`.
