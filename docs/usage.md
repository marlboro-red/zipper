# How to use Zipper

This guide shows how to extract ZIP, tar and 7z archives, use Zipper from Rust, find duplicate files, and tune Zipper.
Build the release binary first with `cargo build --release`.

## Extract, list, and test an archive

Extract into a new destination directory whose parent exists:

```sh
target/release/zipper extract bundle.zip --output restored
target/release/zipper list bundle.zip
target/release/zipper test bundle.zip
```

`list` reads only the archive's index.
`test` decodes and checks every entry without writing.
Use a private destination parent, because publication does not defend against concurrent filesystem changes by other users.

### Limit the extracted size

Before writing anything, `extract` adds up the declared size of every entry. By default the total must fit in the destination filesystem's free space. The estimate rounds each nonempty file up to a 4 KiB allocation unit. To set your own cap instead, pass `--max-extracted-bytes`:

```sh
target/release/zipper extract bundle.zip --output restored --max-extracted-bytes 10000000000
```

The limit applies to ZIP, tar and 7z archives read in process, which is the default. The 7-Zip backend applies its own rules and refuses the option. On a compressed filesystem the free-space estimate can be too strict, so pass a cap to override it.

### Extract into an existing folder

To update a folder that already exists, pass `--overwrite`:

```sh
target/release/zipper extract update.zip --output installed --overwrite
```

Zipper decodes the whole archive into a hidden `.zipper-` folder inside the destination first. If the archive is damaged or rejected, no file is replaced and the hidden folder is removed. Then each archive file replaces the file of the same name by rename, so no file is ever half written. If a rename fails partway, the files before it stay replaced. Files the archive does not name stay as they are. Existing folders keep their permissions. A folder in the destination that is on another disk, such as a mount point, fails the extraction before any file changes. If `--output` is a link to a folder, Zipper extracts into that folder. A killed run can leave its hidden `.zipper-` folder behind, and you can delete it. `tests/overwrite_exhaustive.rs` checks this contract on every pair of small destination and archive trees, including destinations with links that point outside.

`--existing` chooses what happens to a path the destination already holds, as 7-Zip's `-ao` switches do:

- `overwrite`, the same as `--overwrite`, replaces each file of the same name.
- `skip` keeps the existing file and leaves the archive's copy out. Where an archive folder meets anything other than a folder, the folder and everything under it is left out.
- `rename-new` writes the archive's file or folder as `name_1.ext`, the first free name.
- `rename-existing` renames what is already there to `name_1.ext`, then writes the archive's file. A link is renamed, never followed.

In every mode a folder merges into an existing folder, and nothing outside the destination changes. Only `overwrite` fails on a file where the archive has a folder, or the reverse. A renamed folder keeps default permissions.

Zipper never writes through a link that is already in the destination. With `overwrite`, a link that the archive names as a file is replaced by that file, and a link where the archive needs a folder is an error. Where the destination has a file and the archive has a folder of the same name, or the reverse, `overwrite` fails before any file changes, and the other modes skip or rename as above. In every mode, an archive link whose target passes through a link the destination keeps is refused before any file changes. A renamed name must fit in 255 bytes, or the extraction fails before any file changes. Each new name is moved into place with a rename that cannot replace a file, so a file another program creates there during extraction stops the extraction instead of being lost. Library callers pass a `zipper::Existing` value in the reader options.

## Extract selected paths

To extract part of a ZIP, tar or 7z archive read in process, pass `--path` with an exact path or a folder. Repeat it to select more:

```sh
target/release/zipper extract bundle.zip --output selected --path assets/icons --path readme.txt
```

A folder selects everything under it, even when the archive has no entry for the folder itself. Each path must match at least one entry, or the extraction fails before writing anything. `--path` combines with `--overwrite`. Library callers set `paths` in `native_zip::ReadOptions`.

For ZIP, `extract --path` decodes only the selected entries. For 7z it decodes only the folders that hold selected files. Both validate what they decode, not unrelated payloads. Tar reads the whole stream and checks every header, writing only the selected entries. A selected tar hard link whose target is left out is refused.

To select by wildcard, pass `--include` and `--exclude`, each repeatable. `*` and `?` match within one name, and `**` crosses folders. A pattern without `/`, such as `*.log`, matches a name at any depth, and a pattern with `/`, such as `build/*`, matches the whole path. A pattern that matches a folder covers everything under it. An entry is extracted when it matches `--path` or `--include`, or when neither is given, and matches no `--exclude`. An `--include` that matches nothing is an error:

```sh
target/release/zipper extract bundle.zip --output selected --include '*.txt' --exclude 'drafts'
```

The 7-Zip backend refuses `--path`, `--include` and `--exclude`.

## Extract a folder at the output root

To extract one folder of an archive with its contents at the output root, pass `--strip-prefix` with the folder. Its structure below stays as it is:

```sh
target/release/zipper extract bundle.zip --output icons --strip-prefix assets/icons
```

An entry `assets/icons/small/a.png` lands at `icons/small/a.png`, and nothing outside `assets/icons` is extracted. `--strip-components N` instead drops the first `N` folders of every path, as tar does, and skips an entry with no more than `N` segments. Both place each file in one pass, with no rename after extraction.

