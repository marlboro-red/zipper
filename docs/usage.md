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

Zipper decodes the whole archive into a hidden `.zipper-` folder inside the destination first. If the archive is damaged or rejected, no file is replaced and the hidden folder is removed. Then each archive file replaces the file of the same name by rename, so no file is ever half written. If a rename fails, for example in a read-only folder, or a cancel or signal arrives, Zipper undoes the renames before it in reverse order. The destination then holds every file, link and folder it held before, with the same bytes and permissions, and the error names the path that failed. Folder modification times are not restored. On Linux and macOS a replaced file swaps names with its new copy in one step, so its path always holds the old or the new file. Zipper tests the swap once per extraction. Where the filesystem cannot swap names, as on HFS+, and on Windows, the old file first moves into the hidden folder, so its path is empty for a moment. An undo can fail too, when another program changes the destination during these renames or an access rule allows a rename one way only. An undo also skips a path that holds something other than what Zipper put there. The error then names each path left changed, and the hidden folder stays behind with everything not moved back. An undone `rename-existing` can leave a set-aside name in the archive's spelling on Windows. It can do the same elsewhere for a file with a second hard link in its folder, when the two spellings differ by more than case. Files the archive does not name stay as they are. Existing folders keep their permissions. A folder in the destination that is on another disk, such as a mount point, fails the extraction before any file changes. In every `--existing` mode, a destination on FAT or exFAT on macOS is refused before decoding, because a merge there cannot be undone exactly. So is one on Linux or macOS whose filesystem cannot rename without replacing. Extract into a new folder on such a disk instead. If `--output` is a link to a folder, Zipper extracts into that folder. A killed run can leave its hidden `.zipper-` folder behind, and you can delete it. `tests/overwrite_exhaustive.rs` checks this contract on every pair of small destination and archive trees, including destinations with links that point outside.

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

An entry `assets/icons/small/a.png` lands at `icons/small/a.png`, and nothing outside `assets/icons` is extracted. `--strip-components N` instead drops the first `N` folders of every path, as tar does, and skips an entry with no more than `N` segments. A tar entry written as `./pkg/a.txt` counts its `.` as a segment, as GNU tar and bsdtar count it. Both place each file in one pass, with no rename after extraction.

- `--path`, `--include` and `--exclude` still match the full archive paths, before the strip.
- The extraction fails before publishing anything when the prefix is not a folder of the archive, or when the strip and the selection leave nothing to extract.
- It also fails when the stripped paths would put two files or links at one path, or a file where a folder must go. Folders of the same name merge, as tar merges them.
- The prefix is an archive path written with `/`, matched letter for letter. A leading `./` is dropped.
- A symlink is checked at its stripped path, so a link that pointed elsewhere in the archive but would leave the output is refused.
- A tar hard link whose target the strip leaves out is refused.
- `--flat` with a strip extracts only the stripped entries, without their folders.
- A single compressed file has no folders, so any strip leaves nothing and fails.
- Library callers set `strip` in `ExtractOptions` to `Strip::Prefix` or `Strip::Components`. `tests/strip.rs` strips every archive of the ZIP, tar and 7z corpora by each of their folders and by 1 to 3 segments.
- The 7-Zip backend refuses both options.

## Extract contents only

When the extracted files feed a build or a runtime, pass `--content-only` to write their contents and skip the archive's other metadata:

```sh
target/release/zipper extract toolchain.tar.gz --output tools --content-only
```

- No modification time is restored, so every file and folder carries the time it was written.
- On Windows, no read-only, hidden or system attribute is set, and no Mark of the Web is copied from the archive.
- On Unix, a file gets mode 0o666, or 0o777 when the archive marks it executable, less the umask. Folders get the default mode. A read-only entry therefore stays writable, and scripts still run.
- On macOS, no extended attribute is restored from an APFS or HFS+ volume.
- Links are still created, with the same safety checks.
- Library callers set `metadata: Metadata::ContentOnly` in `ExtractOptions`. `tests/content_only.rs` extracts every archive of the ZIP, tar and 7z corpora this way and checks contents, modes, times and, on Windows, attributes and zone marks.
- The 7-Zip backend refuses it, because 7-Zip restores times and attributes itself.

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

