# Zipper

[Local performance baseline and reproduction steps](benchmarks/README.md).
[Research review and frontier experiments](docs/research-2026-10.md).
[Comparison against tar+zstd and tar+pzstd](benchmarks/zstd-comparison.md).
[V2 results and experiment decisions](benchmarks/v2/README.md).
[Matched-sync v2 comparison against tar+Zstandard](benchmarks/v2/zstd-fullsync.md).
[Decompression/ratio Pareto curve, including classic formats](docs/performance.md).

A Rust archive engine pursuing fast compression and decompression at competitive
compression ratios. This is an experimental foundation, not a performance leader
yet. Native archives use the versioned `.zipper` format.

```sh
cargo build --release
target/release/zipper create backup.zipper ./data
target/release/zipper extract backup.zipper --output restored
target/release/zipper extract backup.zipper --output selected --path data/subdir
target/release/zipper list backup.zipper
target/release/zipper test backup.zipper
target/release/zipper --threads 4 create backup.zipper ./data --level 1
target/release/zipper --threads 4 create dense.zipper ./data --level 6 --block-size-kib 4096
```

The archive must be outside the input tree. Creation never overwrites an archive;
extraction requires a new destination directory with an existing parent.

Find identical file contents before archiving:

```sh
zipper duplicates ./data ./other-data
zipper --threads 4 duplicates --summary ./data
```

Matches are confirmed byte for byte. Overlapping input paths are counted once;
symlinks and special files are skipped. The report is read-only. Duplicate payload
bytes describe logical content, not reclaimable disk space (hard links and
filesystem compression matter). Keep inputs unchanged during the scan.
[Duplicate-checking details](docs/duplicates.md). Archive-level deduplication is
not implemented yet.

New archives use v2: files share independent blocks (1 MiB by default), encoded with Zstandard
or raw storage, whichever is smaller. A compressed, checksummed index maps paths
to payload extents. `list` reads only this index; `test` verifies every payload.
`--block-size-kib` accepts powers of two from 64 to 16384. Larger blocks can
improve compression but use more memory and decode more data for small selections.
`extract --path` accepts exact paths or directory subtrees and decodes only blocks
intersecting the selection. It validates those blocks, not unrelated payloads.
V1 archives remain readable; v1 listing still validates all content.

Reading, codec work, and writing overlap through bounded per-worker queues.
Workers reuse codec contexts and recycle payload buffers. `--threads` controls
codec workers (1–64), with separate reader/writer work. Blocks containing many
complete small files also use the Rayon pool for
parallel file writes, so `--threads 4` can activate four codec workers and four
file-writing workers plus reader/control work; it is not a total thread budget.
Metadata grows with entry
count, capped at one million entries and a 64 MiB decoded index. Payload memory
scales with worker count and block size; codec contexts add level-dependent memory.
The Rust engine currently uses the C Zstandard codec through
the `zstd` crate ([API documentation](https://docs.rs/zstd/latest/zstd/bulk/)).

Limitations: regular files and directories with UTF-8 paths only; permissions,
timestamps, links, extended attributes, and encryption are not implemented.
V2 CRC32 checks payloads and decoded index metadata for accidental corruption;
archives are not authenticated. V1 metadata remains unchecked. Inputs must remain
unchanged during creation. Block-scoped solid packing has no cross-block history.
Extraction has no total expanded-size quota yet. Use private destination parents;
publication does not defend against concurrent filesystem changes by other users.

Development checks: `cargo fmt --check`, `cargo test`, and
`cargo clippy --all-targets -- -D warnings`.

Existing formats: `extract`, `list`, and `test` automatically delegate non-native
archives to `7zz` or `7z` on PATH, or the executable specified by `ZIPPER_7Z`.
Supported formats and extraction protections depend on that installed backend.
This default path is an external compatibility bridge; an optional in-process
ZIP reader is described below.
Native `--threads` does not configure the compatibility backend. Legacy listing
uses the backend's listing operation and does not validate payloads.

Experimental in-process ZIP reading is available with `--zip-engine native`:

```sh
zipper --zip-engine native --threads 4 extract files.zip -o restored
zipper --zip-engine native test files.zip
```

For many tiny files, prefer the streaming `native` reader: buffered Deflate often
adds no benefit when file creation dominates. Automatic native ZIP extraction on
macOS uses at most four workers for archives with at least 1,024 regular files,
all at most 16 KiB. This avoids the measured filesystem contention from using
every CPU. Explicit `--threads N` overrides that tuning; verification-only reads
and larger files retain their existing concurrency. Windows and Linux do not
apply this macOS-specific policy. [Small-file ZIP versus bsdtar measurements](benchmarks/native-zip/small-files-tar.md).

This parallel Rust path supports stored/Deflate ZIP and conventional ZIP64,
validates decoded lengths and CRCs, and preserves file bytes and directories.
It does not preserve permissions/timestamps or support encryption, symlinks,
other codecs, multi-disk archives, or ambiguous end records. It accepts at most
100,000 entries and 64 MiB of raw central-directory metadata. Parsed metadata is
shared; streaming buffers scale with workers, not expanded file size. Extraction
has no total expanded-size quota. The compatibility backend remains the default.

`--zip-engine native-fast` adds a libdeflate buffer decoder with a shared
budget for live compressed and expanded entry buffers across workers: 128 MiB
on ARM64 macOS and 64 MiB elsewhere.
Entries that cannot reserve space immediately stream through zlib-rs. Override
with `--zip-buffer-mib N` (0 forces streaming). Metadata, worker state, archive
cache and allocator-retained memory are additional: this is not a peak RSS cap.
On Windows, buffered output is written in 256 KiB slices;
[large-entry measurements](benchmarks/native-zip/windows-write-size/README.md)
show why. Both native ZIP modes verify decoded lengths and CRCs before publication. Use
`native` for low memory consumption. See the [shared-budget measurements](benchmarks/native-zip/shared-budget/README.md).

For `native-fast test` and Linux `native-fast` extraction, archives up to 8 MiB
are cached once and shared by independent read cursors. Larger archives use the
file reader. This
limit covers retained archive bytes, not total peak RAM: loading/conversion,
metadata, and decoder buffers add memory. This cache remains disabled for extraction on other platforms. On the measured
Linux runner, it improved grouped/flat tiny-file extraction by 5.9×/2.0× at four
workers; see [matched Linux comparisons](benchmarks/native-zip/linux-cache/README.md). [Threefold-speed target
investigation and verification results](benchmarks/native-zip/threefold/README.md).

Next performance work: real-corpus level sweeps, similarity ordering, dictionaries,
and measured block-size/worker scaling. See the research agenda above.

Run `python3 scripts/benchmark.py /path/to/corpus --runs 3 --threads 4` after a
release build. On macOS/Linux this records per-process wall time and peak RSS,
archive size, and throughput, and verifies every extracted file with SHA-256.
Add `--sevenzip /path/to/7zz` for an LZMA2 level-1 baseline. Levels across codecs
are not equivalent: compare archive sizes as well as throughput, and sweep levels
before claiming a win. Runs include process startup and filesystem I/O, do not
flush OS caches, and exclude verification from extraction timing. The initial
manifest warms input caches. Keep the corpus unchanged during a run. Use separate
text, source, binary, media, and small-file corpora; tiny smoke tests establish
correctness, not competitive performance.
