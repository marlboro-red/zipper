# V2 application comparison — 2026-10-29

Current v2 with directory deduplication, Apple M5, 32 GiB RAM, macOS 26.7.1. Five measured rounds plus one warmup, seeded interleaved ordering, four requested codec workers, Zstandard level 1. Every round trip passed SHA-256 file-content and tree-manifest verification. [Raw samples](zstd-comparison.json).

| Corpus | Engine | Compression MiB/s | Extraction MiB/s | Archive bytes |
| --- | --- | ---: | ---: | ---: |
| logs | zipper | 3050.7 | 7143.2 | 48,967,453 |
| logs | tar+zstd | 2906.3 | 1796.3 | 49,059,368 |
| logs | tar+pzstd | 2982.9 | 2035.7 | 49,040,393 |
| binaries | zipper | 2868.9 | 5980.4 | 77,270,195 |
| binaries | tar+zstd | 2651.0 | 1608.2 | 76,345,795 |
| binaries | tar+pzstd | 2693.1 | 2059.3 | 76,659,942 |
| random | zipper | 1178.9 | 7023.4 | 268,436,635 |
| random | tar+zstd | 4125.4 | 2105.4 | 268,442,077 |
| random | tar+pzstd | 4137.0 | 2064.5 | 268,445,289 |
| small-files | zipper | 196.6 | 76.4 | 35,013 |
| small-files | tar+zstd | 14.0 | 15.3 | 112,224 |
| small-files | tar+pzstd | 13.7 | 15.2 | 118,539 |

| Corpus | Engine | Compression range (ms) | Extraction range (ms) |
| --- | --- | ---: | ---: |
| logs | zipper | 83.2–85.7 | 35.8–78.5 |
| logs | tar+zstd | 87.6–89.0 | 140.3–144.3 |
| logs | tar+pzstd | 85.5–87.3 | 122.5–126.5 |
| binaries | zipper | 76.8–78.9 | 35.5–50.0 |
| binaries | tar+zstd | 82.6–84.9 | 137.2–141.0 |
| binaries | tar+pzstd | 82.1–291.4 | 107.5–173.8 |
| random | zipper | 186.3–279.5 | 35.3–61.8 |
| random | tar+zstd | 61.2–67.2 | 120.8–137.4 |
| random | tar+pzstd | 56.9–65.7 | 122.5–141.1 |
| small-files | zipper | 77.9–280.0 | 174.1–239.0 |
| small-files | tar+zstd | 1110.1–1125.3 | 1011.7–1084.4 |
| small-files | tar+pzstd | 1102.0–1151.6 | 1012.2–1099.8 |

Zipper extracts faster in these local application measurements. Packed small files also compress substantially smaller than either tar alternative. Incompressible-data creation remains a major gap: 1179 MiB/s versus about 4130 MiB/s for the tar pipelines. Binary archives are slightly larger than the tar alternatives. Compression levels and frame/window choices are not identical operating points.

These are end-to-end container/application results, not proof of a faster Zstandard codec. Tar preserves metadata Zipper does not. Requested codec workers are not equal total thread budgets. Creation includes archive-file fsync; extraction is not explicitly synced. Caches are warm, thermal/storage isolation was not performed, and no process-tree RSS was measured. See the [original methodology](../zstd-comparison.md#method-and-limitations) for pipeline and feature differences.

Absolute Zipper timings vary from the separate revision-comparison session. Compare engines within each interleaved experiment; do not combine those sessions into a precise before/after claim. No universal speed leadership follows from these development fixtures.

Reproduce with `python3 scripts/compare_zstd.py /tmp/zipper-corpus --runs 5 --threads 4` after a release build. Requires tar, zstd, and pzstd on PATH. Corpus generation and limitations are described in the [v2 report](README.md).
