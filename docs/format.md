# Experimental native format v1

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
