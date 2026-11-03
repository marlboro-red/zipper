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

### Limit the extracted size

Before writing anything, `extract` adds up the declared size of every entry. By default the total must fit in the destination filesystem's free space. The estimate rounds each nonempty file up to a 4 KiB allocation unit. To set your own cap instead, pass `--max-extracted-bytes`:

```sh
target/release/zipper extract backup.zipper --output restored --max-extracted-bytes 10000000000
```

The limit applies to `.zipper` v2 archives and to ZIP archives the in-process reader handles, which is the default. Legacy v1 archives have no size index, so they refuse the option. The 7-Zip backend applies its own rules and refuses the option too. On a compressed filesystem the free-space estimate can be too strict, so pass a cap to override it.

### Extract into an existing folder

To update a folder that already exists, pass `--overwrite`:

```sh
target/release/zipper extract update.zip --output installed --overwrite
```

Zipper decodes the whole archive into a hidden `.zipper-` folder inside the destination first. If the archive is damaged or rejected, no file is replaced and the hidden folder is removed. Then each archive file replaces the file of the same name by rename, so no file is ever half written. If a rename fails partway, the files before it stay replaced. Files the archive does not name stay as they are. Existing folders keep their permissions. A folder in the destination that is on another disk, such as a mount point, fails the extraction before any file changes. If `--output` is a link to a folder, Zipper extracts into that folder. A killed run can leave its hidden `.zipper-` folder behind, and you can delete it.

Zipper never writes through a link that is already in the destination. A link that the archive names as a file is replaced by that file. A link where the archive needs a folder is an error. Where the destination has a file and the archive has a folder of the same name, or the reverse, the extraction fails before any file changes. Library callers pass `zipper::Existing::Overwrite` in the reader options.

## Extract selected paths

To extract part of a `.zipper` v2 or ZIP archive, pass `--path` with an exact path or a folder. Repeat it to select more:

```sh
target/release/zipper extract backup.zipper --output selected --path data/subdir
target/release/zipper extract bundle.zip --output selected --path assets/icons --path readme.txt
```

A folder selects everything under it, even when the archive has no entry for the folder itself. Each path must match at least one entry, or the extraction fails before writing anything. `--path` combines with `--overwrite`. Library callers set `paths` in `native_zip::ReadOptions`.

For `.zipper` archives, `extract --path` decodes only the blocks that intersect the selection. For ZIP it decodes only the selected entries. Both validate what they decode, not unrelated payloads. Tar archives refuse `--path`, because a tar stream has no index.

## Follow and cancel extraction from Rust

An application can pass a progress callback and a cancel flag in `zipper::Control`. ZIP takes them in `native_zip::ReadOptions` and tar in `native_tar::ReadOptions`:

```rust
use std::sync::atomic::AtomicBool;
use zipper::{Cancelled, Control, Existing, Progress, native_zip};

let cancel = AtomicBool::new(false);
let show = |progress: Progress| {
    println!("{} of {:?} bytes", progress.bytes_done, progress.bytes_total);
};
let result = native_zip::read_archive_with_options(
    "update.zip".as_ref(),
    Some("installed".as_ref()),
    native_zip::ReadOptions {
        existing: Existing::Overwrite,
        control: Control { progress: Some(&show), cancel: Some(&cancel) },
        ..Default::default()
    },
);
if let Err(error) = &result && error.is::<Cancelled>() {
    println!("cancelled; installed is unchanged");
}
```

The callback runs on worker threads, one call at a time, after each file and each decoded chunk. Its counts only grow, and the bytes are decoded file bytes. If the callback panics, the panic reaches your code and nothing is published. ZIP reports both totals up front. Tar reports `None` for them, because a tar stream has no index. Another thread stops the extraction by storing `true` in the flag. The extraction then returns `Cancelled` and changes nothing in the destination. Once Zipper starts moving files into the destination, it ignores the flag. `.zipper` archives and the 7-Zip backend do not take `Control` yet.

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

`extract`, `list`, and `test` read ZIP and tar archives in process and delegate other formats to `7zz` or `7z` on PATH.
To use another executable, set `ZIPPER_7Z` to its path. To send ZIP archives to 7-Zip as well, pass `--zip-engine backend`.
The installed backend decides which formats work and which extraction protections apply.
This default path is an external compatibility bridge.
Native `--threads` does not configure the compatibility backend.
Legacy listing uses the backend's listing operation and does not validate payloads.

## Read ZIP archives in process

ZIP archives use the in-process `native` reader by default, with no flag:

```sh
zipper --threads 4 extract files.zip -o restored
zipper test files.zip
```

If the archive is encrypted, uses another compression method or spans disks, the error says so. To read it with 7-Zip, pass `--zip-engine backend`. Zipper never makes that switch itself, because 7-Zip accepts some archives the in-process reader rejects as unsafe.

Pick the engine by workload:

- For many tiny files, prefer the streaming `native` reader. Buffered Deflate often adds no benefit when file creation dominates.
- For low memory consumption, use `native`.
- `--zip-engine native-fast` adds a libdeflate buffer decoder with a shared buffer budget.
- To change the `native-fast` buffer budget, pass `--zip-buffer-mib N`. `--zip-buffer-mib 0` forces streaming.

The default is `native` because it uses little memory. On the [scoreboard](results.md), its peak stays under 9 MiB, while `native-fast` reaches 187 MiB on Silesia. `native-fast` is at most 1.10× faster on every other workload, and 1.32× faster on Silesia at one thread.
The [ZIP reader reference](reference/zip-reader.md) lists supported features, limits, budgets, and caching rules.

## Read tar archives in process

Plain tar, and tar inside gzip, zstd or xz, use the in-process tar reader by default:

```sh
zipper extract source.tar.gz -o restored
zipper test source.tar.zst
```

The reader recognizes a tar by its first header, not by its file name. A `.gz` or `.xz` file that holds no tar still goes to 7-Zip. If the archive uses sparse files or names that are not UTF-8, the error says so, and `--zip-engine backend` reads it with 7-Zip. The [tar reader reference](reference/tar-reader.md) lists supported formats and checks.

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
- [Results](results.md), the generated scoreboard of fixed workloads.
- [Silesia extraction time versus compression ratio](../benchmarks/silesia/README.md), including the Pareto curve with classic formats.

The [roadmap](roadmap.md) lists the next ZIP extraction work. The [research agenda](research.md) covers native-format ideas such as similarity ordering and dictionaries.
