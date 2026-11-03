# Extraction design

Zipper's next objective is **complete, verified extraction**, including creating
the output tree. Decoder throughput is a component metric. This page explains the
extraction direction set by the review of 30 October 2026, and why it looks the way
it does. Zipper reads ZIP, tar and 7z; it no longer has a format of its own.

The review reached these conclusions:

- The requested 3× faster small-file extraction remains unachieved. The grouped
  Linux fixtures exceed the threefold target in matched experiments. The original
  local macOS small-file target remains unmet.
- Removing decoding from the current output implementation does not approach 3×.
  Output work has to be executed differently.
- The recent 10–16× gain in ZIP integrity verification is not an extraction gain.
- Execution is rebuilt around a validated extraction plan, one measured seam at a
  time.

## Retained changes improve the input path only

[Portable measurements](../benchmarks/extraction-matrix/README.md) now cover Linux
and macOS, plus a smaller Windows diagnostic. The review kept three changes:

- [shared fast-ZIP entry-buffer accounting](../benchmarks/native-zip/shared-budget/README.md)
- [Linux extraction caching](../benchmarks/native-zip/linux-cache/README.md)
- [reduced input read-ahead](../benchmarks/native-zip/input-buffer/README.md)

These are bounded input-path improvements, not the full executor and output [redesign](#a-validated-plan-drives-execution)
described below. Descriptions of the old per-worker budget on this page refer to the
review baseline.

## Output work, not decoding, limits small-file extraction

On the local M5, with four workers and seven measured rounds, extracting 4,000
grouped files took 122.37 ms. Writing their already-decoded bytes from RAM took
117.72 ms. For 10,000 flat files, those medians were 318.49 and 328.71 ms. These
pipelines overlap, so you cannot subtract one from the other to assign decoder
time. They do show that removing decoding from the current output implementation
does not approach 3×. The RAM writer is a reference implementation, **not a physical
lower bound**.
[Raw evidence and rejected experiments](../benchmarks/native-zip/threefold/README.md)
are recorded with the measurements.

The corresponding 3× targets are 40.79 ms and 106.16 ms. That is approximately
98,000 and 94,000 output files per second. Compare each target with its baseline
again in the same session. Reaching it needs a substantial change in how output
work is executed. The alternative is evidence that the target cannot be reached on
a specified platform under the same semantics. Another buffer-size tweak is not a
sufficient strategy.

Several attempts failed, and the failures carry information:

- Four workers outperform ten on these local small-file fixtures.
- Separating a serial creator from writers made things worse.
- Positional reads, directory batching, and empty-file cloning did not demonstrate
  useful extraction gains.

These results argue against assuming that more queues or asynchronous code are
inherently faster. No current result establishes Windows extraction performance.

## A validated plan drives execution

Rebuild execution around a validated extraction plan, and introduce each seam only
when an experiment needs it. The plan describes directory dependencies, file
extents, decode units, expected sizes and checksums, and metadata. A ZIP decode
unit is normally one entry. A 7z unit is one folder, or one LZMA2 segment of it.
Format adapters keep their validation rules rather than forcing all formats into
an unsafe common parser.

### Four resources need separate limits

The executor should account for four distinct resources:

- CPU workers for decoding and checksums, under one total CPU budget.
- Compressed and decoded buffers under one byte budget, including queued work.
- Outstanding output operations and open descriptors under separate limits.
- Metadata and index storage and format limits, reported separately from buffer use.

Today a per-worker fast-ZIP allocation cap and the tar and 7z budgets do not
constitute a process-wide resource contract. Reserve buffer capacity before
scheduling work, and release it only after all consumers finish. Streaming fallback
must work when a whole-buffer decode does not fit. Describe an allocation budget
honestly. It is not an exact RSS limit once libraries, stacks, and allocators count.
[libdeflate's whole-buffer API](https://github.com/ebiggers/libdeflate) makes this
tradeoff explicit.

### Buffers are owned and reused

Decode into owned reusable buffers, then pass bounded references to output extents.
Avoid additional copies where lifetimes allow, but measure bytes copied before
adding complex lifetime machinery. Let scheduling balance input locality, directory
contention, and completion of large files. A filesystem I/O limit must be
independent of CPU count. Test a simple calibrated policy before an adaptive
controller. The present macOS four-worker heuristic is evidence for two fixtures,
not a portable scheduling law.

### Safety invariants do not change

These invariants remain:

- private staging and exclusive creation
- path validation
- exact length and checksum checks
- publication only after every task succeeds

Failure must drain or cancel work safely and clean up. Rename publication is not a
claim of crash durability. Add metadata fidelity and output-size limits to the
product contract. The fast ZIP reader currently preserves bytes and directories
only, and the backend remains the default. A benchmark-only fast option is not yet
the user-visible extraction engine we intend to ship.

## Each workload gets its own research track

| Workload | Hypothesis to test | Decisive measurement |
| --- | --- | --- |
| Many tiny files | Namespace operations, filesystem contention or filter overhead dominate; an OS-specific output implementation can reduce them | RAM-to-tree and full extraction, files/s, syscall/CPU traces, bounded RSS, same metadata |
| A few large ZIP entries | Entry-level scheduling leaves cores idle inside a large Deflate stream | Single-entry and mixed-size ZIPs, decode-only and extraction, memory and correctness |
| Native mixed trees | Block layout and scheduling can balance compression locality with output readiness | Size/time/RSS frontier plus selected-file read amplification |
| Duplicate-rich trees | Shared content can reduce stored bytes and repeated decoding | Unique bytes decoded, output time and archive size including dedup construction cost |

### Output backends start from a platform trace

For Linux, prototype batched open, write, and close operations with io_uring only
after a trace identifies syscall or descriptor overhead. Its direct descriptors
avoid some shared file-table overhead. They do not remove filesystem namespace
work. Maintain bounded queue depth and a synchronous fallback. This is a
hypothesis, not a predicted speedup.
[Upstream openat/direct-descriptor documentation](https://github.com/axboe/liburing/blob/master/man/io_uring_prep_openat.3).

For Windows, collect ETW traces on ordinary NTFS before choosing a new backend.
Record storage, filesystem, antivirus and filter configuration, and cache state.
Test Dev Drive separately. It uses ReFS and different filesystem and security
settings, so do not pool its results with NTFS or attribute them solely to Zipper.
Do not change antivirus settings to manufacture a win.
[Microsoft Dev Drive documentation](https://learn.microsoft.com/en-us/windows/dev-drive/).

For macOS, investigate APFS metadata and thread contention in system traces. Keep
the current simple parallel writer as the baseline to beat.

### Large ZIP entries need parallel Deflate decoding

Parallel Deflate decoding for large ZIP entries is a distinct algorithmic project.
Rapidgzip demonstrates speculative parallel decoding of arbitrary gzip data. It
resolves unknown history through a cache and prefetch design. It is relevant prior
art, not an already-integrated ZIP decoder. Raw Deflate entry boundaries,
cancellation, bounded memory, CRC validation, and malicious inputs would all need
proof and tests. Keep libdeflate or streaming fallback when parallel startup costs
exceed the benefit. [Rapidgzip paper](https://arxiv.org/abs/2308.08955).

### Mounting is a separate product

Mounting an archive is a separate product opportunity when the actual need is fast
access to a few files. DwarFS supplies relevant prior art for a mountable,
deduplicating filesystem. Measure time to first useful read and subsequent workload
latency there. Never report mount time as full extraction time.
[DwarFS](https://github.com/mhx/dwarfs).

## Implementation proceeds through five ordered gates

[The roadmap](roadmap.md) turns these gates into milestones with exit checks.

1. **Establish portable evidence first.** Extend the rerunnable extraction benchmark
   to Windows. Collect real source and application trees alongside synthetic size
   sweeps. Include flat and deep layouts, empty files, 100k files, large
   single-entry ZIPs, duplicate-rich data, and incompressible inputs. Million-entry
   ZIP experiments require explicitly revisiting the current 100k-entry safety
   limit. Do not silently bypass it. Preserve corpus manifests and input hashes.
2. **Instrument the critical path.** Record planning time, decoder CPU, queue wait,
   bytes copied, read, and written, active output operations, and peak memory.
   Collect filesystem traces outside timed runs. Stage times overlap, so their sum
   is not wall time. Compare no-output decode, RAM materialization, and full
   extraction.
3. **Prove an output improvement before integrating it.** Implement the smallest
   platform-specific diagnostic justified by the trace. Require exact tree and byte
   checks, failure cleanup, and repeatable end-to-end improvement before putting it
   behind the production sink interface. Rejected prototypes stay rejected unless a
   new workload or concrete mechanism changes the hypothesis.
4. **Introduce shared resource accounting incrementally.** Use fixed memory tiers
   and explicit CPU and I/O limits. Compare against current RSS and speed.
   Concurrency changes require independent review. Do not attempt a broad executor
   rewrite without a workload showing why each new mechanism is needed.
5. **Pursue parallel Deflate separately.** Each needs its own
   controlled baseline and fallback. Do not mix codec, scheduler, and file-output
   changes in one experiment, or attribute their combined result to one feature.

Extraction comparisons follow the rules of the [experiment log](../benchmarks/README.md),
with these extraction-specific rules:

- Compare identical archives against current 7-Zip and libarchive.
- Match bytes, metadata policy, and durability.
- Report warm and controlled cold-cache runs separately. If cold state cannot be
  established, label it unknown.
- Include whole process-tree memory for subprocess pipelines.
- Publish raw samples, medians, ranges, archive size, files/s, and MiB/s. Use
  enough runs before claiming tail percentiles.
- Re-run baselines interleaved on the same host.

A credible first milestone is a reproducible extraction frontier on named real
workloads on Windows and macOS. It needs Linux replication, bounded resources, and
complete supported metadata. The 3× goal remains an experiment target, not an
estimate. State-of-the-art requires beating strong contemporary baselines under
these contracts and making the evidence independently reproducible.
