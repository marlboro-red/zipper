# Native archive format

This page specifies the experimental native `.zipper` archive formats. Writers
emit version 2. Readers accept version 1 and version 2. Neither format is frozen.

## Version 2

All integers are unsigned little-endian.

### Header

The header is 16 bytes long. It contains these fields, in order:

| Header field | Encoding |
| --- | --- |
| Magic | eight bytes `5a 49 50 50 45 52 00 02` |
| Block size | u32 |
| Flags | u32, must be zero |

The block size is a power of two from 64 KiB through 16 MiB. Writers default to
1 MiB. `--block-size-kib` selects another supported size.

### Layout

An archive contains these parts, in order:

1. The 16-byte header.
2. The payload blocks.
3. The compressed index.
4. A fixed 40-byte footer.

Payload bytes start immediately after the header. The compressed index follows
the last block. No trailing index or archive bytes are allowed. The index and the
payload extents must exactly fill the archive.

### Payload blocks

The writer concatenates regular file payloads in manifest order, across file
boundaries, then splits the result into blocks. Every non-final block has the
declared block size. Empty files and directories contribute no payload.

Each block is encoded independently. There is no cross-block history.

### Footer

| Footer field | Encoding |
| --- | --- |
| Index offset | u64, absolute byte offset |
| Stored index size | u64 |
| Decoded index size | u64, at most 64 MiB |
| Index checksum | u32 CRC32 of decoded index |
| Flags | u32, must be zero |
| Footer magic | eight ASCII bytes `ZIPIDX02` |

Readers bound the stored index size by Zstandard's compression bound for 64 MiB
before they allocate.

### Index

The index is a Zstandard-compressed buffer. Decoded, it contains these fields, in
order:

1. Entry count, u32.
2. The entries.
3. Block count, u32.
4. The block descriptors.

| Entry field | Encoding |
| --- | --- |
| Kind | u8: 1 = directory, 2 = regular file |
| Path length and path | u32 followed by UTF-8 bytes, 1–4096 bytes |
| Size | u64; zero for directories |
| Logical offset | u64; sum of all preceding file sizes |

Paths follow the same restrictions as [version 1 paths](#records). Readers reject
duplicate names and file-as-parent conflicts. Readers limit the entry count to one
million and to the number of entries that the decoded index can contain. All
extent arithmetic is checked for overflow.

| Block descriptor field | Encoding |
| --- | --- |
| Raw size | u32, 1 through block size |
| Stored size | u32, 1 through raw size |
| Codec | u8: 0 = raw, 1 = Zstandard |
| Checksum | u32 CRC32 of raw bytes |

A block's archive position is the cumulative stored size of the blocks before it,
starting at offset 16. A block's logical position is the cumulative raw size of
the blocks before it. A raw block requires equal stored and raw lengths. The block
table must cover the exact logical payload length of the manifest. Decoders
enforce the exact output size and checksum of every block they read.

### Verification scope

Listing validates metadata without reading payloads. Full `test` and full
extraction verify all blocks.

Selective extraction looks up file ranges and decodes only the blocks that
intersect them. It therefore does not certify unselected blocks. A selected small
file can require decoding its entire shared block. Version 2 bounds that extra work
by the block size, not by the whole archive. Index loading still scales with total
metadata.

### Not represented

Version 2 does not represent permissions, timestamps, links, or extended
attributes. CRC32 is not authentication.

## Version 1 (read compatibility)

All integers are unsigned little-endian. There is no padding. The magic is eight
bytes, `5a 49 50 50 45 52 00 01`. Version 1 is experimental and has no long-term
compatibility promise.

### Records

Records repeat after the magic:

| Field | Encoding |
| --- | --- |
| Kind | u8: 0 = archive end, 1 = directory, 2 = file |
| Path length | u32, 1–4096; absent for end record |
| Path | UTF-8 bytes, slash-separated relative components |
| File size | u64, files only |
| Blocks | Until uncompressed lengths sum to file size |

Empty files have zero blocks. The end record is mandatory, and EOF must follow it.

Paths cannot contain empty, `.` or `..` components. Paths cannot contain
backslashes, colons, or NUL. Readers reject duplicate paths. Creators emit parent
directories before their children.

### Blocks

Each block contains these fields, in order:

| Block field | Encoding |
| --- | --- |
| Codec | u8: 0 = raw, 1 = independent Zstandard frame |
| Raw length | u32 |
| Stored length | u32 |
| Checksum | u32 CRC32 of raw bytes |
| Stored bytes | stored length bytes |

Both lengths are 1–1,048,576 bytes. A raw block must have equal lengths. Decoders
validate the exact decoded length and the checksum. Encoders choose raw when
compression does not shrink a block.

The format permits decoding blocks independently. The initial engine decodes
ordered batches of up to eight blocks.

### Integrity

Metadata has no checksum. CRC32 is not a cryptographic integrity mechanism.
