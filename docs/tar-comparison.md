# Direct comparisons against bsdtar

Apple M5, 32 GiB RAM, macOS; Silesia, 12 files, 211,938,580 input bytes.
Seven interleaved measured runs after one warmup. Times include process startup
and extraction writes, exclude explicit extraction sync, and use warm caches.
These are local workload results, not general performance guarantees.

## Identical ZIP archive

All readers extract the same 70,252,453-byte Deflate ZIP. Every extraction passed
direct byte comparison, file-size checks, and tree-membership checks.
The tar executable is `/usr/bin/tar`: bsdtar 3.5.3 / libarchive 3.7.4.

| Reader | Median extraction | Observed range | Throughput | Speed relative to tuned bsdtar |
|---|---:|---:|---:|---:|
| bsdtar, default | 226.72 ms | 223.97–228.39 ms | 891 MiB/s | 0.80× |
| bsdtar, `-b 2048` (1 MiB) | 182.45 ms | 179.78–184.16 ms | 1,108 MiB/s | 1.00× |
| Zipper `native`, four workers | 114.82 ms | 114.26–115.35 ms | 1,760 MiB/s | 1.59× |
| Zipper `native-fast`, four workers | 91.46 ms | 90.25–91.66 ms | 2,210 MiB/s | 1.99× |

Zipper's native readers are opt-in; its default ZIP reader still delegates to
7-Zip and took 559.38 ms here. Native stream peaked at 4.9 MiB RSS; native-fast
peaked at 174.2 MiB. bsdtar RSS was not measured. bsdtar uses a single extraction
process; this is not a comparison against multiple coordinated tar processes.
Zipper's native readers currently preserve bytes and directories, not full
archive metadata.

[Raw samples](../benchmarks/silesia/zip-vs-tar.json).
Reproduce from the repository root with the fetched Silesia corpus:

```sh
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia \
  --formats zip --native-fast --tar-reader tar --reuse-archives \
  --runs 7 --threads 4
```

## Own format versus TAR pipelines

A separate seven-run comparison on the same corpus uses Zipper level 1 and
zstd/pzstd level 1. Creation uses matched macOS `F_FULLFSYNC`; extraction checks
SHA-256 file manifests. These tar results use default record buffering.

| Archive / extractor | Archive bytes | Median extraction | Extraction throughput |
|---|---:|---:|---:|
| `.zipper` / Zipper | 73,602,933 | 34.63 ms | 5,837 MiB/s |
| `.tar.zst` / zstd + bsdtar | 73,317,298 | 131.61 ms | 1,536 MiB/s |
| `.tar.zst` / pzstd + bsdtar | 73,435,701 | 104.18 ms | 1,940 MiB/s |
| Uncompressed `.tar` / bsdtar | 211,978,240 | 94.23 ms | 2,145 MiB/s |

Zipper extracts 2.72× as fast as plain bsdtar and 3.01× as fast as the
parallel-pzstd pipeline in this run. Its archive is about 0.23% larger than
pzstd's. Metadata capabilities differ; these numbers do not establish metadata
fidelity equivalence or the best possible tuned tar performance.

[Raw samples](../benchmarks/silesia/tar-current.json).
