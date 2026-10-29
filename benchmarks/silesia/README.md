# Decompression time versus compression ratio

![Measured Pareto frontier](pareto.svg)

[PNG](pareto.png) · [PDF](pareto.pdf) · [Native samples](sweep.json) · [Higher native levels](dense-sweep.json) · [Classic-format samples](classic-formats.json)

A point is on the observed Pareto frontier if no other measured configuration extracts in less or equal median time while producing a smaller or equal archive, with at least one strict improvement. Better points are toward the upper left. Ratio means original bytes divided by complete archive bytes. Lines only connect observed points; they do not imply intermediate configurations exist. Error bars show observed minima and maxima, not confidence intervals. Overlapping ranges do not establish a reliable speed ranking.

## Native frontier (numbers on the detail plot)

| Point | Setting | Ratio | Extraction median (ms) | Range (ms) |
| --- | --- | ---: | ---: | ---: |
| 1 | L-3 / 1024 KiB | 2.229× | 29.12 | 28.27–31.02 |
| 2 | L-3 / 256 KiB | 2.314× | 32.91 | 31.65–36.08 |
| 3 | L1 / 1024 KiB | 2.879× | 36.08 | 35.15–38.52 |
| 4 | L15 / 1024 KiB | 3.474× | 36.44 | 35.29–36.96 |
| 5 | L15 / 4096 KiB | 3.627× | 40.17 | 39.33–42.55 |
| 6 | L19 / 1024 KiB | 3.705× | 40.63 | 38.83–42.44 |
| 7 | L19 / 4096 KiB | 3.863× | 44.13 | 42.99–45.06 |
| 8 | L19 / 16384 KiB | 3.970× | 52.87 | 51.31–54.11 |

## All native configurations

| Level / block | Ratio | Archive bytes | Compression MiB/s | Extraction ms | Range ms | Peak creation RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| L-3 / 64 KiB | 2.141× | 98,999,999 | 1517 | 41.40 | 40.52–48.49 | 5.6 |
| L-3 / 256 KiB | 2.314× | 91,584,442 | 1720 | 32.91 | 31.65–36.08 | 10.9 |
| L-3 / 1024 KiB | 2.229× | 95,082,179 | 1664 | 29.12 | 28.27–31.02 | 28.3 |
| L-3 / 4096 KiB | 2.237× | 94,744,290 | 1511 | 33.13 | 32.73–34.56 | 100.5 |
| L-3 / 16384 KiB | 2.240× | 94,633,608 | 1495 | 39.01 | 37.85–41.02 | 258.6 |
| L1 / 64 KiB | 2.762× | 76,724,793 | 1571 | 47.32 | 46.78–48.85 | 5.6 |
| L1 / 256 KiB | 2.891× | 73,317,860 | 1631 | 39.37 | 38.32–42.92 | 10.7 |
| L1 / 1024 KiB | 2.879× | 73,602,933 | 1784 | 36.08 | 35.15–38.52 | 28.3 |
| L1 / 4096 KiB | 2.890× | 73,338,765 | 1769 | 41.19 | 39.26–41.71 | 97.9 |
| L1 / 16384 KiB | 2.894× | 73,238,528 | 1500 | 45.83 | 44.96–47.01 | 248.4 |
| L3 / 64 KiB | 2.882× | 73,545,070 | 1595 | 48.78 | 46.32–49.30 | 7.1 |
| L3 / 256 KiB | 3.042× | 69,676,144 | 1538 | 41.64 | 40.31–43.41 | 12.6 |
| L3 / 1024 KiB | 3.147× | 67,353,540 | 1544 | 39.40 | 36.52–40.60 | 33.3 |
| L3 / 4096 KiB | 3.188× | 66,488,588 | 1427 | 43.07 | 41.89–47.04 | 103.5 |
| L3 / 16384 KiB | 3.203× | 66,174,317 | 1264 | 47.59 | 45.56–48.90 | 234.5 |
| L6 / 64 KiB | 3.037× | 69,792,285 | 549 | 51.43 | 49.62–53.30 | 8.0 |
| L6 / 256 KiB | 3.205× | 66,136,979 | 595 | 39.56 | 37.85–43.75 | 20.5 |
| L6 / 1024 KiB | 3.357× | 63,138,699 | 568 | 38.68 | 38.19–39.55 | 36.5 |
| L6 / 4096 KiB | 3.431× | 61,779,861 | 528 | 41.87 | 39.56–44.41 | 110.7 |
| L6 / 16384 KiB | 3.455× | 61,336,542 | 527 | 48.85 | 45.72–51.35 | 236.2 |
| L9 / 1024 KiB | 3.415× | 62,064,993 | 344 | 38.65 | 36.68–40.59 | 67.6 |
| L9 / 4096 KiB | 3.525× | 60,117,935 | 317 | 44.70 | 41.55–45.41 | 140.8 |
| L9 / 16384 KiB | 3.573× | 59,311,908 | 300 | 50.44 | 48.92–52.18 | 265.3 |
| L15 / 1024 KiB | 3.474× | 61,015,011 | 77 | 36.44 | 35.29–36.96 | 92.6 |
| L15 / 4096 KiB | 3.627× | 58,429,378 | 43 | 40.17 | 39.33–42.55 | 356.8 |
| L15 / 16384 KiB | 3.703× | 57,241,254 | 34 | 47.90 | 47.50–48.96 | 487.2 |
| L19 / 1024 KiB | 3.705× | 57,196,380 | 25 | 40.63 | 38.83–42.44 | 95.6 |
| L19 / 4096 KiB | 3.863× | 54,863,200 | 19 | 44.13 | 42.99–45.06 | 295.7 |
| L19 / 16384 KiB | 3.970× | 53,386,059 | 16 | 52.87 | 51.31–54.11 | 551.8 |