The callback runs one call at a time, after each file and each written chunk. Its counts only grow. `bytes_done` counts file bytes written to disk, not bytes decoded. A ZIP entry whose writer thread is still writing, or a tar file in the writer pool, keeps the count below the total until it lands. Each file's last chunk of bytes, and the file itself, count only once the file is closed. So `files_done` reaches its total only after every close, including the virus scan Windows runs when a new executable closes. `bytes_done` can reach its total a little earlier, while empty files and links are still being created. A tar file counts as done only when all of it is written.

`Progress::phase` is `Phase::Extracting` while files are written. The final report, from the calling thread, says `Phase::Finishing` with the final counts. Zipper then restores folder times and modes, writes Windows zone marks, and moves the result into the destination, and the call returns when that ends. Links are created just before this report, or during the phase. A UI can show "finishing" instead of a bar stuck at 100%. `tests/control.rs` checks the phases for ZIP, tar, 7z and gzip, and on Linux and macOS that no extracted file is still open when the counts reach their totals. Each 7z folder worker writes through a 1 MiB buffer, so 7z can run that far ahead per worker. `tests/control.rs` checks that ZIP and tar, at 4 threads, never run more than one 1 MiB buffer ahead of the bytes on disk. If the callback panics, the panic reaches your code and nothing is published. The callback may run Rayon work. A ZIP worker waits at most one second for decoder memory, and its write-behind writer threads never call the callback. A ZIP entry that Rayon starts inside the callback is decoded in another pass after the current one, so the callback never decodes a whole entry itself. ZIP and 7z report both totals up front. Tar reports `None` for them, because a tar stream has no index. Another thread stops the extraction by storing `true` in the flag. The extraction then returns `Cancelled` and changes nothing in the destination. Zipper also checks the flag while it moves files into the destination. In an existing folder it checks before each rename and once more after the last rename and the folder modes. A new folder is checked once, just before its one rename. A cancel seen then undoes the renames already made, as a failed rename does, so the folder ends as it was. If that undo fails, the error is still `Cancelled` and names each path left changed. That last check is the point of no return. A flag set after it is ignored, and the call returns success with every file in place. `src/staging.rs` tests a cancel at every check in every `Existing` mode, over replaced files, merged folders and links. The 7-Zip backend reports no progress and checks the flag only before it starts.

`native_zip::read_archive_with_options` and `native_tar::read_archive` take the same options when you already know the format.

### Build only the formats you read

The default features build the CLI and every reader. A library that reads only some formats turns the defaults off and names them:

```toml
zipper = { git = "https://github.com/marlboro-red/zipper", default-features = false, features = ["zip", "7z"] }
```

