# M2. Metadata fidelity

The native reader restores what users expect from a ZIP extractor.

Scope:

- Unix permission bits for entries made on Unix, masked to `0o777`. Done in [permission restore](../../benchmarks/native-zip/permissions/README.md).
- Modification times from the DOS fields and the extended timestamp field. Directory times are set after their children. Done in [timestamp restore](../../benchmarks/native-zip/timestamps/README.md).
- A symlink policy, decided and documented before implementation. The [ZIP reader reference](../reference/zip-reader.md#symlinks) records the decision, which is to create only links that stay inside the destination.
- Filename decoding for UTF-8 and CP437, and a Unicode path field that spells a CP437 name in UTF-8. Done in the [ZIP reader reference](../reference/zip-reader.md#supported-archives). No three reference readers agree on a CP437 name on macOS, so the corpus cannot hold one, and `tests/hostile_zip.rs` covers it.
- A Unicode normalization policy. Done in the [ZIP reader reference](../reference/zip-reader.md#name-normalization). Zipper keeps stored name bytes, as `unzip`, Python and 7-Zip do.

Exit checks:

1. The reference comparison also compares permissions and modification times.
2. `docs/reference/zip-reader.md` lists exactly what is restored and what is not.
3. Benchmarks compare against default bsdtar, because both sides now do the same metadata work. Done in `scripts/extraction_matrix.py`. The [bsdtar comparison](../../benchmarks/native-zip/bsdtar-comparison.md) predates metadata restore, so M3 measures it again.
