# Shared native-fast ZIP entry-buffer budget

A shared entry-buffer budget cut extraction time for one large Deflate entry by 24.7% (1.33×), at a substantial memory cost. It is kept, with a default of 128 MiB on ARM64 macOS and 64 MiB elsewhere.

## Question

The old limit was 64 MiB per worker. Can a shared live-buffer budget admit a large single Deflate entry that the old limit sent to streaming? Can it also bound aggregate entry buffers independently of worker count? This is an extraction experiment, not the earlier integrity-verification cache result.

## Setup

The implementation defaults to 128 MiB **per extraction invocation** across all workers. Each eligible entry atomically reserves compressed plus decoded bytes before either buffer allocation. A nonblocking reservation failure uses streaming, so workers never wait for memory while holding other resources. RAII releases the reservation after both buffers are dropped, including on errors. Allocation is fallible. `--zip-buffer-mib N` overrides the budget, and zero forces streaming.

Metadata, worker state, the verification archive cache, allocator-retained pages and concurrent independent library invocations are outside this budget. It is not an RSS ceiling. Scheduling can affect which entries get whole-buffer decoding.

The measurement uses seven interleaved measured rounds after warmup, four workers, Apple M5 and an uncontrolled warm cache. Before is source `4226bc4`. After is the shared-budget implementation committed with this report. Reports retain binary and archive hashes, all samples, run order, memory and direct byte verification. Each pair uses the same input ZIP bytes.

The synthetic entry repeats a block 1,024 times. The block contains 32 KiB of deterministic random bytes plus 29 KiB of repeated text. Its construction is in the portable extraction workflow.

## Results

| Workload | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| One 61 MiB entry | 62.48 [62.20–62.94] | 47.03 [46.83–49.94] | 3.11 / 96.28 |
| Silesia, 12 files | 92.54 [91.21–104.88] | 88.67 [79.07–98.26] | 174.00 / 189.23 |
| 10,000 tiny flat files | 337.33 [310.06–349.43] | 347.56 [338.89–367.51] | 9.75 / 9.75 |

The large entry takes **24.7% less extraction time (1.33×)**. Silesia and tiny-file ranges overlap.

A separate baseline matrix found roughly 61 ms for both old native modes at 1/2/4/8 threads. It found 41 ms for tuned bsdtar on ZIP and 11 ms for plain TAR. Those measurements precede the change and come from a different session.

### Cross-platform default correction

The subsequent three-round portable matrix at source `cc8aced` tested the same 61 MiB entry. Whole-buffer mode was slower on hosted x86-64 Linux, with native-fast4 at 143.42 ms versus native4 at 124.21 ms. It was much slower on Windows, with native-fast4 at 715.22 ms versus native4 at 122.86 ms. These are engine comparisons, not matched before and after builds. They still rule out recommending the widened eligibility there. See the [portable reports](../../extraction-matrix/README.md).

A subsequent [Windows write-size experiment](../windows-write-size/README.md) identified large output writes as the main cause of the measured Windows penalty. Bounded writes now greatly improve that path, without increasing the default budget. The engine timings above describe the pre-fix implementation.

## Decision

Keep for the targeted large-entry result, with a substantial disclosed memory tradeoff. Claim neither an improvement nor an established regression for Silesia and tiny files.

The final default is **128 MiB for ARM64 macOS builds and 64 MiB elsewhere**. Explicit budgets remain available. An x86-64 macOS binary under Rosetta receives 64 MiB. The conservative default sends the 61 MiB entry back to streaming on Linux and Windows.

Validation covered full Rust tests, formatting, Clippy and direct release byte round trips. It also covered concurrent budget exhaustion, release and unwind, and zero, tiny, contended and sufficient budgets. CRC failure cleanup and CLI zero-budget extraction were tested too. Independent review found no blocker. It also exercised hostile ZIP64 metadata claiming 2^63 output bytes with an unlimited library budget, and allocation returned an error, not a panic.

## Limits

This does not achieve the requested threefold small-file speedup. Peak RSS can increase despite the shared bound, because larger entries now qualify and freed allocations may remain resident. Use `native` or a zero buffer budget for low memory.

The new result is not evidence that Zipper now beats bsdtar on this workload. This is the first limited resource-accounting change from the extraction review. It does not replace the output executor or implement parallel decoding within a single Deflate entry.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py ARCHIVE.zip SOURCE_DIRECTORY \
  --reader before native-fast BEFORE_BINARY \
  --reader after native-fast target/release/zipper --runs 7 --threads 4
zipper --zip-engine native-fast --zip-buffer-mib 32 extract ARCHIVE.zip -o NEW_DIRECTORY
```
