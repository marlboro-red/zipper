# Duplicate-content checking

`zipper duplicates PATH...` reports groups of byte-identical regular files.
Paths may be files or directories. Add `--summary` to print only totals, and
`--threads N` to choose the maximum hashing-worker count.

```sh
zipper duplicates ./photos ./backup
zipper --threads 4 duplicates --summary ./data
```

The checker groups by length, fingerprints the first 64 KiB of same-size files,
and hashes full contents only for remaining candidates. CRC32 is a filter,
never proof of equality: a final byte-for-byte comparison confirms every group.
Unequal files with matching CRCs are kept separate. Path/group ordering is
deterministic. No new hashing dependency is required.

Overlapping inputs are canonicalized so the same path is counted once. Symlinks
and special files are skipped without following them, including symlink roots.
Different hard-link paths are reported as content duplicates. Thus duplicate
payload bytes are **logical redundant content, not reclaimable disk space**.
The skipped-entry total counts traversal visits, which can repeat with
overlapping roots.

Inputs must remain unchanged during scanning. Length and modification-time
checks detect many changes, but do not provide a filesystem snapshot. Read
failures abort the report. Very large trees require metadata memory proportional
to the number of paths; payload buffers remain bounded per worker. Deliberately
constructed CRC collisions can increase comparison work, but cannot make unequal
bytes pass the final comparison.

This is a read-only report. It does not change archive storage or remove files.
Native creation/extraction still handles each logical file; archive-level
deduplication would require a separate measured format/design change.

## What the current performance fixtures contain

| Fixture | Files | Duplicate groups | Duplicate payload bytes |
| --- | ---: | ---: | ---: |
| Grouped small files | 4,000 | 0 | 0 |
| Flat tiny files | 10,000 | 1 (2,000 empty files) | 0 |
| Silesia | 12 | 0 | 0 |

Content deduplication cannot save payload decoding in these fixtures. Every
required output filename still has to be created. Real input collections may
contain substantial repeated content; this command makes that measurable.

Tests cover real CRC32 collisions, differing tails after identical prefixes,
empty files, hard links, symlink loops, overlapping inputs, deterministic output,
and unchanged source bytes. Independent review additionally checked escaped
newline paths and one/four-worker output equivalence.
