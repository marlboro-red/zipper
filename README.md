# Zipper

[Local performance baseline and reproduction steps](benchmarks/README.md).
[Research review and frontier experiments](docs/research-2026-10.md).
[Comparison against tar+zstd and tar+pzstd](benchmarks/zstd-comparison.md).

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
```

The archive must be outside the input tree. Creation never overwrites an archive;
extraction requires a new destination directory with an existing parent.

New archives use v2: files share independent 1 MiB blocks, encoded with Zstandard
or raw storage, whichever is smaller. A compressed, checksummed index maps paths
to payload extents. `list` reads only this index; `test` verifies every payload.
`extract --path` accepts exact paths or directory subtrees and decodes only blocks
intersecting the selection. It validates those blocks, not unrelated payloads.
V1 archives remain readable; v1 listing still validates all content.

Reading, codec work, and writing overlap through bounded per-worker queues.
Workers reuse codec contexts and recycle payload buffers. `--threads` controls
codec workers (1–64), with separate reader/writer work. Metadata grows with entry
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
This is an external compatibility bridge, not yet an in-process Rust decoder.
Native `--threads` does not configure the compatibility backend. Legacy listing
uses the backend's listing operation and does not validate payloads.

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