## Zipper handling classic formats

Archives are created by 7-Zip 26.05: ZIP/Deflate and 7z/LZMA2 (levels labeled in the table), and uncompressed TAR. Default extraction delegates to that backend. ZIP stream uses the opt-in parallel Rust ZIP/zlib-rs reader; ZIP buffered adds libdeflate with up to 64 MiB of entry buffers per worker. Both request four workers and read the same archive bytes as their matched direct backend. Native ZIP preserves bytes/directories only, so it is not yet a metadata-equivalent replacement. The plot uses Zipper extraction time, not direct-backend time. [Current level-1 samples](fast-zip-current.json) and [current level-9 samples](dense-classic-current.json) use work-stealing revision `afb77d0` and include in-process RSS. Earlier static-scheduling trials remain in `fast-zip.json` and `dense-classic.json`. Current extraction-only trials prepare one archive per format before timing repeated reads; their null creation times are intentional.

| Format / reader | Ratio | Archive bytes | Zipper median ms | Zipper range ms | Direct 7-Zip median ms | Native reader RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ZIP L1 | 3.017× | 70,252,453 | 565.62 | 564.36–568.82 | 563.52 | — |
| 7Z L1 | 3.584× | 59,126,554 | 204.76 | 202.18–210.40 | 205.30 | — |
| TAR | 1.000× | 211,948,544 | 34.06 | 29.77–68.38 | 33.34 | — |
| ZIP L9 | 3.274× | 64,727,209 | 538.60 | 537.36–551.33 | 536.80 | — |
| ZIP L9 stream | 3.274× | 64,727,209 | 115.64 | 100.75–116.50 | 536.80 | 5.0 |
| ZIP L9 buffered | 3.274× | 64,727,209 | 92.61 | 91.79–99.37 | 536.80 | 175.1 |
| 7Z L9 | 4.353× | 48,688,221 | 1051.27 | 1048.95–1058.55 | 1048.61 | — |
| ZIP L1 stream | 3.017× | 70,252,453 | 115.48 | 114.52–132.14 | 560.77 | 4.8 |
| ZIP L1 buffered | 3.017× | 70,252,453 | 91.75 | 91.00–93.50 | 560.77 | 174.0 |