- `--path`, `--include` and `--exclude` still match the full archive paths, before the strip.
- The extraction fails before publishing anything when the prefix is not a folder of the archive.
- It also fails when the stripped paths would put two entries at one path, or a file where a folder must go.
- A symlink is checked at its stripped path, so a link that pointed elsewhere in the archive but would leave the output is refused.
- A tar hard link whose target the strip leaves out is refused.
- `--flat` with a strip extracts only the stripped entries, without their folders.
- A single compressed file has no folders, so any strip leaves nothing and fails.
- Library callers set `strip` in `ExtractOptions` to `Strip::Prefix` or `Strip::Components`. `tests/strip.rs` strips every archive of the ZIP, tar and 7z corpora by each of their folders and by 1 to 3 segments.
- The 7-Zip backend refuses both options.

## Extract without folders, or through pipes

`--flat` puts every file in the output folder itself, without its folders, as `7z e` does. Files that share a name follow `--existing` in path order. The default refuses them, `overwrite` keeps the last, `skip` keeps the first, and the rename modes number the others `name_1.ext` and on. An archive with links is refused, because flattening breaks their targets:

```sh
target/release/zipper extract photos.zip --output all-photos --flat --existing rename-new
```

An archive named `-` is read from standard input. Zipper first copies it to a hidden file beside the output folder, because ZIP and 7z need to seek, and deletes that file afterwards. `--stdout` writes one selected file to standard output instead of a folder, and refuses a selection of any other number of files:

```sh
curl -sL https://example.com/release.zip | target/release/zipper extract - --output release
target/release/zipper extract release.zip --stdout --path docs/LICENSE > LICENSE
```

## Extract from Rust

`zipper::extract_archive` reads any archive the CLI reads. It picks the reader from the archive's first bytes, as `zipper extract` does. `ExtractOptions` holds the same choices as the CLI flags, plus a progress callback and a cancel flag in `zipper::Control`:

```rust
use std::sync::atomic::AtomicBool;
use zipper::{Cancelled, Control, Existing, ExtractOptions, Progress, extract_archive};

let cancel = AtomicBool::new(false);
let show = |progress: Progress| {
    println!("{} of {:?} bytes", progress.bytes_done, progress.bytes_total);
};
let paths = ["assets".to_string()];
let result = extract_archive(
    "update.zip".as_ref(),
    "installed".as_ref(),
    ExtractOptions {
        existing: Existing::Overwrite,
        paths: &paths,
        control: Control { progress: Some(&show), cancel: Some(&cancel) },
        ..Default::default()
    },
);
if let Err(error) = &result && error.is::<Cancelled>() {
    println!("cancelled; installed is unchanged");
}
```

The callback runs one call at a time, after each file and each decoded chunk. Its counts only grow, and the bytes are decoded file bytes. If the callback panics, the panic reaches your code and nothing is published. ZIP and 7z report both totals up front. Tar reports `None` for them, because a tar stream has no index. Another thread stops the extraction by storing `true` in the flag. The extraction then returns `Cancelled` and changes nothing in the destination. Once Zipper starts moving files into the destination, it ignores the flag. The 7-Zip backend reports no progress and checks the flag only before it starts.

`native_zip::read_archive_with_options` and `native_tar::read_archive` take the same options when you already know the format.

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

`extract`, `list`, and `test` read ZIP, tar and most 7z archives in process and delegate other formats to `7zz` or `7z` on PATH. A 7z archive whose methods the [7z reader](reference/7z-reader.md) does not decode, such as PPMd, BCJ2 or a password, also goes to 7-Zip.
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

Plain tar, and tar inside gzip, zstd, xz or bzip2, use the in-process tar reader by default:

```sh
zipper extract source.tar.gz -o restored
zipper test source.tar.zst
```

The reader recognizes a tar by its first header, not by its file name. A compressed file that holds no tar goes to the [single compressed file reader](reference/stream-reader.md), which writes the one decompressed file. If the archive uses sparse files or names that are not UTF-8, the error says so, and `--zip-engine backend` reads it with 7-Zip. The [tar reader reference](reference/tar-reader.md) lists supported formats and checks.

## Choose the thread count

`--threads` sets the parallelism of the reader, from 1 to 64. By default it is the number of logical CPUs.

- ZIP decodes entries on up to `--threads` workers. A large entry can also borrow a spare thread to write while it decodes.
- Tar decodes on one thread and writes files on 4 writer threads by default, or `--threads` of them.
- 7z decodes folders on up to `--threads` workers. A solid folder can split into LZMA2 segments that decode on the spare threads.

Memory stays within each reader's budgets, which the reader references list.

For native ZIP extraction on macOS, Zipper picks a smaller worker count for small-file archives.
Pass an explicit `--threads N` to override it.
The [ZIP reader reference](reference/zip-reader.md#concurrency) gives the exact rule.

## Run benchmarks

`cargo xtask bench` measures the fixed scoreboard workloads against bsdtar and regenerates [Results](results.md). To compare two Zipper binaries on one archive, run `scripts/compare_zip_readers.py` with `--archive-format zip`, `tar` or `7z`. It interleaves the readers, verifies every extracted byte and reports wall time, CPU time and peak memory. `scripts/compare_workloads.py` runs it over every scoreboard workload.

The [experiment log](../benchmarks/README.md) holds every record and its reproduction steps.

The [roadmap](roadmap.md) lists the milestones behind the readers.
