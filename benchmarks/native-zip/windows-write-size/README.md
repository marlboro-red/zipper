# Bounded native-fast output writes on Windows

Writing whole-buffer output in 256 KiB slices fixed a native-fast whole-buffer write regression on Windows. Against the old whole-write path, extraction became 5.38× to 6.72× faster. The streaming reader stayed faster on the compressible entry, at 33.12 ms versus 35.93 ms for the chunked path. The mixed and random speedups apply only with a budget above the 64 MiB default. The change is retained only on Windows.

## Question

Does the previously observed Windows large-entry regression come from large synchronous output writes, rather than solely from the decoder?

The experiment compares streaming, whole-buffer decoding with one output write, and identical whole-buffer decoding with 256 KiB output slices. The candidate changes only write size. It adds no buffers, copies, threads or checksum changes.

## Setup

[Experiment 40985992348](https://github.com/marlboro-red/zipper/actions/runs/40985992348) built source `b48ea32` and an isolated candidate patch. It ran seven interleaved measured rounds after warmup, with four requested workers and one entry per ZIP.

Whole-buffer readers explicitly use a 128 MiB shared entry-buffer budget, so the mixed and random cases cannot silently fall back to streaming. Every extraction is compared with the original bytes and complete tree. Integrity-only timing also has a separately verified extraction outside its timed region. `scripts/make_large_zip_corpus.py` generates the fixtures with seed 20261030.

No cache flushing or extraction fsync is requested. Protection settings were not changed.

## Results

### Windows extraction

| Fixture | Whole-write ms [range] | 256 KiB writes ms [range] | Speedup | Streaming ms | Whole/chunked peak RAM MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Mixed, 61 MiB | 702.86 [630.03–728.91] | 104.65 [104.22–105.50] | 6.72× | 124.16 | 97.12 / 97.12 |
| Compressible, 32 MiB | 193.37 [177.18–231.09] | 35.93 [35.32–36.51] | 5.38× | 33.12 | 35.95 / 35.93 |
| Random, 32 MiB | 229.35 [215.19–263.43] | 37.25 [36.87–37.49] | 6.16× | 45.44 | 67.88 / 67.88 |

The speedup column compares the chunked path with the old whole-write path, not with streaming. After the change, whole-buffer mode beats streaming on mixed and random data in this run, with separate ranges. Streaming took 124.16 [122.60–130.96] ms on mixed data and 45.44 [45.28–45.89] ms on random data. Streaming still wins on compressible data, at 33.12 [32.43–33.80] ms versus 35.93 [35.32–36.51] ms, also with separate ranges. Streaming uses approximately 4 MiB RAM.

### Windows integrity-only

Integrity-only medians (whole-buffer / streaming) were 86.61 / 88.32 ms for mixed, 26.06 / 23.04 ms for compressible and 27.33 / 20.03 ms for random data. The large whole-buffer extraction penalty is absent without output writes. These timings include input reads, allocation, decoding and CRC checks. They are not isolated codec throughput.

### Other platforms

Linux whole-write / chunked medians were 91.73 / 92.25 ms (mixed), 28.64 / 28.69 ms (compressible), and 39.45 / 39.61 ms (random), with overlapping ranges. Streaming was faster than either whole-buffer path on all three Linux fixtures. Local M5 mixed extraction was 54.64 [50.05–57.91] versus 67.49 [54.95–75.16] ms. Those ranges also overlap, and no benefit is established.

## Decision

Retain **only on Windows**. Linux and Mac retain whole writes.

The change fixes the whole-buffer write regression in native-fast. It does not make native-fast faster than streaming on every input.

The default shared budget stays 64 MiB on Windows. The mixed and random inputs here still stream unless the user increases it, so their 6.72× and 6.16× gains need a larger budget. The compressible input fits the default budget, so native-fast users get the 5.38× gain without increasing the budget. On that input the streaming `native` reader remains faster than either whole-buffer path.

The production change is conditional on Windows. It executes the same chunk loop as the measured Windows candidate. Full Rust tests, formatting, Clippy and direct release byte checks pass. The experiment's Windows release tests exercise chunk boundaries and final partial chunks through existing large-entry, CRC-failure and cleanup tests. Validation still completes before buffered output is written, and failed extraction is never published.

## Limits

These are end-to-end extraction gains against the old whole-buffer path, not universal speedups over streaming. Of these three fixtures, only the compressible one uses the chunked path under the default 64 MiB Windows budget. Streaming beat it there. The experiment identifies a beneficial write size on this hosted Windows system. It does not prove a kernel or antivirus cause. Do not multiply this gain by unrelated small-file reader or cache improvements.

## Reproduce

Reports retain every sample, observed range, memory high-water mark, binary and archive hash, command and run order. The original candidate patch is kept alongside the reports. Reproduction requires the recorded baseline, from before the production conditional loop was added.

The optional higher-memory mode looks like this:

```powershell
# Optional higher-memory mode; the normal Windows default remains 64 MiB.
zipper --zip-engine native-fast --zip-buffer-mib 128 extract files.zip -o restored
```
