# Investigating a threefold speed target

Target interpreted as threefold faster small-file ZIP extraction than native Zipper at source ae0e59a. **The extraction target is not achieved.** A separate, substantial integrity-verification improvement is retained; it must not be described as an extraction gain.

## Extraction cost separation

Apple M5, four workers, seven measured rounds after warmup. The diagnostic loads source bytes into memory before timing and alternates native extraction, integrity-only native verification, and creation of the same output tree directly from those bytes. Materialization includes exclusive file creation, writes, staging, and rename but no ZIP parsing or checksum work. Every materialized/extracted tree is checked for exact bytes and membership after timing. This measures one filesystem implementation, not a theoretical lower bound.

| Fixture | Native extraction ms | RAM-to-files materialization ms | 3× extraction target ms |
| --- | ---: | ---: | ---: |
| 4,000 grouped files | 122.37 | 117.72 | 40.79 |
| 10,000 flat tiny files | 318.49 | 328.71 | 106.16 |

Filesystem creation and writes dominate this workload. A faster decoder alone is unlikely to deliver the extraction target; a materially faster output strategy still needs to be demonstrated. Raw CSVs retain all samples. The diagnostic is `examples/small_zip_costs.rs`.

## Retained: cached native-fast integrity verification

Integrity-only native-fast reads now snapshot ZIP archives up to 8 MiB after preflight validation, sharing immutable bytes among independent cursors. This avoids repeated buffered file reads, seeks, and input locking for thousands of small entries. All size and CRC checks remain enabled. Extraction and the low-memory native streaming mode retain the existing file reader. Archives over the limit also retain that reader.

| Fixture | Before ms [range] | After ms [range] | Speedup | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: | ---: |
| 4,000 grouped | 79.47 [75.92–80.85] | 7.48 [7.29–7.71] | 10.63× | 6.78 / 7.08 |
| 10,000 flat | 154.14 [150.15–175.09] | 9.30 [9.19–9.80] | 16.58× | 9.64 / 14.92 |

The 8 MiB bound covers retained archive data, not peak process memory. Vector capacity growth and conversion to shared storage can transiently allocate additional memory. Metadata and decoder buffers are additional. Peak RSS above includes the observed process allocation costs. These are local warm-cache results, not Windows measurements.

The [extraction-cache experiment](cache-extract.json) changed the grouped median from 128.94 to 124.43 ms, but ranges overlapped. Consequently the cache was restricted to verification-only reads. The grouped verification report was collected before that scope restriction; its verification code path is unchanged. The flat report uses the final restricted implementation.

Independent code review found no correctness/concurrency blocker. Tests cover empty/cache-boundary/fallback inputs, independent concurrent cursors, malformed ZIPs, CRCs and decoded lengths. The benchmark separately extracts and directly compares every file after each timed verification call.

```sh
zipper --zip-engine native-fast test files.zip

cargo build --release --example small_zip_costs
 target/release/examples/small_zip_costs ARCHIVE.zip SOURCE_DIRECTORY 7 4
python3 scripts/compare_zip_readers.py ARCHIVE.zip SOURCE_DIRECTORY \
  --reader before native-fast BEFORE_BINARY --reader after native-fast AFTER_BINARY \
  --operation test --runs 7 --threads 4
```

To reach threefold faster extraction on these fixtures, the next successful change must significantly reduce file-materialization cost. This evidence does not establish that the goal is impossible, nor justify weakening extraction or integrity semantics.
