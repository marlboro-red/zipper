# One Deflate64 decoder per ZIP entry

Kept. Decoding each ZIP Deflate64 entry once instead of twice makes a Deflate64 ZIP of Silesia extract 1.83 times faster at one thread, 0.412 s against 0.755 s, with ranges far apart. It is now faster than 7-Zip, which takes 0.530 s.

## Question

Can one decoder replace the two that each Deflate64 entry ran? Commit `d7ce08d` added a second decoder, fed every byte but the last, only to learn whether a stream ended before its last compressed byte. A research probe that fed one decoder the last byte in a call of its own halved the instructions, 9.37 G against 4.72 G. The hypothesis was close to half the wall time, with the same verdict on every damaged entry.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `base` is main at `b98b9ee`, binary SHA-256 prefix `a08cb2c5271e1228`. `deflate64` is `77e17c8` with the change main has as `3a3ea5c`, prefix `165caa3bffc16026`. Both are release builds. 7-Zip is 26.05.
- `zip-deflate64.zip` is the codec scoreboard's archive, Silesia (211.9 MB) as 7-Zip Deflate64 entries, 63.8 MB.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range:

| Threads | `base` | `deflate64` | 7-Zip |
| --- | --- | --- | --- |
| 1 | 0.755 (0.753 to 0.760) | 0.412 (0.411 to 0.415) | 0.530 (0.528 to 0.533) |
| default | 0.251 (0.247 to 0.256) | 0.146 (0.143 to 0.152) | not run |

CPU time fell with wall time, 0.75 s against 0.41 s at one thread and 1.01 s against 0.58 s at default threads. Peak memory stayed at 5 MiB at one thread and fell from 17 to 15 MiB at default threads. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** `inflate64` in `src/native_zip.rs` runs one decoder and gives it the last compressed byte in a call of its own. A decoder that already finished reads nothing more, so a stream with data after its end leaves that byte unread and is refused.
- **Same verdict.** The deflate64 0.1.12 decoder keeps its state across calls, reads ahead only inside the bytes it has, and pads a short Huffman code with zeros only where a canonical code has a symbol. So splitting the input changes no verdict. A unit test runs the old two-decoder check beside the new one on every variant.
- **Tests.** New cases cover a stream with no end-of-block code and a 7-Zip dynamic-Huffman entry, cut at every length and with 1 to 3 bytes added. Five planted breaks were each caught. An adversarial review ran about 711,000 differential cases and 63 plain, ZipCrypto and AES archives with the same verdict as the old code, and found nothing.

A stream that ends early with 3 or more bytes after it now reports data after its end, where it reported truncation before. It is refused in both.

## Limits

- One Mac and one Deflate64 archive.
- The argument rests on deflate64 0.1.12. A version that reads further ahead, or lets a finished decoder take input, would weaken it. The side-by-side unit test should catch either.

## Reproduce

Build `b98b9ee`, and `77e17c8` after `git cherry-pick 3a3ea5c`, with `cargo build --release`. Then:

```sh
python3 scripts/compare_zip_readers.py target/bench-corpora/codecs/zip-deflate64.zip \
  target/bench-corpora/silesia/silesia --archive-format zip --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader deflate64 native <deflate64 binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads base 1 --reader-threads deflate64 1 --reader-threads 7zip 1
```

Use `auto`, without 7-Zip, for default threads.