| Feature | What it builds | Crates it adds |
| --- | --- | --- |
| `zip` | The ZIP reader with every method it decodes, including ZipCrypto and WinZip AES, and Zstandard entries with `zstd`. It is `zip-deflate` with every `zip-` feature below | The union of theirs |
| `zip-deflate` | The ZIP reader for stored and Deflate entries only | None |
| `zip-encryption` | ZipCrypto and WinZip AES entries, with `zip-deflate` | `aes`, `ctr`, `hmac`, `pbkdf2`, `sha1` |
| `zip-legacy` | Shrink and Implode entries, with `zip-deflate` | None |
| `zip-deflate64` | Deflate64 entries, with `zip-deflate` | `deflate64` |
| `zip-bzip2` | BZip2 entries, with `zip-deflate` | None |
| `zip-lzma` | LZMA and XZ entries, with `zip-deflate` | None |
| `zip-ppmd` | PPMd entries, with `zip-deflate` | `ppmd-rust` |
| `7z` | The 7z reader with every method it decodes, including AES-256 | `aes`, `bzip2`, `deflate64`, `ppmd-rust`, `zeroize` |
| `tar` | The tar reader and single compressed files such as `.gz`, `.xz` and `.bz2`, and `.zst` with `zstd` | `bzip2`, `ppmd-rust` |
| `zstd` | Zipper's own Zstandard decoder for ZIP entries, `.zst` files, tar and RPM payloads and SquashFS images | None |
| `cpio` | The cpio reader | None |
| `ar` | The `ar` and Debian package reader, and `tar` for the parts inside | None beyond `tar` |
| `rpm` | The RPM reader, and `cpio` and `tar` for its payload | None beyond `tar` |
| `xar` | The xar and flat macOS package reader | `bzip2`, `md-5`, `quick-xml`, `sha1` |
| `cab` | The CAB reader | None |
| `msi` | The compound file reader, and `cab` for the cabinets inside | None beyond `cab` |
| `lzh` | The LZH reader | None |
| `arj` | The ARJ reader | None |
| `iso` | The ISO 9660 and UDF reader | None |
| `wim` | The WIM and ESD reader | `sha1` |
| `chm` | The CHM and Help 2 reader | None |
| `exe` | The PE, COFF, TE, ELF and Mach-O reader | None |
| `ntfs` | The NTFS reader | None |
| `squashfs` | The SquashFS reader, and zstd images with `zstd` | None |
| `cramfs` | The CramFS reader | None |
| `ext` | The ext2, ext3 and ext4 reader | None |
| `apfs` | The APFS reader | None |
| `dmg` | The DMG, HFS+ and HFSX reader, and `apfs` | `bzip2` |
| `disk` | The VHD, VHDX, QCOW, VDI, VMDK, Android sparse and MBR or GPT disk reader with FAT and HFS+, and `apfs`, `cramfs`, `ext`, `ntfs` and `squashfs` for the other partitions | None |
| `swf` | The SWF reader | None |
| `flv` | The FLV reader | None |
| `lp` | The Android logical partition (`super.img`) reader | None |
| `uefi` | The UEFI firmware volume and capsule reader | None |
| `more-formats` | Every reader from `cpio` to `uefi` above | The union of theirs |
| `all-formats` | `zip`, `7z`, `tar`, `zstd` and `more-formats` | The union of theirs |
| `cli` | The `zipper` binary, with `all-formats` | `clap` |

Every build also compiles `anyhow`, `crc32fast`, `rayon`, `sha2`, `tempfile` and `walkdir`, with `libc` and `rustix` on Unix or `windows-sys` on Windows. A feature that names another, such as `rpm` naming `tar`, also builds that reader. Counted with `cargo tree -e normal` on macOS, each package once and Zipper included, a `zip-deflate` build has 28 packages, a `zip` build and a `zip` and `7z` build 40, and the default 60. No feature compiles C. Every reader decodes Deflate, zlib and gzip with Zipper's own decoder in `src/inflate.rs`, so no build links a zlib. A build without `zstd` refuses a ZIP Zstandard entry, a `.zst` file or tar, an RPM with a zstd payload and a zstd SquashFS image with `zipper::NotBuilt` naming `zstd`, and writes nothing. `tests/zstd_feature.rs` checks the ZIP, tar and SquashFS cases.

A program that reads only plain ZIP archives can build the stored and Deflate reader alone:

```toml
zipper = { git = "https://github.com/marlboro-red/zipper", default-features = false, features = ["zip-deflate"] }
```

That build leaves out the 12 packages that only encryption, Deflate64 and PPMd need. It refuses an archive with an entry it cannot decode, such as a BZip2 or an encrypted entry, with `zipper::NotBuilt` naming the feature to add, and writes nothing. Add only the method features your archives use, such as `features = ["zip-deflate", "zip-lzma"]`. The [ZIP feature size record](../benchmarks/cargo-features/zip-deflate/README.md) measures a small program at 1.42 MB with `zip-deflate` against 1.68 MB with `zip`.

