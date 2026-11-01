# Native level and block-size sweep on dependency sources

Hypothesis: on a real source tree of many small related files, larger blocks and
higher levels reduce archive size substantially without a measurable extraction
penalty, because file creation rather than decoding dominates extraction.
Metric: archive bytes plus median extraction time with observed ranges.

Workload: the [locked dependency corpus](README.md) (70 crates, 3,981 files,
67,479,735 bytes), regenerated and checked file-for-file against the committed
manifest. Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1, release build of
`b111c00`, `--threads 4`. `scripts/sweep.py` ran 35 configurations in a shuffled
order each round: one warmup round and seven measured rounds. Every extraction was
checked by direct byte comparison, file sizes and tree membership; there were no
errors. Load average rose from 2.2 to 4.6 during the run. Warm cache only.
[Raw samples](level-sweep.json).

## Findings

- **Size:** the current default (L1 / 1 MiB) gives 5.51×. L3 / 4 MiB gives 7.35×
  (25% fewer bytes) and L19 / 16 MiB gives 10.56× (48% fewer bytes). Block size
  matters more than on Silesia: small source files gain from shared history
  across files within a block.
- **Extraction:** every configuration of at least 1 MiB has a median between
  182 and 217 ms, and their ranges overlap. This run does not establish a
  speed ranking among them. 64 KiB blocks are slower (232–245 ms medians).
- **Memory:** extraction peak RSS grows with block size: about 26 MiB at 1 MiB,
  63–70 MiB at 4 MiB and 75–84 MiB at 16 MiB. Creation at L15+ with 4 MiB+
  blocks exceeds 270 MiB.
- **Creation:** L3 / 4 MiB keeps 542 MiB/s against 591 MiB/s for the default;
  L19 drops to about 20 MiB/s.

Observed extraction/ratio frontier (no other point is both faster and smaller):
L1 / 4 MiB (5.64×, 181.91 ms), L3 / 16 MiB (7.58×, 183.75 ms) and L19 / 16 MiB
(10.56×, 190.20 ms). Because the ranges overlap, treat it as a size ranking with
inconclusive speed differences, not as proof that those points are fastest.

Decision: no default change. On Silesia, L3 / 4 MiB extracted measurably slower
than L1 / 1 MiB (43.07 ms vs 36.08 ms, non-overlapping ranges), and larger blocks
increase memory and selected-file read amplification. A default change needs a
matched experiment across both workloads, plus memory and `extract --path` cost.
For source trees where size matters, `--level 3 --block-size-kib 4096` is the
measured cost-effective setting on this machine.

Limitations: one machine, one source tree, warm cache, concurrent background
load. Not a cross-platform or cold-cache result.

## All configurations

