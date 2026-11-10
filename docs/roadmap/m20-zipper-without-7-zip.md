# M20. Zipper without 7-Zip

The default build extracts every format in the M10 matrix with no 7-Zip installed.

Scope:

- The 7-Zip backend becomes an optional build feature, then is removed when no row needs it.
- The Windows package ships without 7-Zip.
- `docs/usage.md`, the reader references and the Windows notes describe the result.

Exit checks:

1. The release checks run the full M10 corpus on all three operating systems, on machines with no 7-Zip installed.
2. The scoreboard compares every format with 7-Zip, with CPU time and memory.
