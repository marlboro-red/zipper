# Native ZIP extraction across workloads — 2026-10-29

[Latest small-file ZIP versus bsdtar results and automatic-concurrency tuning](small-files-tar.md). The tables below retain the earlier revision baseline.

Engine `c996d32`, Apple M5, 32 GiB, macOS 26.7.1. Each ZIP is created by 7-Zip 26.05 at level 1, then extracted through the default Zipper backend, direct 7-Zip, and opt-in native ZIP. Decoder order is interleaved, seven measured rounds plus warmup. Every extraction is directly compared byte for byte against the original tree. Native requests four workers; backend paths both use 7-Zip defaults.

| Workload | Native MiB/s | Direct 7-Zip MiB/s | Native median ms | Native range ms | Direct range ms | Archive bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [logs](logs.json) | 1041 | 502 | 245.89 | 244.01–248.53 | 504.31–517.06 | 55,355,767 |
| [binaries](binaries.json) | 633 | 336 | 351.32 | 348.47–352.68 | 656.77–665.98 | 76,607,604 |
| [random](random.json) | 4398 | 4331 | 58.21 | 41.23–62.23 | 58.21–61.40 | 268,435,748 |
| [small-files](small-files.json) | 119 | 64 | 131.41 | 130.03–147.39 | 239.27–256.32 | 1,088,044 |

The native path improves these compressible-data workloads; random-data ranges overlap, so no speed win is established there. This is an application comparison. Native preserves bytes/directories only; 7-Zip preserves additional metadata. The native ZIP option remains explicit, and the backend remains the default. These results do not establish that the zlib-rs codec alone is faster or that Zipper is the fastest ZIP implementation.

Silesia adds a public mixed corpus: [Pareto plot and samples](../silesia/README.md). Its identical ZIP extracts in 168.05 ms median through native ZIP versus 563.32 ms directly through 7-Zip (3.35× throughput). The buffered-codec prototype, if tested later, must be reported separately with its memory cost.

The initial native ZIP prototype reparsed metadata per worker and lacked early raw directory bounds. Independent review caught resource amplification and ambiguous end-record selection. The shipped path preflights directory sizes/counts, rejects ambiguous end records, shares parsed metadata and one file handle through independent buffered cursors, and publishes only after all CRC/length checks pass. See `tests/native_zip.rs` and `src/zip_reader.rs`.

Reproduce with `python3 scripts/compare_formats.py /path/to/corpus --formats zip --native-zip --runs 7 --threads 4`. Timings include process startup and filesystem writes but exclude verification and explicit extraction sync. Caches are warm; storage/thermal isolation and process-tree RSS accounting were not performed.