## Tuned bsdtar baselines

These green points use 1 MiB tar record buffers. T2 uses four pzstd workers; T3 uses ordinary zstd decoding. Compressed pipelines consume all padding (`--ignore-zeros`) so the decoder finishes successfully. T4 extracts the same ZIP used in its matched Zipper comparison. Tar round trips use SHA-256 manifests; ZIP comparisons use direct byte checks. Timing sessions differ from the native level sweep. [Tar samples](tar-tuned.json), [identical-ZIP samples](zip-vs-tar.json), and [matched comparison details](../../docs/tar-comparison.md).

| Point | Reader | Ratio | Archive bytes | Median ms | Range ms |
| --- | --- | ---: | ---: | ---: | ---: |
| T1 | tar-1MiB | 0.996× | 212,860,928 | 43.84 | 36.55–46.87 |
| T2 | tar+pzstd-1MiB | 2.886× | 73,435,731 | 56.24 | 55.55–58.01 |
| T3 | tar+zstd-1MiB | 2.891× | 73,317,325 | 131.42 | 129.22–134.09 |
| T4 | bsdtar ZIP, 1 MiB | 3.017× | 70,252,453 | 182.45 | 179.78–184.16 |

## Method and limits

Silesia contains 211,938,580 bytes in 12 files. Data came from a [pinned public mirror](https://github.com/MiloszKrajewski/SilesiaCorpus/tree/3f3fa2cdbbb3795c903b74e774acb309e1360337); [provenance and SHA-256 hashes](corpus.json) make the inputs auditable. Each native setting ran seven measured rounds after a warmup, with shuffled order. Classic formats ran in a separate interleaved session; native/classic cross-session timing differences are less controlled than comparisons within each session. Every extraction was compared directly byte for byte, including tree membership and file sizes. Verification was outside the timed interval.

Times include process startup, index/metadata work and filesystem writes, and exclude explicit extraction sync. They are application extraction times, not in-memory codec speeds. Cache flushing and thermal/storage isolation were not performed. Native uses four codec workers plus I/O threads; classic extraction uses 7-Zip's default thread allocation for both paths. Tar/ZIP/7z metadata semantics differ from native Zipper. No cross-machine leadership or equal-feature comparison is claimed. Repeat on Linux/x86 and larger corpora before changing defaults.

The sweep exposes a genuine tradeoff: larger blocks can improve higher-level ratio, but consume much more memory and often extract more slowly. The default remains level 1 / 1024 KiB. Compression speed, memory, selective extraction cost, and metadata support are separate axes not optimized by this two-dimensional frontier.

## Reproduce

```sh
python3 scripts/fetch_silesia.py /tmp/zipper-corpora/silesia
cargo build --release
python3 scripts/sweep.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/sweep.json
python3 scripts/sweep.py /tmp/zipper-corpora/silesia --levels 9 15 19 --blocks-kib 1024 4096 16384 --runs 7 --threads 4 > benchmarks/silesia/dense-sweep.json
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/classic-formats.json
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip --native-fast --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/fast-zip-current.json
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip 7z --level 9 --native-fast --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/dense-classic-current.json
python3 scripts/compare_zstd.py /tmp/zipper-corpora --corpus silesia --include-plain-tar --tuned-tar --runs 7 > benchmarks/silesia/tar-tuned.json
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip --native-fast --tar-reader tar --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/zip-vs-tar.json
# With matplotlib installed in your plotting environment:
python3 scripts/plot_pareto.py benchmarks/silesia
```

Copy the adjacent generated `silesia.manifest.json` to `benchmarks/silesia/corpus.json` when refreshing the data. The fetch destination must be new. Classic format runs require 7zz on PATH.
