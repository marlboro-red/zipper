# M34. Decoder cases on Windows

Each decoder that [M31](m31-a-release-case-for-each-decoder.md) judged is measured again on Windows, against the same alternatives, so that every verdict holds on the three operating systems Zipper ships for.

M31 measured an Apple M2 Max and an x86-64 Linux runner. Windows is where most users run archive tools, and its file system, allocator and process start cost differ from both. A verdict that becomes a crate should not rest on two of three platforms.

Scope:

- Run the decode bench in `benchmarks/decoders/decode-bench` on a hosted Windows x86-64 runner, against every alternative M31 names. Under the user's rule of 14 November 2026, those are both pure-Rust crates and crates that wrap C libraries. Decoders without a Windows build are named, not skipped silently.
- Run the extraction rows there, with Zipper at one thread and at all threads, against 7-Zip and the `tar.exe` that Windows ships, which is bsdtar.
- Use both M31 corpora, Silesia and the dependency sources, built to the same sha256 as on the other two machines.
- Add a Windows job, or a second workflow, beside `.github/workflows/m31-decoders.yml`. It runs only on `workflow_dispatch` and uploads its JSON even when a row fails.
- Re-judge each M31 verdict with the Windows rows. A verdict changes when Windows reverses or overlaps a lead.

Exit checks:

1. Each M31 record has decode and extraction rows from a Windows x86-64 runner, on both corpora, against every alternative, with ranges.
2. Each verdict on the M31 page names whether Windows confirms it, overlaps it or reverses it.
3. No claim of leadership rests on fewer than the three operating systems.
