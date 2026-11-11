# M20. Zipper without 7-Zip

The default build extracts every format in the M10 matrix with no 7-Zip installed.

Scope:

- The 7-Zip backend becomes an optional build feature, then is removed when no row needs it.
- Once Zipper reads every row, the `--zip-engine` argument and `Engine` are removed too, since `auto` and `native` then mean the same thing and nothing is left for `backend` to do.
- The Windows package ships without 7-Zip.
- `docs/usage.md`, the reader references and the Windows notes describe the result.

Exit checks:

1. The release checks run the full M10 corpus on all three operating systems, on machines with no 7-Zip installed.
2. The scoreboard compares every format with 7-Zip, with CPU time and memory.

On 12 November 2026 the user decided that 7-Zip parity is no longer a goal until the user sets it again. The rows [M32](m32-readers-removed-for-licence-reasons.md) refuses for licence reasons need no reader here, and they never reach 7-Zip.
