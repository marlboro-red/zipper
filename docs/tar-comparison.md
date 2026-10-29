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

## Own format versus tuned TAR pipelines

The stronger comparison uses 1 MiB tar record buffers (`-b 2048`), alongside
default-buffer controls, in seven interleaved rounds. Zipper and zstd/pzstd use
level 1. Creation uses matched macOS `F_FULLFSYNC`; extraction checks SHA-256
file manifests. Compressed extraction uses `--ignore-zeros` so tar consumes all
padding and the decoder completes without SIGPIPE. Source: e382a55.

| Archive / extractor | Archive bytes | Median extraction | Observed range | Throughput |
|---|---:|---:|---:|---:|
| `.zipper` / Zipper | 73,602,933 | 35.37 ms | 34.33–36.29 ms | 5,715 MiB/s |
| Uncompressed `.tar` / bsdtar, 1 MiB | 212,860,928 | 43.84 ms | 36.55–46.87 ms | 4,610 MiB/s |
| `.tar.zst` / pzstd + bsdtar, 1 MiB | 73,435,731 | 56.24 ms | 55.55–58.01 ms | 3,594 MiB/s |
| `.tar.zst` / zstd + bsdtar, 1 MiB | 73,317,325 | 131.42 ms | 129.22–134.09 ms | 1,538 MiB/s |

**Against these tuned baselines, Zipper extracts 1.24× as fast as plain bsdtar
and 1.59× as fast as tar+pzstd.** Its archive is about 0.23% larger than pzstd's.
Metadata capabilities differ, and this is not a comparison against several
coordinated tar extraction processes. Plain tar has substantial run-to-run spread.

The same run measured default-buffer plain tar at 95.11 ms and default-buffer
tar+pzstd at 106.52 ms. Earlier 2.7×/3× claims compared those default settings;
they must not be presented as gains over the stronger tuned baselines.

[Raw tuned samples](../benchmarks/silesia/tar-tuned.json).
[Earlier default-buffer samples](../benchmarks/silesia/tar-current.json).
Reproduce from the repository root:

```sh
python3 scripts/compare_zstd.py /tmp/zipper-corpora --corpus silesia \
  --include-plain-tar --tuned-tar --runs 7
```
