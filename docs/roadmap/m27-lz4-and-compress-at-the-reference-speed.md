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
| LZ4 | Level with bsdtar on Silesia `.tar.lz4` at one thread, 110.3 ms against 113.1 ms over 21 rounds with the ranges overlapping, after the sniff stopped decoding the whole first block. An earlier session gave 1.09× bsdtar. bsdtar decodes in a second `lz4` process and uses 0.207 s of CPU against Zipper's 0.126 s. Exit checks 1 and 3 are met for LZ4. | [LZ4 block decoder at the reference speed](../../benchmarks/lz4/block-decoder/README.md) |
| `compress` | 0.64× 7-Zip 26.06 on Silesia `.tar.Z` at one thread, 269.8 ms against 419.0 ms, with the ranges apart. 7-Zip decodes in one process and reads the tar in a second. Exit checks 1 and 3 are met for `compress`. | [`compress` decoder ahead of 7-Zip](../../benchmarks/compress/lzw-decoder/README.md) |

`compress` now leads 7-Zip. LZ4 is level with bsdtar, which decodes in a second `lz4` process, and the LZ4 record names the xxHash32 checksum as the main cost left. ## Closed on 14 November 2026

- Exit check 1 passes. The [LZ4 record](../../benchmarks/lz4/block-decoder/README.md) times Zipper against bsdtar, interleaved, and states a tie. The [`compress` record](../../benchmarks/compress/lzw-decoder/README.md) times Zipper against 7-Zip with the ranges apart.
- Exit check 2 passes. The codec rows of [Results](../results.md), remeasured at `ba8df332`, show `tar-lz4` at 112.0 ms against bsdtar's 114.9 ms, 1.03 times, with the ranges overlapping. `tar-compress` takes 268.4 ms against 7-Zip's 415.7 ms, 1.55 times. At `cb8fe53` they stood at 0.31 and 0.37 times.
- Exit check 3 passes for both codecs, as the status table above records. The gate's `verify` step passes at `ba8df332`, so the LZ4 and `compress` parser proofs are in step with their code.
- LZ4 is level, not ahead. bsdtar decodes in a second `lz4` process and spends 208 ms of CPU against Zipper's 128 ms. A content checksum on a second thread could take the lead. Its design is written but not reviewed or built.
