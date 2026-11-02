# In-process ZIP reader

Zipper reads ZIP archives with its own parallel Rust reader by default.
The default engine, `--zip-engine auto`, decides from the file's leading bytes and never from 7-Zip's output. A file that starts with a ZIP record signature goes to the in-process reader. A file that starts with the signature of 7z, RAR, gzip, bzip2, xz, CAB or ustar tar goes to 7-Zip with that format pinned by `-t`. A `*.001` volume gets the split chain, such as `-t7z.split`. A pinned 7-Zip cannot reopen the file as a ZIP, even when a ZIP is appended or a wrong password is typed. Every other file goes to the in-process reader, which rejects anything that is not a ZIP it supports. Use `--zip-engine backend` for other formats 7-Zip reads, such as ISO, DMG, WIM, ARJ, zstd and old tar without the `ustar` marker, and for split ZIP volumes. ISO is not pinned, because `-tiso` makes 7-Zip skip a disc's UDF tree. Zipper refuses `*` and `?` in any path it gives 7-Zip, because 7-Zip expands them as wildcards.

A ZIP the reader rejects is never handed to 7-Zip. The reader checks entries in order and stops at the first problem. An unsupported entry can therefore hide an unsafe one, and 7-Zip extracts some archives the reader refuses, such as names that collide on the filesystem. When the reader lacks a feature, such as encryption, another compression method or more than one disk, the error names `--zip-engine backend`. Passing that flag is the user's decision.

