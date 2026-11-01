# Threefold small-file extraction target

**The threefold small-file extraction target is not achieved.** A separate, substantial integrity-verification improvement is retained, and it must not be described as an extraction gain.

## Question

Can small-file ZIP extraction become three times faster than native Zipper at source ae0e59a?

## Setup

Apple M5, four workers, seven measured rounds after warmup.

The cost-separation diagnostic loads source bytes into memory before timing. It alternates native extraction, integrity-only native verification, and creation of the same output tree directly from those bytes. Materialization includes exclusive file creation, writes, staging, and rename, but no ZIP parsing or checksum work. After timing, every materialized and extracted tree is checked for exact bytes and membership. This measures one filesystem implementation, not a theoretical lower bound. The diagnostic is `examples/small_zip_costs.rs`, and raw CSVs retain all samples.

The verification benchmark separately extracts and directly compares every file after each timed verification call.

The output experiments isolate file materialization from archive decoding. A bounded queue of 64 open files separates a serial creator from one or three writers. The three-writer variant has four active workers including the creator. The existing reference uses four parallel create, write and close workers. All bytes and tree membership are checked after each run.

## Results

### Extraction cost separation

| Fixture | Native extraction ms | RAM-to-files materialization ms | 3× extraction target ms |
| --- | ---: | ---: | ---: |
| 4,000 grouped files | 122.37 | 117.72 | 40.79 |
| 10,000 flat tiny files | 318.49 | 328.71 | 106.16 |

Filesystem creation and writes dominate this workload. A faster decoder alone is unlikely to deliver the extraction target. A materially faster output strategy still needs to be demonstrated.

### Cached native-fast integrity verification

Integrity-only native-fast reads now snapshot ZIP archives up to 8 MiB after preflight validation. Independent cursors share the immutable bytes. This avoids repeated buffered file reads, seeks, and input locking for thousands of small entries. All size and CRC checks remain enabled. Extraction and the low-memory native streaming mode keep the existing file reader, and so do archives over the limit.

| Fixture | Before ms [range] | After ms [range] | Speedup | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: | ---: |
| 4,000 grouped | 79.47 [75.92–80.85] | 7.48 [7.29–7.71] | 10.63× | 6.78 / 7.08 |
| 10,000 flat | 154.14 [150.15–175.09] | 9.30 [9.19–9.80] | 16.58× | 9.64 / 14.92 |

The [extraction-cache experiment](cache-extract.json) changed the grouped median from 128.94 to 124.43 ms, but ranges overlapped. The grouped verification report was collected before the cache was restricted to verification. Its verification code path is unchanged. The flat report uses the final restricted implementation.

### Output pipeline

| Fixture | Parallel reference ms | Creator + 3 writers ms | Creator + 1 writer ms |
| --- | ---: | ---: | ---: |
| 4,000 grouped | 119.98 | 142.27 | 142.16 |
| 10,000 flat | 322.15 | 379.80 | 378.52 |

Separating output stages is slower on both fixtures. The raw CSVs include all seven runs, warmup excluded.

### Cloned empty files

A second diagnostic cloned a private empty-file template for the 2,000 empty files in the flat fixture, using macOS `fclonefileat`. Other files kept the reference write path. Its median was 342.08 ms versus 322.38 ms for ordinary creation, with overlapping ranges.

### Duplicate content

[Duplicate-content checks](../../../docs/reference/duplicates.md) found no redundant payload bytes in either fixture.

## Decision

Retain cached native-fast integrity verification. Because the extraction-cache ranges overlapped, the cache is restricted to verification-only reads. Independent code review found no correctness or concurrency blocker. Tests cover empty, cache-boundary and fallback inputs, independent concurrent cursors, malformed ZIPs, CRCs and decoded lengths.

Reject the split output pipeline, because it is slower on both fixtures. Reject the empty-file clone, because it gave no reliable speed gain. Source and data checks pass for the clone. Full metadata-equivalence and unsupported-volume fallback work were not pursued, because the performance gate failed. No production I/O changes or extra dependencies were retained.

The threefold extraction target remains unmet. To reach it on these fixtures, the next successful change must significantly reduce file-materialization cost.

## Limits

The 8 MiB bound covers retained archive data, not peak process memory. Vector capacity growth and conversion to shared storage can transiently allocate additional memory. Metadata and decoder buffers are additional. Peak RSS above includes the observed process allocation costs.

These are local warm-cache results, not Windows measurements. This evidence does not establish that the goal is impossible. It also does not justify weakening extraction or integrity semantics.

## Reproduce

```sh
zipper --zip-engine native-fast test files.zip

cargo build --release --example small_zip_costs
 target/release/examples/small_zip_costs ARCHIVE.zip SOURCE_DIRECTORY 7 4
python3 scripts/compare_zip_readers.py ARCHIVE.zip SOURCE_DIRECTORY \
  --reader before native-fast BEFORE_BINARY --reader after native-fast AFTER_BINARY \
  --operation test --runs 7 --threads 4
```

[The diagnostic patch](rejected-output-prototypes.patch) for the output experiments applies with `git apply --unidiff-zero` to an isolated checkout of `573037e`. Build the example and run it with the usual archive and source arguments. The optional final argument `materialize,clone-empty` selects the macOS clone trial. The patch includes the macOS-only development dependency needed for that diagnostic.
