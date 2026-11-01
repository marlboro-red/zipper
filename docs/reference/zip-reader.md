# In-process ZIP reader

Zipper has an experimental parallel Rust ZIP reader.
`--zip-engine native` and `--zip-engine native-fast` select it for `extract` and `test`.
Without `--zip-engine`, `extract`, `list`, and `test` delegate non-native archives to the 7-Zip compatibility backend, which remains the default.
The [usage guide](../usage.md#read-zip-archives-in-process) shows the commands.

## Supported archives

The reader supports these features:

- Stored and Deflate entries.
- Conventional ZIP64.
- File bytes and directories.

The reader does not support these features:

- Encryption.
- Symlinks.
- Codecs other than stored and Deflate.
- Multi-disk archives.
- Ambiguous end records.
- Data before the first record, such as a self-extracting executable from `zip -A`. The compatibility backend still reads these.

It restores Unix permission bits the way bsdtar does:

- Each file and directory gets its archive mode, masked to `0o777`, so setuid, setgid and sticky bits are never set. Archives made on DOS or Windows give files `0o664`, directories `0o775`, and read-only entries no write bits.
- The user's umask limits every mode, as it does for a new file. Info-ZIP `unzip` ignores the umask, so it can restore wider modes such as `666` or `777`.
- Directory modes are applied after all files are written, deepest first, so a read-only directory can still receive its children. A directory entry listed after its own contents still gets its mode, which bsdtar and `unzip` skip.

It does not restore timestamps. On Windows it restores no permissions.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 100,000 |
| Raw central-directory metadata | at most 64 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

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
- A Unicode path extra field (`0x7075`), local or central, names the entry exactly as its stored name does. The `zip` crate renames an entry from the central field and some streaming readers use the local one, so a different name is rejected and left to the compatibility backend. Old archives that pair a CP437 stored name with the same name in UTF-8 are rejected too.

`list` runs only the central-directory checks, including the central Unicode path and ZIP64 field checks, and the first-record check. It does not read local headers, so it does not detect bytes hidden between records or a Unicode path field present only in a local header. `test` and `extract` detect both.

## Reference corpus

`tests/data/zip-corpus` holds 17 archives from these real ZIP writers:

- Info-ZIP `zip` 3.0, in default, stored, ZIP64, data-descriptor and piped modes.
- 7-Zip, with Deflate, Copy and timestamps.
- bsdtar, with Deflate and stored entries.
- macOS `ditto`.
- Python `zipfile`, seekable and streaming, with Deflate, stored and ZIP64.
 The source tree has empty files and directories, non-ASCII names, and files at the decoder's 64 KiB and 192 KiB slice boundaries.

`scripts/make_zip_corpus.py` extracts each archive with Python `zipfile`, Info-ZIP `unzip` 6.0, bsdtar and 7-Zip, and records in `manifest.tsv` the tree that at least three of them produce identically. `corpus.json` names any reader that disagreed. Python decodes the non-ASCII names in the Info-ZIP and `ditto` archives as CP437, because those writers store UTF-8 names without the UTF-8 flag, so it dissents on those 6 archives.

`tests/zip_corpus.rs` requires both native engines to reproduce every manifest exactly, on every operating system in CI. On Unix it also compares permission bits where `unzip` and bsdtar agree, which they do on all 17 archives. The modes were recorded under umask `022`, and the test masks them with the current umask. Python restores no modes and 7-Zip skips directory modes, so neither votes on modes. Regenerate the corpus on macOS with `python3 -I scripts/make_zip_corpus.py`. Java `jar` output is missing because no JDK was installed.

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

## Concurrency

Automatic native ZIP extraction on macOS uses at most four workers when both of these hold:

- The archive has at least 1,024 regular files.
- Every regular file is at most 16 KiB.

The limit avoids the measured filesystem contention from using every CPU.
An explicit `--threads N` overrides it.
Verification-only reads and larger files keep their existing concurrency.
Windows and Linux do not apply this macOS-specific policy.
The [small-file ZIP versus bsdtar measurements](../../benchmarks/native-zip/bsdtar-comparison.md) record the evidence.

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
