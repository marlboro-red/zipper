# M34. Decoder cases on Windows

Each decoder that [M31](m31-a-release-case-for-each-decoder.md) judged is measured again on Windows, against the same alternatives, so that every verdict holds on the three operating systems Zipper ships for.

M31 measured an Apple M2 Max and an x86-64 Linux runner. Windows is where most users run archive tools, and its file system, allocator and process start cost differ from both. A verdict that becomes a crate should not rest on two of three platforms.

Scope:

- Run the decode bench in `benchmarks/decoders/decode-bench` on a Windows 11 desktop, AMD Ryzen 5 5600X, 6 cores and 12 threads, against every alternative M31 names. The user chose that machine over a hosted runner. Under the user's rule of 14 November 2026, those are both pure-Rust crates and crates that wrap C libraries. Decoders without a Windows build are named, not skipped silently.
- Run the extraction rows there, with Zipper at one thread and at all threads, against 7-Zip and the `tar.exe` that Windows ships, which is bsdtar.
- Use both M31 corpora, Silesia and the dependency sources, built to the same sha256 as on the other two machines.
- Run `scripts/m31_decoders.py` there by hand, since the machine is not a runner. It keeps a failed row in the JSON, as on the other machines.
- Re-judge each M31 verdict with the Windows rows. A verdict changes when Windows reverses or overlaps a lead.

Exit checks:

1. Each M31 record has decode and extraction rows from a Windows x86-64 runner, on both corpora, against every alternative, with ranges.
2. Each verdict on the M31 page names whether Windows confirms it, overlaps it or reverses it.
3. No claim of leadership rests on fewer than the three operating systems.

## Measured on 14 November 2026

The machine is a Windows 11 desktop, AMD Ryzen 5 5600X, 6 cores and 12 threads. The decode rows ran at `5e6dd97b`, with the C libraries built by the MSVC Build Tools. The extraction rows ran the same binaries with the driver at `3b324db6`, which pipes 7-Zip's output correctly on Windows. The XPRESS WIM rows ran the same way, with WIMs that wimlib 1.14.5 wrote. The dependencies rows ran again with the same binaries at `c16c47c1`, whose `corpus` writes the same tar on every system. bsdtar is the `tar.exe` Windows ships, libarchive 3.8.9, and 7-Zip is 26.05 x64. No process was busy in any run, and no row failed.

- Each [M31 record](m31-a-release-case-for-each-decoder.md#verdicts) has a Windows section for each corpus, with decode rows against every alternative and extraction rows against bsdtar and 7-Zip, all with ranges.
- The M31 verdict table names whether Windows confirms, overlaps or reverses each verdict. It confirms Deflate, BZip2 and the XPRESS decode lead. It overlaps Zstandard, which is level with libzstd, and reverses the Linux gap for LZMA, which leads liblzma. No verdict changes.
- BZip2 and XPRESS, the two crate verdicts, lead their best alternative on all three operating systems, so exit check 3 holds.
- On the dependencies corpus every reader takes 4.1 to 7.4 s, so file creation on NTFS dominates. Zipper is fastest in every row. On Silesia Zipper beats bsdtar in every row. 7-Zip is faster on `.tar.xz`, 1,312.7 against 1,486.0 ms, and on the WIM, which Zipper extracts in 1.400 times its time, as on the other two systems.

## Closed on 14 November 2026

- Exit check 1 passes. Every M31 record has decode and extraction rows from the Windows desktop on both corpora, against every alternative, with ranges. XPRESS has a WIM row on both.
- Exit check 2 passes. The M31 verdict table names for each decoder whether Windows confirms, overlaps or reverses its verdict.
- Exit check 3 passes. BZip2 and XPRESS, the two crate verdicts, lead their best alternative with ranges apart on macOS, Linux and Windows.
- Each tar and every stream now has the same sha256 on all three machines. The first Windows `dependencies.tar` listed mixed-case names in another order, since Windows sorts paths without regard to case. `c16c47c1` sorts by each path's parts. The rerun on that tar moved no ratio by more than 0.02 and changed no verdict.
- `2eccda4c` added the first Windows rows, and `c16c47c1` added the WIM rows and the sort fix.
- The machine is a desktop the user owns, not a hosted runner, so Windows rows compare only within that machine.
