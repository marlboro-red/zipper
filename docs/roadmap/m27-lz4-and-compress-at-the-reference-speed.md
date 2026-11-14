# M27. LZ4 and `compress` at the reference speed

Zipper extracts `.tar.lz4` and `.tar.Z` as fast as the fastest reference tool, in Rust and with every check it has today.

The codec rows of [Results](../results.md) at `cb8fe53`, on an Apple M2 Max at one thread, show the two largest gaps against a reference. A Silesia `.tar.lz4` takes 335 ms in Zipper against 102 ms in bsdtar, 3.3 times as long. A Silesia `.tar.Z` takes 1,120 ms against 413 ms in 7-Zip, 2.7 times as long. Both gaps predate M25, and M13 recorded them first.

Scope:

- Profile `src/lz4.rs`, `src/lzw.rs` and the tar reader's path for each before changing code, and name what limits each one.
- Study how the reference decodes each format, by its source or by disassembly, and measure the gap stage by stage.
- Keep each change that a record shows faster with ranges apart, in safe Rust where a measured gain does not need `unsafe`.

Exit checks:

1. A record for each codec times Zipper against its reference on the second machine, interleaved, with the ranges apart or the verdict stated as a tie.
2. The codec rows of the scoreboard show each codec level with its reference or faster, or the record names the gap left and why only assembly could close it.
3. The LZ4 and `compress` parser proofs and tests still pass, and a planted break shows each new check can fail.

Status:

| Codec | Zipper against its reference | Record |
| --- | --- | --- |
| LZ4 | 1.09× bsdtar on Silesia `.tar.lz4` at one thread, 114.8 ms against 105.3 ms. bsdtar decodes in a second `lz4` process. Exit checks 1 and 3 are met for LZ4. | [LZ4 block decoder at the reference speed](../../benchmarks/lz4/block-decoder/README.md) |
| `compress` | 0.64× 7-Zip 26.06 on Silesia `.tar.Z` at one thread, 269.8 ms against 419.0 ms, with the ranges apart. 7-Zip decodes in one process and reads the tar in a second. Exit checks 1 and 3 are met for `compress`. | [`compress` decoder ahead of 7-Zip](../../benchmarks/compress/lzw-decoder/README.md) |

`compress` now leads 7-Zip. LZ4 is at 1.09× bsdtar, which decodes in a second `lz4` process, and the LZ4 record names the xxHash32 checksum as the main cost left. Exit check 2 stays open until `cargo xtask bench` remeasures the codec rows of [Results](../results.md), which still show the times at `cb8fe53`.
