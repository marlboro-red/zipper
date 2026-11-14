# M35. Zstandard and LZMA at C speed on x86-64

Zipper's Zstandard and LZMA decoders decode as fast as libzstd and liblzma on x86-64 Linux, so that both can become crates under [M31](m31-a-release-case-for-each-decoder.md)'s rule.

M31 found both decoders the fastest pure-Rust option in every run, and ahead of their C libraries on an Apple M2 Max. On x86-64 Linux they trail. Zstandard takes 1.34 times libzstd's time, and LZMA and xz take 1.09 to 1.20 times liblzma's, on both corpora, as the [Zstandard record](../../benchmarks/decoders/zstandard/README.md) and the [LZMA and xz record](../../benchmarks/decoders/lzma-and-xz/README.md) show. On 14 November 2026 the user marked both "not ready yet" rather than "stays inside Zipper", and asked for this milestone.

Scope:

- Profile each decoder on x86-64 Linux against its C library, outside timed runs, before any change. Name the limiter for each, such as the bit reader, the literal and match loops, the Huffman or FSE tables, the match copy or the window. A hosted runner or any x86-64 Linux machine will do, with `perf` where it runs.
- Compare code generation on x86-64 with the M2 Max, where Zipper leads, to find what differs.
- Change only in safe Rust, as AGENTS.md and the pure-Rust rule require, with no assembly and no C. Keep every Kani proof, hostile test and differential test, and the M2 Max lead.
- Measure each kept change with the M31 driver, `scripts/m31_decoders.py`, on both corpora and both machines.

Exit checks:

1. A record for each decoder shows it level with or ahead of its C library on x86-64 Linux, with ranges apart or overlapping, on both corpora, with the M2 Max lead kept.
2. The M31 verdicts are re-judged with the new records. LZMA becoming a crate starts [M23](m23-the-lzma-decoder-as-its-own-crate.md).
3. If a gap cannot be closed in safe Rust, a record names the limiter with a profile, and the verdict stays "not ready yet" with that reason.
