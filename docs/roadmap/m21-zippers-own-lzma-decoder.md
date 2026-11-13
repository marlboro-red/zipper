# M21. Zipper's own LZMA decoder

LZMA and LZMA2 decode through Zipper's own Rust code, at least as fast as 7-Zip at one thread.

A comparison on 5 November 2026 showed why. At one thread, Zipper takes 1.25× 7-Zip's time on solid Silesia, and that gap is liblzma against 7-Zip's own decoder. sevenz-rust2, whose decoder is written in Rust with inline assembly for one step on arm64 and x86-64, also decodes an LZMA setting (`lc=8`) that liblzma refuses, so Zipper sends that archive to 7-Zip. The parallel LZMA2 segments that put Zipper ahead of sevenz-rust2 cost 347 MiB against 7-Zip's 256 MiB at default threads.

Scope:

- A range decoder, LZMA and LZMA2 in safe Rust, with every `lc`, `lp` and `pb` the format allows, used by 7z, ZIP LZMA, `.lzma`, lzip and the parallel LZMA2 segments.
- The xz container, with its CRC-32, CRC-64 and SHA-256 checks.
- The branch filters (x86, ARM, ARM Thumb, ARM64, RISC-V, PowerPC, SPARC, IA-64) and Delta, so liblzma is no longer needed to read.
- The decoder's dictionary and buffers count against the shared decoder budget, and parallel segments hold no more memory than 7-Zip does at default threads.

Exit checks:

1. A differential test decodes every LZMA, LZMA2 and xz stream in the corpora with Zipper's decoder and with liblzma, and finds the same bytes or the same refusal. It also runs a pseudo-random set of streams built from them.
2. Kani proves the range decoder, the bit-tree and length decoders and the match copy never panic or read or write out of bounds, up to a bound. An exhaustive test covers what Kani cannot finish.
3. Each branch filter round-trips every short input under liblzma's encoder, and its hostile tests cover every truncation.
4. A benchmark record shows Zipper's decoder at least as fast as 7-Zip's at one thread on solid Silesia, with ranges apart, and peak memory at default threads no higher than 7-Zip's.
5. The 7z corpus archive with `lc=8` reads natively, and its parity row moves from 7-Zip to native.

Done so far, by 5 November 2026:

- A raw LZMA decoder in safe Rust, `src/lzma.rs`, used only by tests so far. It decodes 64 MiB of Silesia in 436.9 ms, against 400.9 ms for liblzma and about 310 ms for 7-Zip, as the [hot-loop record](../../benchmarks/lzma/hot-loop/README.md) shows.
- Differential tests against liblzma for every `lc`, `lp` and `pb`, every cut stream and an end marker before the declared size.
- An exhaustive test of the match copy in every window state. A Kani proof of it ran out of memory and was dropped for that test.
- An LZMA2 reader on the same decoder, also used only by tests, with the same bytes or the same refusal as liblzma for every control code, every properties byte and every cut. A builder agent wrote it, and its tests found a read that could return bytes a chunked match copy had just overrun, fixed in `a69a361`.
- An xz container reader on the same LZMA2 decoder, with the branch filters and Delta, also used only by tests so far. Kani proves the range coder and symbol decoders, and a reader fails every read after its first error.

Done by 7 November 2026:

- The decoder takes 0.878 times liblzma's time on the probe stream, as the [tree-preload record](../../benchmarks/lzma/tree-preload/README.md) shows.
- 7z decodes LZMA and LZMA2 with it, and the `lc=8` corpus archive reads natively. Silesia extracts 7 to 9% faster than with liblzma, and 7-Zip is still about 8% faster at one thread, as the [switch record](../../benchmarks/native-7z/own-decoder-shipped/README.md) shows. Exit check 4 is not met yet.
- 7z decodes its branch and Delta filters with `src/xz_filters.rs`, at unchanged speed, as the [filters record](../../benchmarks/native-7z/own-filters/README.md) shows. The 7z reader no longer uses liblzma outside tests.
- Parallel LZMA2 segments peak at 146 MiB on solid Silesia at default threads, against 257 MiB for 7-Zip, as the [segment memory record](../../benchmarks/native-7z/segment-memory/README.md) shows. That meets the memory half of exit check 4.
- Two changes from a machine-level study of 7-Zip's decoder made 7z Silesia extract about 3.5% faster, as the [kernel record](../../benchmarks/lzma/7zip-study-kernel/README.md) shows. 7-Zip is still about 11% faster at one thread, so exit check 4 is not met yet.
- tar, the ZIP XZ method and RPM decode xz with Zipper's own reader. Silesia `.tar.xz` extracts 11 to 15% faster than with liblzma, as the [xz record](../../benchmarks/xz/own-decoder/README.md) shows.
- `.lzma`, lzip, the ZIP LZMA method and RPM's LZMA payloads decode with Zipper's own readers, and liblzma is only a test dependency, as the [lzip and `.lzma` record](../../benchmarks/xz/lzip-lzma-zip/README.md) shows. The release build no longer links liblzma.

Decided, on 8 November 2026:

- Zipper stays pure Rust, with no hand-written assembly in the hot loop. The user decided this on 8 November 2026, after first allowing assembly the same morning. 7-Zip's arm64 loop is assembly, and every safe-Rust change tried left 7-Zip about 10% faster at one thread on solid Silesia, 1.335 against 1.484 s in the [unchecked tables record](../../benchmarks/lzma/unchecked-tables/README.md).
- A study of 7-Zip's arm64 `LzmaDecOpt.S` found two shortcuts that a port must not copy. Its eight direct bits after a normalization are exact only when the range is at least 2^31, and its sign-flag compares differ from an unsigned compare on damaged streams. Valid encoder output does not reach either case.

Closed on 8 November 2026:

- M21 closed on 8 November 2026, by the user's decision. Exit checks 1, 2, 3 and 5 are met, and so is the memory half of exit check 4. The speed half is not met. At one thread on solid Silesia, 7-Zip takes 1.335 s and Zipper's safe decoder at `46a8df0` takes 1.484 s, in one rotation of the [unchecked tables record](../../benchmarks/lzma/unchecked-tables/README.md). The remaining gap is 7-Zip's hand-written arm64 loop, which the pure Rust decision rules out. Zipper's decoder is still faster than liblzma, which it replaced.
