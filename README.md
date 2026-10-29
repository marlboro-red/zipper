# Zipper

A Rust archive engine pursuing fast compression and decompression at competitive
compression ratios. This is an experimental foundation, not a performance leader
yet. Native archives use the versioned `.zipper` format.

```sh
cargo build --release
target/release/zipper create backup.zipper ./data
target/release/zipper extract backup.zipper --output restored
target/release/zipper list backup.zipper
target/release/zipper test backup.zipper
target/release/zipper --threads 4 create backup.zipper ./data --level 1
```

The archive must be outside the input tree. Creation never overwrites an archive;
extraction requires a new destination directory with an existing parent.

Independent 1 MiB blocks use Zstandard or raw storage, whichever is smaller.
Compression and decompression process up to eight blocks concurrently with Rayon.
Payload buffering is bounded per batch; file metadata and the entry index currently
grow with entry count. The Rust engine currently uses the C Zstandard codec through
the `zstd` crate ([API documentation](https://docs.rs/zstd/latest/zstd/bulk/)).

Limitations: regular files and directories with UTF-8 paths only; permissions,
timestamps, links, extended attributes, encryption, random access, and solid
compression are not implemented. CRC32 detects accidental payload corruption;
metadata is not checksummed and archives are not authenticated. Listing currently
decodes and validates all payloads. Inputs must remain unchanged during creation.
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

Next performance work: a reproducible corpus, fair ratio/throughput comparisons,
codec context reuse, I/O overlap, batching small files, and an indexed format.
