# Roadmap

This phase ends when Zipper's in-process reader replaces 7-Zip as the default ZIP engine. It must be correct on hostile input, restore the metadata users expect, and extract faster with bounded memory. Speed work starts on macOS. Every correctness check runs on macOS, Linux and Windows in CI.

Each milestone closes only when its exit checks pass. A check is a gate step, a test or an experiment record, never an opinion. [The design page](design.md#implementation-proceeds-through-five-ordered-gates) describes how extraction experiments proceed.

## Milestones

| Milestone | Status | Depends on |
| --- | --- | --- |
| [M0. Verified workflow](#m0-verified-workflow) | Done | None |
| [M1. Correctness contract for ZIP](#m1-correctness-contract-for-zip) | Next | M0 |
| [M2. Metadata fidelity](#m2-metadata-fidelity) | Planned | M1 |
| [M3. Fixed scoreboard](#m3-fixed-scoreboard) | Planned | M0 |
| [M4. Small-file output path](#m4-small-file-output-path) | Planned | M3 |
| [M5. Large single entries](#m5-large-single-entries) | Planned | M3 |
| [M6. Native reader as the default](#m6-native-reader-as-the-default) | Planned | M1, M2 |

M6 can close as soon as M1 and M2 do. M4 and M5 make the default faster and can continue after it.

## M0. Verified workflow

The repository enforces its own rules.

- `cargo xtask gate fast` runs format, lint, tests, a release build and a byte-exact round trip. The pre-push hook runs it.
- `cargo xtask gate full` adds release tests and a benchmark smoke run. CI runs it on all three operating systems.
- `cargo xtask lint-docs` checks writing rules, links, record headings, reachability and every path a page names.
- Benchmark scripts record CPU time and refuse comparisons without a 1-thread Zipper row.
- The skills in `.claude/skills/` cover benchmarking, review, documentation and reflection.

## M1. Correctness contract for ZIP

The native reader extracts the same bytes as a reference extractor and survives hostile archives.

Scope:

- A reference comparison that extracts each corpus archive with Zipper, bsdtar and 7-Zip and compares the trees.
- A corpus of real producer output. It covers Java `jar`, Windows Explorer, macOS Archive Utility, Info-ZIP `zip`, Python `zipfile` and 7-Zip. It includes ZIP64 by size and by entry count, data descriptors and archive comments.
- A hostile collection with a test for each case. It covers path traversal, absolute paths, drive letters, duplicate names, names that collide by case, overlapping entries, a zero declared size hiding real data, local and central headers that disagree, truncation and inconsistent ZIP64 fields.
- Fuzz targets for the central-directory and local-header parsers.
- Local headers that agree with the central directory, and records that do not overlap. Done in the [local-header check](../benchmarks/native-zip/local-header-check/README.md).
- Unicode path extra fields (`0x7075`) that rename an entry are rejected, so every reader uses the same name. Done in the [Unicode path field check](../benchmarks/native-zip/unicode-path/README.md).
- Records that tile the archive, with checked data descriptors, the local encryption flag and local ZIP64 sizes. Done in the [record layout check](../benchmarks/native-zip/record-layout/README.md).
- Deflate streams that end exactly at their compressed size, and stored entries that cannot hide a descriptor. Done in [exact decoding](../benchmarks/native-zip/exact-decoding/README.md).
- A limit on total extracted bytes, with a documented default.

Exit checks:

1. The reference comparison finds no byte mismatch on any corpus archive.
2. Every hostile case is rejected or stays inside the destination, and its test fails if the check is removed.
3. A short fuzz replay runs in `cargo xtask gate full`. Longer fuzz runs find no crash.
4. Each parser change passed the `zipper-review` skill.

## M2. Metadata fidelity

The native reader restores what users expect from a ZIP extractor.

Scope:

- Unix permission bits for entries made on Unix, masked to `0o777`.
- Modification times from the DOS fields and the extended timestamp field. Directory times are set after their children.
- A symlink policy, decided and documented before implementation.
- Filename decoding for the UTF-8 flag and for CP437. Accept a Unicode path field whose UTF-8 name equals the CP437-decoded stored name, which M1 rejects.
- A Unicode normalization policy. Zipper keeps stored name bytes today, while bsdtar on macOS rewrites names to NFD.

Exit checks:

1. The reference comparison also compares permissions and modification times.
2. `docs/reference/zip-reader.md` lists exactly what is restored and what is not.
3. Benchmarks compare against default bsdtar, because both sides now do the same metadata work.

## M3. Fixed scoreboard

One command measures every tracked workload the same way and publishes the numbers.

Scope:

- A new `bench` subcommand of `cargo xtask` runs fixed workloads. They are 10,000 flat tiny files, 4,000 grouped files, the dependency corpus, a Silesia ZIP, one large compressible entry, one large random entry and a mixed tree.
- Every run includes the parity rows, CPU time and peak memory.
- The command generates `docs/results.md` from the committed JSON.
- Instrumentation reports decoder CPU, bytes read and written, and output operations, outside timed runs.

Exit checks:

1. `docs/results.md` is generated, and the gate fails when it does not match the committed JSON.
2. Two sessions on one host record the session-to-session drift for each workload.
3. One command reproduces every published number.

## M4. Small-file output path

Extraction of many small files gets faster at matched threads and CPU, starting on macOS.

Scope:

- Trace macOS extraction outside timed runs and name the limiter before changing code.
- Restate the 3× target against reduced-metadata bsdtar at matched parity before the first experiment.
- Test one change at a time, following the design gates. Replicate on Linux and Windows after macOS.

Exit checks:

1. A kept change beats the spread on the M3 small-file workloads at matched threads, with CPU time reported.
2. Linux and Windows show no regression on the same workloads.

## M5. Large single entries

One large Deflate entry extracts faster without unbounded memory.

Scope:

- Parallel decoding of a single Deflate entry above a size threshold, with streaming as the fallback.
- A memory-mapped input experiment, with a fallback when the archive changes during extraction.

Exit checks:

1. A single-entry speedup beats the spread, with CPU time and peak memory reported.
2. The memory bound is documented and tested, and CRC checks still cover every byte.
3. Hostile Deflate streams pass the `zipper-review` skill.

## M6. Native reader as the default

`zipper extract` uses the in-process reader for ZIP without any flag.

Scope:

- Make the native reader the default `--zip-engine` and choose between `native` and `native-fast` from M3 data.
- Keep 7-Zip for formats other than ZIP.
- Update the Windows package and the docs.

Exit checks:

1. Default ZIP extraction runs in-process on all three operating systems in CI.
2. The M1 and M2 checks pass on all three operating systems.
3. `docs/usage.md` and `docs/reference/zip-reader.md` describe the new default.

## Close a milestone

1. Confirm every exit check with its gate step, test or record.
2. Set the milestone's status to Done in this page.
3. Run the `zipper-reflect` skill and commit what it encodes.