`--zip-engine native` and `--zip-engine native-fast` select the reader explicitly. `--zip-engine backend` sends every archive except `.zipper` to 7-Zip. Whenever 7-Zip extracts, under either engine, it anchors an absolute symlink target inside its output directory. Zipper renames that directory into place, so such a link can dangle outside the destination.
The [usage guide](../usage.md#read-zip-archives-in-process) shows the commands.

## Supported archives

The reader supports these features:

- Stored and Deflate entries.
- Conventional ZIP64.
- File bytes and directories.
- Symlinks that stay inside the destination, on Unix. See [Symlinks](#symlinks).
- Names stored as UTF-8, and other names decoded as code page 437. A name that is valid UTF-8 is read as UTF-8 even without the UTF-8 flag, because Info-ZIP and `ditto` store UTF-8 names without it. A name with the UTF-8 flag that is not valid UTF-8 is rejected, because Python and 7-Zip do not decode it as CP437.

The reader does not support these features:

- Encryption.
- Symlinks that point outside the destination, and any symlink on Windows.
- Codecs other than stored and Deflate.
- Multi-disk archives.
- Ambiguous end records.
- Data before the first record, such as a self-extracting executable from `zip -A`. The default engine reads it in process and rejects it, because no 7-Zip signature matches.

It restores Unix permission bits the way bsdtar does:

- Each file and directory gets its archive mode, masked to `0o777`, so setuid, setgid and sticky bits are never set. Archives made on DOS or Windows give files `0o664`, directories `0o775`, and read-only entries no write bits.
- The user's umask limits every mode, as it does for a new file. Info-ZIP `unzip` ignores the umask, so it can restore wider modes such as `666` or `777`.
- Directory modes are applied after all files are written, deepest first, so a read-only directory can still receive its children. A directory entry listed after its own contents still gets its mode, which bsdtar and `unzip` skip.

It restores modification times the way Info-ZIP `unzip` and bsdtar do:

- The extended timestamp field (`0x5455`) gives an exact UTC time. Info-ZIP and bsdtar write it.
- Otherwise the older Info-ZIP Unix field (`0x5855`), which `ditto` writes, gives an exact time.
- Otherwise the DOS date and time are read in the local time zone, with 2-second resolution.
- The NTFS field (`0x000a`) is ignored, as both reference readers ignore it, so 7-Zip archives get their DOS time.
- A 32-bit exact time with its top bit set is read as signed or unsigned, whichever lies nearer the DOS time, so a 1969 file is not dated 2106.
- Invalid DOS fields, such as month 13, are normalized as bsdtar does. Setting a time is best effort.
- Directory times are set after all files are written.

On Windows it restores no permissions, no directory times, and no times for entries that have only a DOS time.

It never restores owners, access times, extended attributes, ACLs, file flags or macOS metadata.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 100,000 |
| Raw central-directory metadata | at most 64 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Name normalization

The native engines write each name with its stored bytes, as Info-ZIP `unzip`, Python and 7-Zip do. bsdtar on macOS rewrites names to Unicode NFD instead. APFS and NTFS ignore case, and APFS also ignores normalization, so two entries can name one file there:

- Two files that collide fail the extraction with an error naming the entry, and nothing is published.
- Two directories that collide merge into one, as in `unzip` and Python. The merged directory takes the spelling, mode and time of the first entry in the central directory.

## Symlinks

The native engines are stricter with symlinks than Info-ZIP `unzip` and bsdtar, which create links to any target. A symlink entry is created only when all of these hold:

- The target is a relative UTF-8 path of at most 4,096 bytes, with no NUL byte and, on Windows, no backslash or drive letter.
- Every `..` in the target comes before its first name, as in `../../shared/lib`.
- Counted from the link's own directory, those `..` steps stay inside the destination.
- No other entry uses the link as a directory.

The `..` rule makes the check independent of how the filesystem compares names. A target such as `a/L/../x` climbs out of a link `a/L`, and a lexical check misses it when the filesystem matches `a/l` to `a/L` by ignoring case or Unicode normalization, as APFS does. A target that only descends after its `..` steps stays below the directory it reached, because every link it passes through obeys the same rule.

Links are created after every file is written, so no file is written through a link. An archive with any other symlink is rejected, and the compatibility backend reads it. On Windows, creating a symlink needs special privileges, so the native engines reject every archive that contains one.

## Integrity checks

Both native ZIP modes run these checks before publishing any output:

- Decoded lengths and CRCs match the central directory.
- Each local header's name, method, CRC and sizes match its central-directory entry. With a data descriptor, a local CRC or size may be zero or `0xFFFFFFFF` instead.
- Local records tile the archive. The first starts at offset 0, and each ends exactly where the next record or the central directory starts.
- With a data descriptor, the bytes after the data are exactly one descriptor of 12, 16, 20 or 24 bytes, and its CRC and sizes match the central directory.
- The local encryption flag is clear, and any local ZIP64 sizes match the central directory.
- No local or central record has two ZIP64 extra fields. Readers disagree on which one wins.
- Each Deflate stream ends exactly at its compressed size, so no bytes can hide after it.
- A stored entry followed by a data descriptor does not contain a descriptor whose CRC and sizes match the bytes before it. A streaming reader would end the entry there. Descriptors that describe other data, as in a nested archive, are allowed.
- Names are unique and safe, and no file is also used as a directory.
- A Unicode path extra field (`0x7075`), local or central, names the entry as its stored name does. The `zip` crate renames an entry from the central field and some streaming readers use the local one, so a different name is rejected and left to the compatibility backend. The field may spell a CP437 stored name in UTF-8 when its CRC matches the stored bytes. Readers that honor the field and readers that decode CP437 then reach the same name. bsdtar and 7-Zip use neither on some archives, and they already disagree on CP437 names without a field. A field whose content is not UTF-8 is rejected. A field with a stale CRC must repeat the stored bytes, because readers that check the CRC ignore it.

`list` runs only the central-directory checks, including the central Unicode path and ZIP64 field checks, and the first-record check. It does not read local headers, so it does not detect bytes hidden between records or a Unicode path field present only in a local header. `test` and `extract` detect both.

## Reference corpus

`tests/data/zip-corpus` holds 21 archives. Seventeen come from these real ZIP writers:

- Info-ZIP `zip` 3.0, in default, stored, ZIP64, data-descriptor and piped modes.
- 7-Zip, with Deflate, Copy and timestamps.
- bsdtar, with Deflate and stored entries.
- macOS `ditto`.
- Python `zipfile`, seekable and streaming, with Deflate, stored and ZIP64.

Four more archives come from Info-ZIP `zip -y`, 7-Zip `-snl`, bsdtar and `ditto`, which store symlinks as links. Only `unzip` and bsdtar create links, so link targets are recorded where those two agree, and the other paths still need three readers. On Windows the test expects these archives to be rejected.

The manifest also records each path's exact modification time where `unzip` and bsdtar both restore the source's own time under `TZ=UTC` and `TZ=Pacific/Auckland`. That covers the 11 Info-ZIP, bsdtar and `ditto` archives. The other archives carry only DOS times, which depend on the time zone. `tests/cli.rs` checks those under `TZ=UTC` and `TZ=Asia/Tokyo`.
 The source tree has empty files and directories, non-ASCII names, and files at the decoder's 64 KiB and 192 KiB slice boundaries.

`scripts/make_zip_corpus.py` extracts each archive with Python `zipfile`, Info-ZIP `unzip` 6.0, bsdtar and 7-Zip, and records in `manifest.tsv` the tree that at least three of them produce identically. `corpus.json` names any reader that disagreed. Python decodes the non-ASCII names in the Info-ZIP and `ditto` archives as CP437, because those writers store UTF-8 names without the UTF-8 flag, so it dissents on those 6 archives.

`tests/zip_corpus.rs` requires both native engines to reproduce every manifest exactly, on every operating system in CI. On Unix it also compares permission bits where `unzip` and bsdtar agree, which they do on all 21 archives. The modes were recorded under umask `022`, and the test masks them with the current umask. Python restores no modes and 7-Zip skips directory modes, so neither votes on modes. Regenerate the corpus on macOS with `python3 -I scripts/make_zip_corpus.py`. Java `jar` output is missing because no JDK was installed.

## Mutation testing

`tests/zip_fuzz.rs` mutates corpus archives with seeded bit flips, truncations, insertions and edge values written into header fields. Both engines extract every mutant. Each case must either fail and leave nothing in the destination's parent, or succeed with verification agreeing and exactly the declared bytes written. A panic fails the test.

The gate runs 1,000 cases. To run more, or to replay a reported failure:

```sh
ZIPPER_FUZZ_CASES=300000 cargo test --release --test zip_fuzz
ZIPPER_FUZZ_SEED=303 ZIPPER_FUZZ_CASES=1 cargo test --release --test zip_fuzz
```

A run of 300,000 cases on 2026-11-01 at this revision found no failure. In a 20,000-case sample, 3,588 of 40,000 extractions accepted their mutant, mostly through changed timestamps, attributes or comments. The rest were rejected. This is random mutation, not coverage-guided fuzzing. A coverage-guided `cargo fuzz` target needs a nightly toolchain and is not set up.

## Engines

### `native`

`native` streams every entry. Each worker reads compressed input in 64 KiB slices and decodes Deflate with zlib-rs into a 192 KiB output slice, both carved from one 256 KiB buffer.
Streaming buffers scale with workers, not expanded file size.

### `native-fast`

`native-fast` adds a libdeflate buffer decoder.
A shared budget covers live compressed and expanded entry buffers across workers.

| Platform | Default buffer budget |
| --- | --- |
| ARM64 macOS | 128 MiB |
| Other platforms | 64 MiB |

`--zip-buffer-mib N` overrides the budget.
`--zip-buffer-mib 0` forces streaming.
Entries that cannot reserve space immediately stream through zlib-rs.

For extraction, an entry also streams when both of these hold:

- Its expanded size is at least 16 MiB.
- Its compressed size is at most 1/128 of its expanded size.

On the measured highly compressible workloads, streaming these entries is faster and uses much less memory.
Integrity-only selection is unchanged.
The [decoder-selection measurements](../../benchmarks/native-zip/decoder-selection/README.md) record the evidence.
The [shared-budget measurements](../../benchmarks/native-zip/shared-budget/README.md) record the budget evidence.

## Memory model

Parsed metadata is shared across workers.
The `native-fast` budget is not a peak RSS cap.
Metadata, worker state, the archive cache, and allocator-retained memory are additional to it.
A streamed entry with a writer thread also holds five 1 MiB write buffers, outside the budget.

## Concurrency

Automatic native ZIP extraction on macOS uses at most four workers when both of these hold:

- The archive has at least 1,024 regular files.
- Every regular file is at most 16 KiB.

The limit avoids the measured filesystem contention from using every CPU.
An explicit `--threads N` overrides it.
Verification-only reads and larger files keep their existing concurrency.
Windows uses one worker under the same conditions when one folder holds at least 1,024 of the files, because parallel creation in one folder was slower than serial there. Linux has no cap.
The [small-file ZIP versus bsdtar measurements](../../benchmarks/native-zip/bsdtar-comparison.md) and the [Linux and Windows scoreboard](../../benchmarks/cross-platform/scoreboard-2026-11-02/README.md) record the evidence.

A streamed entry of at least 16 MiB gets a writer thread when the pool has more threads than entries with a payload. Every entry that is not a directory counts, including symlinks. The worker then decodes while the writer thread writes, so the number of busy threads never exceeds `--threads`. Spare threads are counted once, before extraction starts. `--threads 1` never gets a writer thread. The [write-behind measurements](../../benchmarks/native-zip/write-behind/README.md) record the evidence.

## Input read-ahead

| Platform | Input buffer |
| --- | --- |
| Windows | starts at 8 KiB, grows to 256 KiB after sustained sequential reads |
| Other platforms | 8 KiB |

On Windows, moving seeks reset the read-ahead to 8 KiB.
The growth adds up to 248 KiB per active file reader outside the entry-buffer budget.
The [platform-specific measurements](../../benchmarks/native-zip/adaptive-input/README.md) record the evidence.

## Output write size

On Windows, buffered output is written in 256 KiB slices.
The [large-entry measurements](../../benchmarks/native-zip/windows-write-size/README.md) show why.

## Archive cache

For `native-fast test` and Linux `native-fast` extraction, Zipper caches archives up to 8 MiB once.
Independent read cursors share the cached bytes.
Larger archives use the file reader.
The cache stays disabled for extraction on other platforms.

The 8 MiB limit covers retained archive bytes, not total peak RAM.
Loading, conversion, metadata, and decoder buffers add memory.

On the measured Linux runner, the cache improved grouped and flat tiny-file extraction by 5.9× and 2.0× at four workers.
The [matched Linux comparisons](../../benchmarks/native-zip/linux-cache/README.md) record the evidence.
The [threefold-speed target investigation and verification results](../../benchmarks/native-zip/threefold/README.md) cover related work.
