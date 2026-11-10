# M25. A release build without C

The release build compiles no C library. Every decoder that reads archive bytes is Rust, so the checks and proofs cover all of it.

When M25 began, hostile archive bytes reached three C libraries. libdeflate decoded Deflate in the opt-in `native-fast` engine. On macOS, the system zlib decoded streaming Deflate. zstd decodes ZIP Zstandard entries, `.zst` files and SquashFS. Linux and Windows decoded Deflate with zlib-rs.

Scope:

- Retire `native-fast`. The scoreboard shows its rows overlapping `native` on every workload, with up to 30 times the memory on large entries. `--zip-engine native-fast`, `Engine::NativeFast`, `--zip-buffer-mib` and `zip_buffer_bytes` are removed, since Zipper has no release yet.
- Time zlib-rs against the system zlib on macOS, and move to zlib-rs if the ranges overlap.
- Time a pure-Rust zstd decoder against libzstd. If it is slower, profile and optimize it as the LZMA decoder was, until only a gap that assembly could close remains.

Exit checks:

1. `cargo tree -e normal` lists no `-sys` crate that compiles C, on macOS, Linux and Windows.
2. A record for each library compares the Rust replacement with it, with ranges apart or the verdict stated as a tie.
3. The codec rows and the scoreboard show no workload slower beyond the spread, or the record names the gap and why it is accepted.

Done so far, by 11 November 2026:

- `native-fast` is retired, and with it libdeflate, its `unsafe` calls, the shared entry-buffer budget and the 8 MiB archive cache. An adversarial review found no check weakened on the remaining engine, and both ZIP models held every rule after the change.
- Zipper's own Deflate decoder in `src/inflate.rs` reads every Deflate, zlib and gzip stream, in ZIP, 7z, tar and single `.gz` files, CAB, DMG, HFS+, APFS, SquashFS, CramFS, xar, SWF, QCOW and VMDK. `flate2`, zlib-rs and the system zlib left the normal dependencies. The release build step of the gate fails if a zlib crate or library comes back.
- One C library remains in `cargo tree -e normal`. zstd builds on every platform.
