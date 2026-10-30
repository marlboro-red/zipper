# Rejected: switch buffered ZIP checksum to libdeflate CRC

Hypothesis: the existing libdeflate checksum implementation might reduce checksum
CPU cost without changing the ZIP CRC contract or entry memory budget.
Baseline `0169f9b`; the one-line candidate is retained in `candidate.patch`.
Apple M5, four workers, warm filesystem caches, native-fast, unchanged default
128 MiB shared entry budget. Interleaved release CLI runs with direct byte/tree
checks after every extraction; process start and output included, durability not
requested. Nine measured Silesia rounds and seven dependency-tree rounds after
warmup. Raw samples include sizes, hashes, memory and ordering.

| Workload | ZIP bytes | Before median [range] ms | After median [range] ms | Peak MiB before / after |
|---|---|---|---|---|
| Silesia | 70,252,453 | 92.04 [85.15, 93.71] | 89.09 [83.17, 99.74] | 187.61 / 188.14 |
| Rust dependencies | 14,325,991 | 177.60 [168.93, 207.42] | 173.18 [165.69, 199.06] | 8.42 / 8.34 |

Ranges overlap: no established extraction gain. Reverted, with no production
change. This does not establish which checksum implementation is faster alone.

`profile.txt` is a separate untimed three-second sample of 80 repeated Silesia
native-fast extractions (`examples/profile_zip.rs`), four workers. Sampled leaf
counts include 2,521 libdeflate decode, 1,157 write, 784 zlib-rs fast inflate and
145 crc32fast update; these are samples across threads, not elapsed-time shares.
The profile includes startup and may include cleanup; it is diagnostic evidence,
not a benchmark. Decode and output are larger targets than checksum selection.
