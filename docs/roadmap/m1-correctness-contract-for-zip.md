# M1. Correctness contract for ZIP

The native reader extracts the same bytes as a reference extractor and survives hostile archives.

Scope:

- A reference comparison that extracts each corpus archive with Zipper, bsdtar and 7-Zip and compares the trees. Done in the [reference corpus](../reference/zip-reader.md#reference-corpus), which still lacks Java `jar` output.
- A corpus of real producer output. It covers Java `jar`, Windows Explorer, macOS Archive Utility, Info-ZIP `zip`, Python `zipfile` and 7-Zip. It includes ZIP64 by size and by entry count, data descriptors and archive comments.
- A hostile collection with a test for each case. It covers path traversal, absolute paths, drive letters, duplicate names, names that collide by case, overlapping entries, a zero declared size hiding real data, local and central headers that disagree, truncation and inconsistent ZIP64 fields.
- Fuzz targets for the central-directory and local-header parsers. Done as [mutation testing](../reference/zip-reader.md#mutation-testing) on stable Rust. Coverage-guided fuzzing still needs a nightly toolchain.
- Local headers that agree with the central directory, and records that do not overlap. Done in the [local-header check](../../benchmarks/native-zip/local-header-check/README.md).
- Unicode path extra fields (`0x7075`) that rename an entry are rejected, so every reader uses the same name. Done in the [Unicode path field check](../../benchmarks/native-zip/unicode-path/README.md).
- Records that tile the archive, with checked data descriptors, the local encryption flag and local ZIP64 sizes. Done in the [record layout check](../../benchmarks/native-zip/record-layout/README.md).
- Deflate streams that end exactly at their compressed size, and stored entries that cannot hide a descriptor. Done in [exact decoding](../../benchmarks/native-zip/exact-decoding/README.md).
- A limit on total extracted bytes, with a documented default. Done. Output must fit in free space unless `--max-extracted-bytes` sets a cap.

Exit checks:

1. The reference comparison finds no byte mismatch on any corpus archive.
2. Every hostile case is rejected or stays inside the destination, and its test fails if the check is removed.
3. A short fuzz run executes in `cargo xtask gate fast` and `gate full`. Longer runs find no crash.
4. Each parser change passed the `zipper-review` skill.

Closed on 1 November 2026:

- `3af1bea` set M1 to done. For exit check 3, the gate runs 1,000 mutation cases, and a run of 300,000 cases on 2026-11-01 found no failure, as [mutation testing](../reference/zip-reader.md#mutation-testing) records. That is random mutation, not coverage-guided fuzzing.
- The page records no proof of exit checks 2 and 4 beyond the records linked in its scope. The reference corpus still lacks Java `jar` output.
