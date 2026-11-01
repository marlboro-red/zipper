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

It does not preserve permissions or timestamps.

## Limits

| Limit | Value |
| --- | --- |
| Entries | at most 100,000 |
| Raw central-directory metadata | at most 64 MiB |
| Total expanded size | no quota |

## Integrity checks

Both native ZIP modes run these checks before publishing any output:

- Decoded lengths and CRCs match the central directory.
- Each local header's name, method, CRC and sizes match its central-directory entry. With a data descriptor, a local CRC or size may be zero or `0xFFFFFFFF` instead.
- Local records tile the archive. The first starts at offset 0, and each ends exactly where the next record or the central directory starts.
- With a data descriptor, the bytes after the data are exactly one descriptor of 12, 16, 20 or 24 bytes, and its CRC and sizes match the central directory.
- The local encryption flag is clear, and any local ZIP64 sizes match the central directory.
- No local or central record has two ZIP64 extra fields. Readers disagree on which one wins.
- Names are unique and safe, and no file is also used as a directory.

`list` runs only the central-directory checks and the first-record check. It does not read local headers, so it does not detect bytes hidden between records.

These checks do not yet cover bytes after a Deflate stream ends inside its declared size, or a fake data descriptor inside stored data. The [roadmap](../roadmap.md#m1-correctness-contract-for-zip) tracks them.

## Engines

### `native`

`native` streams every entry.
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