The `Reader` enum keeps every variant in every build. A ZIP or tar archive that the build does not read fails with `zipper::NotBuilt`, which names the format, and writes nothing. With `Engine::Native`, so does every other format whose signature Zipper knows, such as `gzip` or `7z`. `Engine::Auto` hands those to 7-Zip instead, as it does any format it recognizes but does not read. Choose `Engine::Native` to never start 7-Zip. An archive with no known signature goes to the ZIP reader, as in every build, so a build without `zip` refuses it as `zip`.

`cargo xtask gate` lints every feature on its own with `unused_crate_dependencies` denied, so no feature builds a crate its readers do not use. It runs `tests/feature_subset.rs` and the ZIP and 7z corpus tests in a `zip` and `7z` build, and `tests/zip_method_features.rs` and `tests/zstd_feature.rs` in a `zip-deflate` build. It also runs `tests/consumer`, a crate that depends on Zipper with only `zip` and `7z`, and checks that it extracts a corpus ZIP and 7z in process to the trees their manifests record. Built without its default features, the same crate depends on Zipper with only `zip-deflate` and extracts a stored and a Deflate corpus ZIP. The gate also fails when a `zip-deflate` build depends on a crate that only encryption, Deflate64 or PPMd needs. `cargo xtask format-tests` runs each format's corpus or hostile tests in a build with only that format's feature, and `cargo xtask release-checks` runs it too. The `apfs`, `ext` and `squashfs` tests also wrap their images in disk images and DMGs, so they run with `disk` and `dmg`.

When `extract_archive` or a reader returns, after success, failure or a cancel, Zipper holds no handle to the destination or the archive, and every thread it started has exited. You can rename, move or delete the destination at once. The shared Rayon pool keeps its idle threads between calls, and they hold nothing from the extraction. The 7-Zip backend is waited for before it returns. `tests/handles.rs` checks this after every reader on Unix by counting open descriptors and threads, and on Windows by renaming and deleting the destination of archives that carry a zone mark.

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

`extract`, `list`, and `test` read ZIP, tar and most 7z archives in process and delegate other formats to `7zz` or `7z` on PATH. A 7z archive whose methods the [7z reader](reference/7z-reader.md) does not decode also goes to 7-Zip. Encrypted 7z archives decrypt in process.
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

If the archive uses another compression method, PKWARE strong encryption or more than one disk, the error says so. To read it with 7-Zip, pass `--zip-engine backend`. Zipper never makes that switch itself, because 7-Zip accepts some archives the in-process reader rejects as unsafe.

The `native` reader streams every entry, so its memory stays bounded by the worker count, not by file sizes. The [scoreboard](results.md) records its peak memory on each workload.
The [ZIP reader reference](reference/zip-reader.md) lists supported features, limits and budgets.

### Extract an encrypted ZIP

Zipper decrypts ZipCrypto and WinZip AES entries in process. Run `extract` or `test` as usual. When a selected entry is encrypted and the destination can be used, Zipper asks for the password on the terminal with echo off:

```sh
zipper extract secret.zip -o restored
```

The prompt follows the design of OpenBSD's `readpassphrase`. Zipper turns off echo, line editing and the Ctrl-S and Ctrl-Q flow keys before it shows the prompt, and drops anything typed earlier. The terminal's signal keys stay on. At the prompt these keys act:

- Enter or Return ends the password.
- Backspace or Delete removes the last character. When the password ends in a whole UTF-8 character, all its bytes go at once. Otherwise one byte goes. On a terminal that does not send UTF-8, pass the password with `--password` instead.
- Ctrl-U clears what was typed.
- Ctrl-D cancels on an empty line, and does nothing after text.
- The interrupt key, usually Ctrl-C, ends Zipper by SIGINT, so a calling script stops too. The quit key, usually Ctrl-\\, ends it by SIGQUIT.
- The suspend key, usually Ctrl-Z, stops Zipper. When the job resumes, it shows a new prompt.

These three keys act only while the terminal sends signals. When the caller turned that off, as `stty raw` or `stty -isig` do, the interrupt, quit and suspend characters are password bytes.

