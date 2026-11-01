# Matched-sync application comparison on 2026-10-29

With the same file-sync operation for every engine, Zipper creates random data at 1479 MiB/s versus 1560–1565 MiB/s for the tar pipelines. The creation ranges overlap, so that difference is not established. The old roughly 3.5-fold gap was not an equivalent-sync comparison.

## Question

How do Zipper, tar+zstd, and tar+pzstd compare when every engine uses the same archive-file sync operation?

The earlier tar comparisons understated creation cost on macOS. Rust 1.96 `File::sync_all` calls `fcntl(F_FULLFSYNC)`, whereas Python `os.fsync` calls ordinary fsync. See the [pinned Rust source](https://github.com/rust-lang/rust/blob/1.96.0/library/std/src/sys/fs/unix.rs#L1303-L1313).

## Setup

The comparison script now uses the same file-sync operation for all engines. Zipper itself has not been made less durable.

- Apple M5, 32 GiB RAM, macOS 26.7.1.
- Engine `4d9dddd`, seven measured interleaved rounds plus one warmup.
- Four requested codec workers, level 1.
- Every extraction passed SHA-256 content and tree verification.

The fixtures and engine revisions are described in the [corpus and revision report](README.md). The [raw samples](zstd-fullsync.json) hold every measurement.

## Results

| Corpus | Engine | Compression MiB/s | Extraction MiB/s | Archive bytes |
| --- | --- | ---: | ---: | ---: |
| logs | zipper | 3050.2 | 6992.0 | 48,967,453 |
| logs | tar+zstd | 2723.3 | 1810.2 | 49,059,368 |
| logs | tar+pzstd | 2766.8 | 2034.1 | 49,040,393 |
| binaries | zipper | 2891.6 | 5981.7 | 77,270,195 |
| binaries | tar+zstd | 2456.3 | 1588.2 | 76,345,795 |
| binaries | tar+pzstd | 2433.4 | 1996.9 | 76,659,942 |
| random | zipper | 1478.5 | 7470.2 | 268,436,635 |
| random | tar+zstd | 1560.4 | 1896.5 | 268,442,077 |
| random | tar+pzstd | 1564.7 | 1877.1 | 268,445,289 |
| small-files | zipper | 200.0 | 89.0 | 35,013 |
| small-files | tar+zstd | 14.0 | 15.5 | 112,224 |
| small-files | tar+pzstd | 13.8 | 15.5 | 118,539 |

| Corpus | Engine | Compression range (ms) | Extraction range (ms) |
| --- | --- | ---: | ---: |
| logs | zipper | 82.4–85.4 | 35.9–36.9 |
| logs | tar+zstd | 93.3–94.9 | 140.4–163.1 |
| logs | tar+pzstd | 91.0–93.8 | 123.8–128.1 |
| binaries | zipper | 75.8–78.8 | 36.5–38.8 |
| binaries | tar+zstd | 89.9–99.1 | 137.2–140.9 |
| binaries | tar+pzstd | 90.8–106.0 | 108.3–113.6 |
| random | zipper | 167.0–246.4 | 33.5–37.0 |
| random | tar+zstd | 152.9–225.3 | 131.8–145.9 |
| random | tar+pzstd | 160.4–209.6 | 134.0–186.8 |
| small-files | zipper | 76.5–79.2 | 174.0–181.0 |
| small-files | tar+zstd | 1107.7–1134.7 | 1003.6–1021.3 |
| small-files | tar+pzstd | 1105.4–1148.4 | 1003.6–1041.0 |

## Decision

This correction changes measurement validity, not engine performance. Random-data creation took 167.0–246.4 ms for Zipper and 152.9–225.3 ms for tar+zstd, so the ranges overlap and no difference is established. The remaining random-data creation differences need longer runs and profiling before any optimization.

## Limits

The earlier caveats still apply:

- Tar preserves more metadata. The tar commands used default bsdtar flags, which archive and restore extended attributes. Files created on this Mac carry `com.apple.provenance`, so tar extraction likely restored that attribute on each file. Zipper restores no extended attributes. The cost of that extra work was not measured.
- The pipelines use different total thread counts and frame and window settings.
- Caches are warm, and storage and thermals are not isolated.
- Extraction is not explicitly synced.
- Publication and directory persistence are not equal crash-durability guarantees.
- No process-tree RSS is recorded.

This is an application comparison on development fixtures. It is not evidence of a faster underlying codec or of universal leadership.

## Reproduce

Run `python3 scripts/compare_zstd.py /tmp/zipper-corpus --runs 7 --threads 4` against the corpus from the [v2 report](README.md#reproduce).
