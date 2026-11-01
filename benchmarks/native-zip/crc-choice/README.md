# Buffered ZIP checksums with libdeflate CRC

Switching the buffered ZIP checksum to libdeflate CRC gave no established extraction gain. The change was reverted with no production change.

## Question

Can the existing libdeflate checksum implementation reduce checksum CPU cost? The change must keep the ZIP CRC contract and the entry memory budget.

## Setup

Baseline `0169f9b`. `candidate.patch` keeps the one-line candidate.

Apple M5, four workers, warm filesystem caches, native-fast, and the unchanged default 128 MiB shared entry budget. Interleaved release CLI runs check bytes and the tree directly after every extraction. Timings include process start and output. Durability was not requested.

Nine measured Silesia rounds and seven dependency-tree rounds follow warmup. Raw samples include sizes, hashes, memory and ordering.

## Results

| Workload | ZIP bytes | Before median [range] ms | After median [range] ms | Peak MiB before / after |
|---|---|---|---|---|
| Silesia | 70,252,453 | 92.04 [85.15, 93.71] | 89.09 [83.17, 99.74] | 187.61 / 188.14 |
| Rust dependencies | 14,325,991 | 177.60 [168.93, 207.42] | 173.18 [165.69, 199.06] | 8.42 / 8.34 |

The ranges overlap.

### Profile

`profile.txt` is a separate, untimed three-second sample of 80 repeated Silesia native-fast extractions (`examples/profile_zip.rs`) with four workers. Sampled leaf counts include 2,521 libdeflate decode, 1,157 write, 784 zlib-rs fast inflate and 145 crc32fast update. These are samples across threads, not elapsed-time shares.

## Decision

Reject. The overlapping ranges show no established extraction gain, so the candidate was reverted. The profile suggests that decode and output are larger targets than checksum selection.

## Limits

This does not establish which checksum implementation is faster alone. The profile includes startup and may include cleanup. It is diagnostic evidence, not a benchmark.

## Reproduce

No reproduction command was recorded. Apply `candidate.patch` to baseline `0169f9b` to rebuild the candidate.
