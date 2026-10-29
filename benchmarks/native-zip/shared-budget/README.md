# Shared native-fast ZIP entry-buffer budget

Hypothesis: a shared live-buffer budget can admit a large single Deflate entry
that the old 64 MiB per-worker limit sent to streaming, while bounding aggregate
entry buffers independently of worker count. This is an extraction experiment,
not the earlier integrity-verification cache result.

The implementation defaults to 128 MiB **per extraction invocation** across all
workers. Each eligible entry atomically reserves compressed plus decoded bytes
before either buffer allocation. A nonblocking reservation failure uses streaming;
workers never wait for memory while holding other resources. RAII releases the
reservation after both buffers are dropped, including on errors. Allocation is
fallible. `--zip-buffer-mib N` overrides the budget; zero forces streaming.
Metadata, worker state, verification archive cache, allocator-retained pages and
concurrent independent library invocations are outside this budget. It is not an
RSS ceiling. Scheduling can affect which entries get whole-buffer decoding.

Seven interleaved measured rounds after warmup, four workers, Apple M5, uncontrolled
warm cache. Before is source `4226bc4`; after is the shared-budget implementation
committed with this report. Reports retain binary/archive hashes, all samples,
run order, memory and direct byte verification. Same input ZIP bytes per pair.

| Workload | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| One 61 MiB entry | 62.48 [62.20–62.94] | 47.03 [46.83–49.94] | 3.11 / 96.28 |
| Silesia, 12 files | 92.54 [91.21–104.88] | 88.67 [79.07–98.26] | 174.00 / 189.23 |
| 10,000 tiny flat files | 337.33 [310.06–349.43] | 347.56 [338.89–367.51] | 9.75 / 9.75 |

Keep for the targeted large-entry result: **24.7% less extraction time (1.33×)**,
with a substantial disclosed memory tradeoff. Silesia and tiny-file ranges overlap;
claim neither an improvement nor an established regression there. In particular,
this does not achieve the requested threefold small-file speedup. Peak RSS can
increase despite the shared bound because larger entries now qualify and freed
allocations may remain resident. Use `native` or a zero buffer budget for low memory.

The separate baseline matrix found roughly 61 ms for both old native modes at
1/2/4/8 threads, 41 ms for tuned bsdtar on ZIP and 11 ms for plain TAR. Those
measurements precede the change and come from a different session; the new result
is not evidence that we now beat bsdtar on this workload. The synthetic entry
repeats a block containing 32 KiB deterministic random bytes plus 29 KiB repeated
text 1,024 times; its construction is in the portable extraction workflow.

Validation: full Rust tests, formatting, Clippy, direct release byte round trips,
concurrent budget exhaustion/release/unwind, zero/tiny/contended/sufficient budgets,
CRC failure cleanup, and CLI zero-budget extraction. Independent review found no
blocker and additionally exercised hostile ZIP64 metadata claiming 2^63 output
bytes with an unlimited library budget: allocation returns an error, not a panic.

```sh
python3 scripts/compare_zip_readers.py ARCHIVE.zip SOURCE_DIRECTORY \
  --reader before native-fast BEFORE_BINARY \
  --reader after native-fast target/release/zipper --runs 7 --threads 4
zipper --zip-engine native-fast --zip-buffer-mib 32 extract ARCHIVE.zip -o NEW_DIRECTORY
```

This is the first limited resource-accounting change from the extraction review.
It does not replace the output executor or implement parallel decoding within a
single Deflate entry.
