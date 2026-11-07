# lzip, `.lzma` and ZIP LZMA on Zipper's own decoder

**Kept. With Zipper's own readers in place of liblzma, Silesia `.tar.lz` extracts 15 to 16% faster, `.lzma` 12% faster and a ZIP with the LZMA method 8 to 11% faster, with ranges apart. 128 MiB of zeros in `.tar.lz` extracts 30% faster.** The release build no longer links liblzma. One Mac, one session.

## Question

After the [xz record](../own-decoder/README.md), liblzma still decoded lzip and `.lzma` in tar, the ZIP LZMA method and RPM's LZMA payloads. The change moves all four to readers in `src/lzma.rs` and makes `liblzma` a test dependency. Does extraction stay as fast? The builder's counts showed more instructions retired on these formats, and six times as many on a stream of distance-1 matches, so a slowdown was the risk. The metric is the median wall time of `zipper extract`, with CPU time and peak memory beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Spotlight indexing off. Two builder agents were paused or idle, and no timed run lists a busy process.
- `before` is main at `54a9b0b`, binary SHA-256 prefix `389e256896854596`. `after` is the change at `cb5a141`, prefix `4e283a2b00efdf7e`.
- Archives in `target/bench-work/lz/`, made from a 211,948,544-byte tar of Silesia written with `COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata`:
  - `silesia-6.tar.lz` and `silesia-9.tar.lz`, from lzip 1.26 at `-6` and `-9`, 49,819,884 and 48,308,296 bytes;
  - `silesia.tar.lzma`, from `xz --format=lzma -6`, 49,219,562 bytes, which Zipper extracts as one file;
  - `silesia-lzma.zip`, from `7zz a -tzip -mm=LZMA -mx=5`, 49,602,952 bytes;
  - `zeros.tar.lz`, a tar of one 134,217,728-byte file of zeros at `lzip -6`, 19,133 bytes;
  - `silesia-lzma1.7z` from the [7z switch record](../../native-7z/own-decoder-shipped/README.md), 49,563,337 bytes.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 5` ran each archive after one warmup round, in interleaved order, and checked every extracted byte against the source tree. A small interleaved loop timed `.lzma` the same way, eight rounds with the first as warmup, and compared the extracted file with the tar byte for byte.

## Results

Median wall time in seconds, with the range over seven runs. Raw results are in the JSON files beside this page.

| Archive | Threads | `before` | `after` | `after` / `before` |
| --- | --- | --- | --- | ---: |
| Silesia `.tar.lz`, `-6` | 1 | 1.392 (1.365 to 1.403) | 1.177 (1.147 to 1.180) | 0.845 |
| Silesia `.tar.lz`, `-6` | 4 | 1.369 (1.359 to 1.371) | 1.152 (1.135 to 1.155) | 0.842 |
| Silesia `.tar.lz`, `-9` | 1 | 1.344 (1.327 to 1.358) | 1.129 (1.115 to 1.141) | 0.840 |
| Zeros `.tar.lz` | 1 | 0.130 (0.112 to 0.137) | 0.091 (0.087 to 0.097) | 0.696 |
| Silesia `.lzma` | 1 | 1.281 (1.269 to 1.287) | 1.131 (1.116 to 1.137) | 0.883 |
| Silesia ZIP, LZMA method | 1 | 1.293 (1.282 to 1.300) | 1.157 (1.150 to 1.165) | 0.895 |
| Silesia ZIP, LZMA method | 4 | 0.429 (0.418 to 0.483) | 0.395 (0.379 to 0.407) | 0.921 |
| `silesia-lzma1.7z` | 1 | 1.170 (1.141 to 1.174) | 1.157 (1.138 to 1.173) | 0.988 |

CPU time fell with wall time, from 1.40 to 1.19 s on `.tar.lz` at `-6` and one thread. Peak memory stayed within 1 MiB on every archive, for example 16 to 17 MiB on `.tar.lz` at `-6`.

## Decision

Kept. Every lzip, `.lzma` and ZIP LZMA cell is faster with ranges apart. The 7z LZMA archive, which already used Zipper's decoder, overlaps. The rise in retired instructions did not turn into time, as with the decoder before. The distance-1 stream is the largest gain, not a loss.

The change also removes liblzma from the release build. `cargo tree -e normal` lists neither `liblzma` nor `liblzma-sys`, and the binary holds no `_lzma_` symbols. An adversarial review of about 18,000 cases found no failure.

## Limits

- One Mac and one session. Linux and Windows were not measured.
- Four threads is the script's default, not Zipper's default thread count.
- RPM's LZMA payloads were not timed. They use the same reader as `.lzma`.
- A ZIP LZMA entry with a huge declared size now reserves its window up front and aborts if that allocation fails, where liblzma returned an error.

## Reproduce

Build release binaries at `54a9b0b` and `cb5a141`. Write the archives as Setup describes. Then, for each archive, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/lz/silesia-6.tar.lz \
  target/bench-corpora/silesia/silesia --archive-format tar --runs 7 --cooldown 5 \
  --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER
```

Use `--archive-format zip` for the ZIP and `7z` for the 7z archive, and add `--reader-threads before 1 --reader-threads after 1` for one thread.
