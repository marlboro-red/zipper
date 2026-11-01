# Native ZIP baseline, 2026-10-29

Opt-in native ZIP extraction ran about twice as fast as direct 7-Zip on the logs, binaries and small-files workloads. Random-data ranges overlap, so this baseline shows no speed win there.

This is an early baseline. Newer results are in [small-file ZIP extraction versus bsdtar](bsdtar-comparison.md), which also covers automatic-concurrency tuning.

## Question

How fast is opt-in native ZIP extraction compared with direct 7-Zip on the same ZIP archives?

## Setup

Engine `c996d32`, Apple M5, 32 GiB, macOS 26.7.1. 7-Zip 26.05 creates each ZIP at level 1. Each ZIP is then extracted through the default Zipper backend, direct 7-Zip, and opt-in native ZIP.

Decoder order is interleaved, with seven measured rounds plus warmup. Every extraction is compared byte for byte against the original tree. Native requests four workers. Both backend paths use 7-Zip defaults.

Timings include process startup and filesystem writes. They exclude verification and explicit extraction sync. Caches are warm.

## Results

| Workload | Native MiB/s | Direct 7-Zip MiB/s | Native median ms | Native range ms | Direct range ms | Archive bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [logs](logs.json) | 1041 | 502 | 245.89 | 244.01–248.53 | 504.31–517.06 | 55,355,767 |
| [binaries](binaries.json) | 633 | 336 | 351.32 | 348.47–352.68 | 656.77–665.98 | 76,607,604 |
| [random](random.json) | 4398 | 4331 | 58.21 | 41.23–62.23 | 58.21–61.40 | 268,435,748 |
| [small-files](small-files.json) | 119 | 64 | 131.41 | 130.03–147.39 | 239.27–256.32 | 1,088,044 |

Silesia adds a public mixed corpus, with a [Pareto plot and samples](../silesia/README.md). Its identical ZIP extracts in 168.05 ms median through native ZIP versus 563.32 ms directly through 7-Zip (3.35× throughput).

## Decision

The native path improves these compressible-data workloads. The random-data ranges overlap, so no speed win is established there. The native ZIP option remains explicit, and the backend remains the default.

The initial native ZIP prototype reparsed metadata per worker and lacked early raw directory bounds. Independent review caught resource amplification and ambiguous end-record selection. The shipped path preflights directory sizes and counts and rejects ambiguous end records. It shares parsed metadata and one file handle through independent buffered cursors. It publishes output only after all CRC and length checks pass. See `tests/native_zip.rs` and `src/zip_reader.rs`.

## Limits

This is an application comparison. Native preserves only bytes and directories. 7-Zip preserves additional metadata, including timestamps and attributes that Zipper does not restore.

Native requested four workers, while direct 7-Zip ran as one process with its default thread allocation. The reports record wall time only, not CPU time. This baseline has no one-worker native measurement. The [matched-thread re-run](../rust-dependencies/README.md) compares readers at equal worker counts.

These results do not establish that the zlib-rs codec alone is faster. They also do not establish that Zipper is the fastest ZIP implementation.

Caches are warm. Storage and thermal isolation were not performed, and neither was process-tree RSS accounting. Any test of the buffered-codec prototype must be reported separately, with its memory cost.

## Reproduce

Run `python3 scripts/compare_formats.py /path/to/corpus --formats zip --native-zip --runs 7 --threads 4`.