| Level / block | Ratio | Archive bytes | Creation MiB/s | Extraction ms | Range ms | Creation RSS MiB | Extraction RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| L-3 / 64 KiB | 3.683× | 18,320,407 | 596 | 231.57 | 214.82–264.43 | 8.0 | 6.6 |
| L-3 / 256 KiB | 4.135× | 16,320,484 | 599 | 209.79 | 203.01–228.02 | 11.4 | 10.2 |
| L-3 / 1024 KiB | 4.305× | 15,676,014 | 591 | 187.05 | 180.40–202.53 | 25.2 | 25.8 |
| L-3 / 4096 KiB | 4.406× | 15,314,094 | 560 | 186.99 | 179.26–203.89 | 51.0 | 69.9 |
| L-3 / 16384 KiB | 4.436× | 15,212,621 | 504 | 187.93 | 180.37–195.44 | 94.0 | 83.9 |
| L1 / 64 KiB | 4.679× | 14,420,410 | 584 | 231.56 | 220.39–236.97 | 8.0 | 6.6 |
| L1 / 256 KiB | 5.086× | 13,267,980 | 608 | 208.70 | 194.19–214.11 | 12.2 | 10.2 |
| L1 / 1024 KiB | 5.511× | 12,243,518 | 591 | 193.19 | 187.01–211.47 | 29.4 | 25.8 |
| L1 / 4096 KiB | 5.641× | 11,962,766 | 579 | 181.91 | 171.33–200.66 | 51.0 | 67.2 |
| L1 / 16384 KiB | 5.675× | 11,890,278 | 496 | 190.58 | 176.71–223.23 | 92.5 | 80.7 |
| L3 / 64 KiB | 4.867× | 13,864,924 | 569 | 244.88 | 223.12–356.65 | 9.2 | 6.7 |
| L3 / 256 KiB | 5.333× | 12,653,183 | 594 | 210.70 | 201.52–231.26 | 13.8 | 10.3 |
| L3 / 1024 KiB | 6.249× | 10,799,194 | 589 | 193.88 | 183.21–200.96 | 29.1 | 25.8 |
| L3 / 4096 KiB | 7.347× | 9,184,575 | 542 | 187.92 | 170.66–207.12 | 53.5 | 64.7 |
| L3 / 16384 KiB | 7.578× | 8,904,481 | 469 | 183.75 | 178.13–196.68 | 94.5 | 77.8 |
| L6 / 64 KiB | 5.318× | 12,690,012 | 415 | 241.92 | 228.28–254.92 | 10.3 | 6.5 |
| L6 / 256 KiB | 5.890× | 11,456,981 | 435 | 220.76 | 212.83–237.03 | 22.5 | 10.3 |
| L6 / 1024 KiB | 7.041× | 9,583,548 | 460 | 205.10 | 191.54–211.26 | 34.2 | 25.5 |
| L6 / 4096 KiB | 8.466× | 7,970,997 | 437 | 196.35 | 185.90–217.04 | 75.6 | 63.7 |
| L6 / 16384 KiB | 8.867× | 7,609,902 | 333 | 195.09 | 187.18–206.56 | 104.7 | 76.5 |
| L9 / 64 KiB | 5.403× | 12,488,723 | 316 | 243.37 | 235.85–267.86 | 10.2 | 6.6 |
| L9 / 256 KiB | 6.033× | 11,185,239 | 320 | 218.62 | 211.62–238.13 | 22.6 | 10.3 |
| L9 / 1024 KiB | 7.186× | 9,390,775 | 365 | 207.39 | 197.41–228.92 | 67.2 | 25.3 |
| L9 / 4096 KiB | 8.784× | 7,682,537 | 345 | 195.08 | 182.30–210.90 | 109.6 | 63.5 |
| L9 / 16384 KiB | 9.325× | 7,236,554 | 270 | 196.00 | 182.57–203.97 | 134.3 | 76.1 |
| L15 / 64 KiB | 5.606× | 12,036,074 | 58 | 243.87 | 227.75–326.06 | 13.1 | 6.5 |
| L15 / 256 KiB | 6.358× | 10,613,308 | 49 | 217.58 | 212.67–258.97 | 26.5 | 10.2 |
| L15 / 1024 KiB | 7.355× | 9,174,508 | 88 | 197.37 | 186.53–258.23 | 91.1 | 25.4 |
| L15 / 4096 KiB | 9.060× | 7,447,921 | 64 | 198.91 | 181.95–226.96 | 334.0 | 63.2 |
| L15 / 16384 KiB | 9.717× | 6,944,461 | 50 | 201.86 | 179.91–231.90 | 349.9 | 75.8 |
| L19 / 64 KiB | 5.648× | 11,948,415 | 18 | 241.74 | 235.02–279.73 | 13.1 | 6.5 |
| L19 / 256 KiB | 6.413× | 10,522,612 | 21 | 212.57 | 196.36–259.46 | 30.4 | 10.3 |
| L19 / 1024 KiB | 7.827× | 8,621,861 | 24 | 217.39 | 194.70–235.46 | 93.9 | 25.3 |
| L19 / 4096 KiB | 9.705× | 6,952,740 | 24 | 190.82 | 179.67–200.06 | 272.5 | 62.8 |
| L19 / 16384 KiB | 10.555× | 6,392,933 | 18 | 190.20 | 181.52–196.30 | 416.1 | 75.3 |

```sh
python3 scripts/make_dependency_corpus.py /tmp/rust-dependencies
python3 scripts/sweep.py /tmp/rust-dependencies --levels -3 1 3 6 9 15 19 \
  --blocks-kib 64 256 1024 4096 16384 --threads 4 --runs 7 > level-sweep.json
```
