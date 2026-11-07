# ZIP listing with Zipper's own directory reader

**Kept. Listing a ZIP with Zipper's own central-directory reader takes the same time as with the `zip` crate on six archives, with every range overlapping, and it lists a 100,000-entry archive the old path refused.** The release build no longer depends on the `zip` crate. One Mac, one session, and archives of at most about 2,000 entries for the timed comparison.

## Question

M22 replaced the `zip` crate's `ZipArchive` with the reader in `src/zip_directory.rs` for every listing in the release build. Its last exit check asks that listing get no slower. Does it? The metric is the median wall time of `zipper list` on one archive.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0, release builds. macOS background services were active, as in the [kernel record](../../lzma/7zip-study-kernel/README.md).
- `before` is `4077983`, the last commit before the reader was wired in at `4a37c04`, binary SHA-256 prefix `c625fe5933cdcecc`. `after` is `9802d0b`, which lists through the new reader, prefix `fd005fc8ea80d058`.
- Archives:
  - the five largest ZIP, wheel and jar files of the real-world set in `target/realworld/archives`, 3,914,904 to 17,001,609 bytes, with 120 to 2,019 entries;
  - a ZIP of 2,020 tiny files and folders from Info-ZIP `zip -1`, 356,732 bytes;
  - a ZIP of 100,000 tiny files and 1,000 folders, 17,837,117 bytes.
- A small loop ran `zipper list` on each archive with the two binaries in alternating order, for eight or ten rounds with the first as warmup. It checked that each run succeeded and printed the same number of lines.

## Results

Median wall time in milliseconds, with the range. Raw samples are in the JSON files beside this page.

| Archive | Entries | Runs | `before` | `after` |
| --- | ---: | ---: | --- | --- |
| `caeb872d3f6eae10` wheel | 1,045 | 7 | 5.3 (4.0 to 12.1) | 4.9 (3.9 to 13.8) |
| `710ff00c67ba0417` wheel | 2,019 | 7 | 4.8 (4.6 to 4.9) | 4.6 (4.5 to 4.7) |
| `9155d6c45d78831b` wheel | 1,720 | 7 | 4.3 (4.1 to 4.5) | 4.2 (4.1 to 4.4) |
| `a97951b4da3a7700` zip | 487 | 7 | 3.2 (3.0 to 3.4) | 3.3 (3.1 to 3.3) |
| `e5c786b1047f146d` wheel | 120 | 7 | 2.9 (2.7 to 2.9) | 2.8 (2.7 to 2.9) |
| 2,020 tiny files | 2,020 | 9 | 4.9 (4.7 to 5.2) | 4.6 (4.4 to 4.9) |

On the 100,000-file archive, `before` refused with "ZIP metadata limit exceeded" and `after` listed every entry, so no time compares.

## Decision

Kept. Every range overlaps, so listing is no slower, which closes M22's last exit check. The reader also lists archives the old path refused within the documented limits, and its builder measured peak memory of `zipper list` falling from 68.8 MB to 2.1 MB on 1,015 entries with 65,524-byte extra fields.

## Limits

- One machine and one session, with background services active. Linux and Windows were not measured.
- `after` is `9802d0b`. Main has since refused one empty ZIP64 layout, a check on archives with no entries that this workload never reaches.
- Each listing takes a few milliseconds, so process start-up is much of the time. Archives with tens of thousands of entries that both paths accept were not timed.

## Reproduce

Build release binaries at `4077983` and `9802d0b`. Then run `zipper list` on each archive with the two binaries in alternating order, as Setup describes, and compare the medians.
