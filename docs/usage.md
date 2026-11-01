# How to use Zipper

This guide shows how to create and extract archives, find duplicate files, read other archive formats, and tune Zipper.
Build the release binary first with `cargo build --release`.

## Create an archive

Pass the archive path and the input directory:

```sh
target/release/zipper create backup.zipper ./data
target/release/zipper --threads 4 create backup.zipper ./data --level 1
target/release/zipper --threads 4 create dense.zipper ./data --level 6 --block-size-kib 4096
```

Keep the archive outside the input tree.
Creation never overwrites an archive.
Keep the inputs unchanged while Zipper reads them.

New archives use v2.
Files share independent blocks, 1 MiB by default.
Zipper encodes each block with Zstandard or raw storage, whichever is smaller.
A compressed, checksummed index maps paths to payload extents.
The Rust engine uses the C Zstandard codec through the `zstd` crate ([API documentation](https://docs.rs/zstd/latest/zstd/bulk/)).
The [native format reference](reference/format.md) defines the layout.

### Choose a level and block size

`--block-size-kib` accepts powers of two from 64 to 16384.
Larger blocks can improve compression.
They also use more memory and decode more data for small selections.

For level and block-size advice, read the [level and block sweep on real dependency sources](../benchmarks/rust-dependencies/level-sweep.md).
On that corpus, `--level 3 --block-size-kib 4096` is 25% smaller than the default with overlapping extraction times.

## Extract, list, and test an archive

Extract into a new destination directory whose parent exists:

```sh
target/release/zipper extract backup.zipper --output restored
target/release/zipper list backup.zipper
target/release/zipper test backup.zipper
```

`list` reads only the index.
`test` verifies every payload.
V1 archives remain readable, and v1 listing still validates all content.
Use a private destination parent, because publication does not defend against concurrent filesystem changes by other users.

## Extract selected paths

To extract part of an archive, pass `--path` with an exact path or a directory subtree:

```sh
target/release/zipper extract backup.zipper --output selected --path data/subdir
```

`extract --path` decodes only the blocks that intersect the selection.
It validates those blocks, not unrelated payloads.

## Find duplicate files

To find identical file contents before archiving, run `duplicates`:

```sh
zipper duplicates ./data ./other-data
zipper --threads 4 duplicates --summary ./data
```

Zipper confirms matches byte for byte.
It counts overlapping input paths once and skips symlinks and special files.
The report is read-only.
Duplicate payload bytes describe logical content, not reclaimable disk space, because hard links and filesystem compression matter.
Keep the inputs unchanged during the scan.
The [duplicate-checking reference](reference/duplicates.md) has the details.
Archive-level deduplication is not implemented yet.

## Read ZIP, 7z, and other existing formats

`extract`, `list`, and `test` delegate non-native archives to `7zz` or `7z` on PATH.
To use another executable, set `ZIPPER_7Z` to its path.
The installed backend decides which formats work and which extraction protections apply.
This default path is an external compatibility bridge.
Native `--threads` does not configure the compatibility backend.
Legacy listing uses the backend's listing operation and does not validate payloads.

## Read ZIP archives in process

To read a ZIP archive without 7-Zip, pass the experimental `--zip-engine native`:

```sh
zipper --zip-engine native --threads 4 extract files.zip -o restored
zipper --zip-engine native test files.zip
```

Pick the engine by workload:

- For many tiny files, prefer the streaming `native` reader. Buffered Deflate often adds no benefit when file creation dominates.
- For low memory consumption, use `native`.
- `--zip-engine native-fast` adds a libdeflate buffer decoder with a shared buffer budget.
- To change the `native-fast` buffer budget, pass `--zip-buffer-mib N`. `--zip-buffer-mib 0` forces streaming.

The compatibility backend remains the default.
The [ZIP reader reference](reference/zip-reader.md) lists supported features, limits, budgets, and caching rules.

## Choose the thread count

`--threads` sets the number of codec workers, from 1 to 64.
Reading, codec work, and writing overlap through bounded per-worker queues.
Workers reuse codec contexts and recycle payload buffers.
Reader and writer work runs separately from the codec workers.

`--threads` is not a total thread budget.
Blocks that contain many complete small files also use the Rayon pool for parallel file writes.
So `--threads 4` can activate four codec workers and four file-writing workers, plus reader and control work.

Memory follows the same settings:

- Metadata grows with entry count, capped at one million entries and a 64 MiB decoded index.
- Payload memory scales with worker count and block size.
- Codec contexts add level-dependent memory.

For native ZIP extraction on macOS, Zipper picks a smaller worker count for small-file archives.
Pass an explicit `--threads N` to override it.
The [ZIP reader reference](reference/zip-reader.md#concurrency) gives the exact rule.

## Run benchmarks

After a release build, run `python3 scripts/benchmark.py /path/to/corpus --runs 3 --threads 4` on a corpus.

On macOS and Linux, the script records per-process wall time and peak RSS, archive size, and throughput.
It verifies every extracted file with SHA-256.
To add an LZMA2 level-1 baseline, pass `--sevenzip /path/to/7zz`.

Follow these rules when you read the numbers:

- Levels across codecs are not equivalent. Compare archive sizes as well as throughput.
- Sweep levels before claiming a win.
- Runs include process startup and filesystem I/O and do not flush OS caches.
- Extraction timing excludes verification.
- The initial manifest warms input caches.
- Keep the corpus unchanged during a run.
- Use separate text, source, binary, media, and small-file corpora. Tiny smoke tests establish correctness, not competitive performance.

The [experiment log](../benchmarks/README.md) holds the local performance baseline and reproduction steps.
These records compare Zipper with other tools:

- [V2 results and experiment decisions](../benchmarks/v2/README.md).
- [Matched-sync v2 comparison against tar+zstd and tar+pzstd](../benchmarks/v2/zstd-fullsync.md).
- [Results](results.md), including the decompression and ratio Pareto curve with classic formats.

The [research agenda](research.md) lists the next performance work, which covers similarity ordering, dictionaries, and measured worker scaling.
