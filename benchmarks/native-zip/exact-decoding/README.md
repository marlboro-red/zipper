# Exact Deflate decoding

Decoding ZIP entries in Zipper instead of through the `zip` crate closed two hostile-archive gaps. Streaming extraction of large mixed and random entries also became 1.2× and 2.2× faster, because the new loop reads 64 KiB of input at a time.

## Question

Can the native reader require each Deflate stream to end exactly at its compressed size, and refuse stored entries that hide a data descriptor, without slowing extraction?

Before this change, two archives that `tests/hostile_zip.rs` now rejects were accepted:

- A complete local record after the end of a Deflate stream, inside the entry's declared compressed size. A streaming reader that follows a data descriptor sees that record.
- A stored entry with a data descriptor whose data contains a fake descriptor and then a hidden record. A streaming reader ends the entry at the fake descriptor.

The `zip` crate does not report where a Deflate stream ended. The workers now decode themselves:

- `native` streams Deflate through `flate2::Decompress`, the same zlib-rs backend, and checks the consumed input, the running length and the CRC.
- `native-fast` calls libdeflate's `libdeflate_deflate_decompress_ex` through `libdeflate-sys`, which reports consumed input. This is the crate's first `unsafe` code, with its invariants stated beside it.
- Stored entries with a data descriptor are refused if their data contains a descriptor whose CRC and sizes match the bytes before it. Nested archives contain descriptors for their own entries, which do not match, so they still extract.

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds.
- Baseline is `685069c`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py` ran both binaries with `native` and `native-fast`, four threads, seven interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are 10,000 flat tiny files and the 3,981-file dependency corpus, both zipped by Python `zipfile`, plus the three single-entry fixtures from `scripts/make_large_zip_corpus.py`.
- The load average was 1.9 at the start and 2.6 at the end.

Real archives from Info-ZIP, 7-Zip, Python `zipfile`, `ditto` and bsdtar still extract byte for byte. Across the real archives from all three reviews, the new build produced the same tree as the baseline in all 120 extractions where the baseline succeeded, and rejected nothing the baseline accepted.

The independent review found that a first draft refused every stored entry containing `PK\x07\x08` after a data descriptor. That rejected real bsdtar and Python streaming archives holding nested ZIP files. The final rule rejects only a descriptor that matches the bytes before it. A unit test feeds the scan one byte at a time, so a signature split across reads is still found.

## Results

| Workload | Reader | Wall ms, before | Wall ms, after | CPU ms, before | CPU ms, after |
| --- | --- | ---: | ---: | ---: | ---: |
| 10,000 flat files | native | 319.90 [311.55–376.69] | 318.32 [314.35–336.65] | 1132.0 | 1131.7 |
| 10,000 flat files | native-fast | 332.96 [312.06–337.21] | 328.93 [310.46–348.37] | 1134.3 | 1129.7 |
| Dependency sources | native | 167.82 [165.38–190.21] | 164.60 [162.09–168.17] | 514.0 | 512.4 |
| Dependency sources | native-fast | 158.83 [157.49–174.44] | 159.93 [158.90–193.82] | 496.9 | 502.8 |
| 64 MiB mixed entry | native | 63.99 [63.06–73.44] | 52.72 [50.27–60.44] | 62.0 | 49.6 |
| 64 MiB mixed entry | native-fast | 47.74 [47.25–47.94] | 47.89 [47.29–49.26] | 45.5 | 45.4 |
| 32 MiB compressible entry | native | 10.06 [9.85–15.85] | 10.23 [9.84–21.67] | 9.0 | 9.1 |
| 32 MiB compressible entry | native-fast | 10.27 [9.81–14.31] | 11.14 [9.73–23.65] | 9.1 | 9.4 |
| 32 MiB random entry | native | 21.88 [21.64–22.08] | 10.07 [9.91–10.16] | 20.8 | 9.0 |
| 32 MiB random entry | native-fast | 14.93 [12.92–17.77] | 13.26 [12.70–17.63] | 11.5 | 11.4 |
| Dependency sources, bsdtar stored | native | 165.77 [162.92–220.92] | 157.32 [155.73–161.13] | 520.6 | 491.4 |
| Dependency sources, bsdtar stored | native-fast | 163.25 [161.73–175.85] | 161.61 [156.89–178.64] | 511.4 | 500.3 |

Values are medians with the observed range in brackets. Peak RSS ranges overlap or differ by under 0.1 MiB on every row except the bsdtar stored rows. There, every entry is stored with a data descriptor and gets the descriptor scan, and peak RSS rose by 0.2 MiB, from 6.58 to 6.78 MiB with `native`. The bsdtar stored rows ran in a separate, subsequent session.

- **No regression.** Every row without a gain has overlapping before and after ranges.
- **Two gains.** Streaming `native` on the mixed and random entries got faster, with separate ranges.
- **The limiter is input read size.** A diagnostic build with an 8 KiB input slice returned to the baseline. It took 21.69 ms on the random entry and 63.24 ms on the mixed entry, against 21.84 and 63.41 ms before, and 10.27 and 50.79 ms with 64 KiB. Each small read goes through the shared file handle as its own seek and read.

## Decision

Kept. The change closes two ways to hide data from Zipper, costs no memory, and makes large streamed entries faster. `native-fast` still decodes buffered entries with libdeflate, now through `libdeflate-sys`.

## Limits

- One machine, warm cache, four threads.
- The gain applies to streamed entries larger than the old read size. Tiny files show no change.
- A Unicode path extra field that renames an entry is still not checked. The [roadmap](../../../docs/roadmap.md#m1-correctness-contract-for-zip) tracks it.
- Java `jar` output and archives over 4 GiB were not tested.

## Reproduce

```sh
python3 scripts/make_large_zip_corpus.py /tmp/large
python3 scripts/compare_zip_readers.py /tmp/large/random.zip /tmp/large/random \
  --reader before native zipper-before --reader after native zipper-after \
  --reader before-fast native-fast zipper-before --reader after-fast native-fast zipper-after \
  --runs 7 --threads 4 > random.json
```

Build `zipper-before` at `685069c` and `zipper-after` at this commit. For the diagnostic, change `split_at_mut(64 * 1024)` in `stream_entry` to 8 KiB.