Every other byte, such as Tab, Escape, Ctrl-S or Ctrl-Q, is part of the password. A password, typed or given, holds at most 1,024 bytes. A longer one fails, but only after Zipper has read and dropped the rest of the line. A paste over the limit is therefore dropped up to the next Enter, and the prompt waits for Enter before it fails, so the paste never reaches the shell. Typing cannot enter line feed, carriage return, backspace, delete, Ctrl-U or Ctrl-D, nor the terminal's interrupt, quit and suspend characters. Pass a password that holds them with `--password`. When the prompt ends, Zipper restores the terminal and drops anything typed but not read, such as the rest of a paste.

During the prompt Zipper catches SIGALRM, SIGCONT, SIGHUP, SIGINT, SIGPIPE, SIGQUIT, SIGTERM, SIGTSTP, SIGTTIN and SIGTTOU, unless they are ignored. SIGCONT, which follows any resume, including one after an uncatchable SIGSTOP, restores the terminal and shows a new hidden prompt, so it does not matter whether the shell put the hidden settings back. Rust programs ignore SIGPIPE, so it stays ignored. A caught signal restores the terminal first. A stop then stops Zipper, and when the job resumes Zipper hides the terminal again and shows a new prompt. A stray SIGTTIN or SIGTTOU that arrives while Zipper is already in the foreground only brings a new prompt. So does a stop that the system discards, as it does for a process group with no job-control parent. Any other caught signal ends Zipper by that signal, after it removes the temporary folder of `--stdout`. With the archive read from `-`, standard input is not a terminal, so Zipper never prompts. Zipper reads the terminal settings to restore only while its job is in the foreground. A job started in the background stops first, and reads them after `fg`, so it never takes a line editor's raw settings as the ones to restore. A signal the shell ignores, as `nohup` and `trap ''` arrange, stays ignored. That includes SIGINT and SIGQUIT, which `readpassphrase` takes over even when they are ignored. SIGKILL and SIGSTOP cannot be caught. SIGKILL leaves the terminal hidden, and `stty sane` recovers it.

While `extract` runs, and while the 7-Zip backend runs `list` or `test`, Zipper also catches SIGINT, SIGTERM, SIGHUP and SIGQUIT, unless they are ignored. The signal stops the extraction through its cancel check, and the staging folder and any decrypted bytes in it are removed. With the 7-Zip backend, for `extract`, `list` and `test` alike, Zipper sends SIGTERM to 7-Zip, waits for it to exit, and sends SIGKILL if it still runs after 2 seconds. Only that one process is stopped, so a program that `ZIPPER_7Z` names and that starts helpers of its own must stop them itself. With `--stdout`, the signal also stops the copy to standard output, even while a full pipe blocks it, and the temporary folder is removed. A signal after the last byte was copied leaves the output whole, and Zipper exits with success. While Zipper copies an archive read from `-` into its spool file, the signal stops the copy and the spool is removed. Zipper then ends by the same signal. A signal while files move into the destination undoes the moves made so far, as a cancel does [from Rust](#extract-from-rust). A signal that arrives after the last move does not undo it, and Zipper exits with success.

To give the password without a prompt, as a script must, pass `--password`. It takes any bytes on Unix, including ones that are not UTF-8. Other users on the machine can read it in the process list:

```sh
zipper --password 'correct horse' extract secret.zip -o restored
```

When standard input is not a terminal and no password was given, Zipper refuses. Windows has no prompt yet, so `extract` and `test` fail there when a selected entry is encrypted and no `--password` was given. A wrong password fails before anything is published, and the error says the password is wrong, or that it is wrong or the entry is damaged. ZipCrypto is weak, so treat its archives as unprotected. Library callers set `password` in `ExtractOptions`. Zipper decrypts ZIP and 7z archives. It refuses `--password` for archives it does not decrypt, such as tar, and the 7-Zip backend refuses it too. The [ZIP reader reference](reference/zip-reader.md#encrypted-entries) lists what each scheme checks.

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
