# M31. A release case for each decoder

Each of Zipper's own decoders is measured against the Rust crates users already have, on more than one kind of machine. A decoder becomes its own crate only where that measurement shows a clear lead.

Zipper's decoders were measured against C libraries, such as libdeflate, libzstd and zlib, almost only on Apple Silicon. A separate crate adds value only if it beats what a Rust program can use today, and only where the lead holds on the machines most programs run on.

Scope:

- Compare each decoder with its Rust alternatives. Zstandard goes against `ruzstd` and libzstd, and Deflate and gzip against `zlib-rs`, `miniz_oxide` and libdeflate. LZMA and xz go against `lzma-rs`, `lzma-rust2` and liblzma. BZip2 goes against `libbz2-rs-sys` and libbzip2, and XPRESS against any Rust crate that reads it. The LZX, LZMS, LZFSE and LZVN decoders were removed on 12 November 2026 for licence reasons, as [M32](m32-readers-removed-for-licence-reasons.md) records.
- Measure decode alone in one process, and end-to-end extraction, on the second machine, an Apple M2 Max, and on an x86-64 Linux CI runner.
- Record each decoder's case as one of three verdicts. It becomes a crate, stays inside Zipper, or is not ready yet. Name the lead and its spread.
- M23, the LZMA decoder as its own crate, starts only if LZMA's verdict is a crate.

Exit checks:

1. A record for each decoder above gives its decode and extraction numbers on both machines, against every alternative it names, with ranges.
2. Each record ends in a verdict, and this page lists them.
3. No claim of leadership rests on one machine or one corpus.
