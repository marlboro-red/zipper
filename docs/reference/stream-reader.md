# Single compressed file reader

Zipper reads a file that holds one gzip, bzip2, xz, zstd, lzip or LZMA stream, and no tar, with its own reader. `zipper extract notes.txt.gz -o out` writes `out/notes.txt`.

## Supported files

- gzip, including several gzip members in a row, which decode as one file.
- bzip2, including several bzip2 streams in a row. Each stream after the first must bring at least 4 KiB of output, because each one costs a block of several MiB to start. A file of many tiny streams goes to `--zip-engine backend`.
- xz, including several xz streams in a row.
- zstd, including several frames in a row.
- lzip, versions 0 and 1, including several members in a row, decoded by liblzma one member at a time. 7-Zip does not read lzip. Zero padding after the last member, which bsdtar writes to standard output, is refused as trailing data.
- LZMA in its old `.lzma` form. It has no signature, so a file counts as LZMA only when its name ends in `.lzma`, its first byte is a valid properties byte, and no ZIP, tar or 7z reader claims it first.

`tests/data/stream` holds the lzip file, which 7-Zip's parity corpus cannot. `scripts/make_stream_files.py` writes it with bsdtar and checks it with a second decoder.

The default engine sends a file here when it starts with one of these signatures and the [tar reader](tar-reader.md) finds no tar inside. A tar inside any of them goes to the tar reader, which extracts its files.

## Output name

The decoded file is named as 7-Zip names it:

- `.gz`, `.gzip`, `.bz2`, `.bzip2`, `.xz`, `.zst`, `.zstd`, `.lz` and `.lzma`, in any letter case, are dropped.
- `.tgz`, `.tbz`, `.tbz2`, `.txz`, `.tzst`, `.tpz` and `.tlz` become `.tar`.
- Any other extension is dropped, and a name without one gains `~`. So the decoded file never takes the archive's own name, even in the archive's own folder.
- `\` and `:` become `_`, and trailing spaces are dropped, with trailing dots too on Windows. A name left as `.` or `..` becomes `_`, and a name that is still not safe, such as `CON` on Windows, gains a leading `_`.

A name stored inside a gzip header is ignored, because it could hold a path.

## Safety checks

- The output goes to a hidden staging folder and is published only when the whole stream decodes, as with every reader.
- The decoded size must stay within free space, or within `--max-extracted-bytes`, checked while decoding.
- Every decoder checks the checksum its format carries. gzip has a CRC-32, bzip2 has block and stream CRCs, xz has its check, zstd has a frame checksum when the frame holds one, and each lzip member has a CRC-32 and its sizes. LZMA has no checksum.
- A truncated stream is refused at every length, and data after the last stream is refused.
- `--path`, `--include` and `--exclude` select the one decoded file by its output name.

## Tests

`tests/hostile_stream.rs` truncates each corpus file at every length, appends trailing data, damages a gzip CRC and caps the output. `tests/parity.rs` checks each corpus file against the tree 7-Zip extracts.
