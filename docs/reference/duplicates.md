# Duplicate-content report

`zipper duplicates PATH...` reports groups of byte-identical regular files. Each
path can be a file or a directory.

| Option | Effect |
| --- | --- |
| `--summary` | Print only totals. |
| `--threads N` | Set the maximum number of hashing workers. |

```sh
zipper duplicates ./photos ./backup
zipper --threads 4 duplicates --summary ./data
```

## Matching

The checker groups files by length. It fingerprints the first 64 KiB of
same-size files. It hashes full contents only for the remaining candidates.

CRC32 is a filter, never proof of equality. A final byte-for-byte comparison
confirms every group. Unequal files with matching CRCs stay in separate groups.
The order of paths and groups is deterministic. The checker adds no hashing
dependency.

## Inputs

The checker canonicalizes overlapping inputs, so it counts each path once. It
skips symlinks and special files without following them, including symlink roots.

The report lists different hard-link paths as content duplicates. Duplicate
payload bytes are therefore **logical redundant content, not reclaimable disk space**.
The skipped-entry total counts traversal visits. Overlapping roots can repeat
those visits.

## Limits

- Inputs must stay unchanged during the scan. Length and modification-time
  checks detect many changes, but they do not provide a filesystem snapshot.
- A read failure aborts the report.
- Metadata memory grows with the number of paths, so very large trees need more
  of it. Payload buffers stay bounded per worker.
- Deliberately constructed CRC collisions can increase comparison work. They
  cannot make unequal bytes pass the final comparison.

The command is a read-only report. It does not change archive storage or remove
files. Native creation and extraction still handle each logical file.
Archive-level deduplication would require a separate, measured change to the
format and design.

## Duplicates in the performance fixtures

| Fixture | Files | Duplicate groups | Duplicate payload bytes |
| --- | ---: | ---: | ---: |
| Grouped small files | 4,000 | 0 | 0 |
| Flat tiny files | 10,000 | 1 (2,000 empty files) | 0 |
| Silesia | 12 | 0 | 0 |

Content deduplication cannot save payload decoding in these fixtures. Extraction
still has to create every required output file. Real input collections can
contain substantial repeated content. This command measures it.

## Test coverage

Tests cover:

- real CRC32 collisions
- differing tails after identical prefixes
- empty files
- hard links
- symlink loops
- overlapping inputs
- deterministic output
- unchanged source bytes

Independent review also checked escaped newline paths and equal output from one
and four workers.
