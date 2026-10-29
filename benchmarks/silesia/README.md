# Decompression time versus compression ratio

![Measured Pareto frontier](pareto.svg)

[PNG](pareto.png) · [PDF](pareto.pdf) · [Native samples](sweep.json) · [Classic-format samples](classic-formats.json)

A point is on the observed Pareto frontier if no other measured configuration extracts in less or equal median time while producing a smaller or equal archive, with at least one strict improvement. Better points are toward the upper left. Ratio means original bytes divided by complete archive bytes. Lines only connect observed points; they do not imply intermediate configurations exist. Error bars show observed minima and maxima, not confidence intervals. Overlapping ranges do not establish a reliable speed ranking.

## Native frontier (numbers on the detail plot)

| Point | Setting | Ratio | Extraction median (ms) | Range (ms) |
| --- | --- | ---: | ---: | ---: |
| 1 | L-3 / 1024 KiB | 2.229× | 29.12 | 28.27–31.02 |
| 2 | L-3 / 256 KiB | 2.314× | 32.91 | 31.65–36.08 |
| 3 | L1 / 1024 KiB | 2.879× | 36.08 | 35.15–38.52 |
| 4 | L6 / 1024 KiB | 3.357× | 38.68 | 38.19–39.55 |
| 5 | L6 / 4096 KiB | 3.431× | 41.87 | 39.56–44.41 |
| 6 | L6 / 16384 KiB | 3.455× | 48.85 | 45.72–51.35 |

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

## Zipper handling classic formats

Archives are created by 7-Zip 26.05: ZIP/Deflate level 1, 7z/LZMA2 level 1, and uncompressed TAR. Zipper currently delegates their extraction to that same backend. The direct-backend column measures the same archive without Zipper's dispatch and staging. It is not an independent Rust ZIP or LZMA2 decoder. The plot uses Zipper extraction time, not the direct-backend time.

| Format | Ratio | Archive bytes | Zipper median ms | Zipper range ms | 7-Zip direct median ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| ZIP | 3.017× | 70,252,453 | 565.62 | 564.36–568.82 | 563.52 |
| 7Z | 3.584× | 59,126,554 | 204.76 | 202.18–210.40 | 205.30 |
| TAR | 1.000× | 211,948,544 | 34.06 | 29.77–68.38 | 33.34 |

## Method and limits

Silesia contains 211,938,580 bytes in 12 files. Data came from a [pinned public mirror](https://github.com/MiloszKrajewski/SilesiaCorpus/tree/3f3fa2cdbbb3795c903b74e774acb309e1360337); [provenance and SHA-256 hashes](corpus.json) make the inputs auditable. Each native setting ran seven measured rounds after a warmup, with shuffled order. Classic formats ran in a separate interleaved session; native/classic cross-session timing differences are less controlled than comparisons within each session. Every extraction was compared directly byte for byte, including tree membership and file sizes. Verification was outside the timed interval.

Times include process startup, index/metadata work and filesystem writes, and exclude explicit extraction sync. They are application extraction times, not in-memory codec speeds. Cache flushing and thermal/storage isolation were not performed. Native uses four codec workers plus I/O threads; classic extraction uses 7-Zip's default thread allocation for both paths. Tar/ZIP/7z metadata semantics differ from native Zipper. No cross-machine leadership or equal-feature comparison is claimed. Repeat on Linux/x86 and larger corpora before changing defaults.

The sweep exposes a genuine tradeoff: larger blocks can improve higher-level ratio, but consume much more memory and often extract more slowly. The default remains level 1 / 1024 KiB. Compression speed, memory, selective extraction cost, and metadata support are separate axes not optimized by this two-dimensional frontier.

## Reproduce

```sh
python3 scripts/fetch_silesia.py /tmp/zipper-corpora/silesia
cargo build --release
python3 scripts/sweep.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/sweep.json
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/classic-formats.json
# With matplotlib installed in your plotting environment:
python3 scripts/plot_pareto.py benchmarks/silesia
```

Copy the adjacent generated `silesia.manifest.json` to `benchmarks/silesia/corpus.json` when refreshing the data. The fetch destination must be new. Classic format runs require 7zz on PATH.
