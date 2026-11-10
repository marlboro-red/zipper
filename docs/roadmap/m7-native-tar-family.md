# M7. Native tar family

`zipper extract` reads tar and compressed tar in process, so the archives most people extract on macOS and Linux no longer need 7-Zip.

Scope:

- A streaming tar reader with the M1 safety contract and the M2 metadata contract, applied to tar entries. Done in the [tar reader reference](../reference/tar-reader.md).
- Decompression for gzip, zstd and xz in front of it, so `.tar.gz`, `.tgz`, `.tar.zst` and `.tar.xz` extract their files and not an inner `.tar`. Done.
- POSIX ustar, pax and GNU headers, plus old tar without the `ustar` marker. Done, except GNU sparse files.
- A reference corpus from GNU tar, bsdtar and Python `tarfile`. Done in the [tar reference corpus](../reference/tar-reader.md#reference-corpus).
- Hostile tar tests for traversal, absolute names, links that escape, hard links, special files, duplicate names and truncation. Done in `tests/hostile_tar.rs`, after an adversarial review.
- Scoreboard rows for the tar workloads against bsdtar. Done in [Results](../results.md) for macOS, and in the [Linux and Windows tar rows](../../benchmarks/cross-platform/tar-scoreboard/README.md).

Exit checks:

1. The default engine extracts every corpus tar format in process on all three operating systems, with the same tree as the reference readers.
2. Every hostile tar layout has a test that failed before its fix, and the adversarial review finds no escape.
3. `docs/reference/` documents the tar reader, and the scoreboard shows its tar rows.
