# Experimental native formats

New writers emit v2; readers accept v1 and v2. Neither format is frozen yet.

## Version 2

All integers are unsigned little-endian. The 16-byte header contains magic
`5a 49 50 50 45 52 00 02`, block size u32, and flags u32 (must be zero).
Block size is a power of two from 64 KiB through 16 MiB; current writers use 1 MiB.

Regular file payloads are concatenated in manifest order, including across file
boundaries, then split into blocks. Every non-final block has the declared block
size. Empty files and directories contribute no payload. Blocks are independently
encoded; there is no cross-block history. Payload bytes immediately follow the
header. The compressed index follows the last block, then a fixed 40-byte footer:

| Footer field | Encoding |
| --- | --- |
| Index offset | u64, absolute byte offset |
| Stored index size | u64 |
| Decoded index size | u64, at most 64 MiB |
| Index checksum | u32 CRC32 of decoded index |
| Flags | u32, must be zero |
| Footer magic | eight ASCII bytes `ZIPIDX02` |

The index is a Zstandard-compressed buffer containing an entry count u32, entries,
block count u32, then block descriptors. No trailing index or archive bytes are
allowed. The index and payload extents must exactly fill the archive. Stored index
size is bounded by Zstandard's compression bound for 64 MiB before allocation.

| Entry field | Encoding |
| --- | --- |
| Kind | u8: 1 = directory, 2 = regular file |
| Path length and path | u32 followed by UTF-8 bytes, 1–4096 bytes |
| Size | u64; zero for directories |
| Logical offset | u64; sum of all preceding file sizes |

Paths obey the same restrictions as v1 below. Duplicate names and file-as-parent
conflicts are rejected. Readers limit entry count to one million and to what the
decoded index can contain. All extent arithmetic is checked for overflow.

| Block descriptor field | Encoding |
| --- | --- |
| Raw size | u32, 1 through block size |
| Stored size | u32, 1 through raw size |
| Codec | u8: 0 = raw, 1 = Zstandard |
| Checksum | u32 CRC32 of raw bytes |

Block positions are cumulative stored sizes starting at offset 16. Logical
positions are cumulative raw sizes. Raw blocks require equal stored/raw lengths.
The block table must cover the manifest's exact logical payload length. Decoders
enforce the exact output size and checksum for every block they read.

Listing validates metadata without reading payloads. Selective extraction looks
up file ranges and decodes only intersecting blocks; therefore it does not certify
unselected blocks. Full `test` and extraction verify all blocks. A selected small
file may require decoding its entire shared block; v2 currently bounds that extra
work by the block size rather than an entire archive. Index loading still scales
with total metadata. CRC32 is not authentication. Permissions, timestamps, links,
and extended attributes are not represented yet.

## Version 1 (read compatibility)

All integers are unsigned little-endian. No padding. Magic is eight bytes:
`5a 49 50 50 45 52 00 01`. This version is experimental, with no long-term
compatibility promise yet.

After the magic, records repeat:

| Field | Encoding |
| --- | --- |
| Kind | u8: 0 = archive end, 1 = directory, 2 = file |
| Path length | u32, 1–4096; absent for end record |
| Path | UTF-8 bytes, slash-separated relative components |
| File size | u64, files only |
| Blocks | Until uncompressed lengths sum to file size |

Empty files have zero blocks. End record is mandatory and must be followed by EOF.
Paths cannot contain empty, `.` or `..` components, backslashes, colons, or NUL.
Duplicate paths are rejected. Creators emit parent directories before children.

Each block contains codec u8 (0 = raw, 1 = independent Zstandard frame), raw length
u32, stored length u32, CRC32 u32 of raw bytes, then stored bytes. Both lengths are
1–1,048,576 bytes. A raw block must have equal lengths. Decoders validate exact
decoded length and checksum. Encoders choose raw when compression does not shrink
a block. The format permits independently decoding blocks; the initial engine
uses ordered batches of up to eight.

Metadata has no checksum. CRC32 is not a cryptographic integrity mechanism.
