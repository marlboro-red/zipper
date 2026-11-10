# Buffered BCJ2 branch streams

Kept. Buffering the four BCJ2 input streams makes a BCJ2 7z of Silesia extract 2.25 times faster at one thread, 1.650 s against 3.715 s, with ranges far apart. 7-Zip is still faster, at 1.022 s.

## Question

Does buffering BCJ2's four input streams remove the per-byte cost of reading them? The BCJ2 decoder reads one byte per call from each stream, and each call reached the LZMA2 reader with a 1-byte buffer. So the LZMA kernel was entered and left once per output byte. A research probe counted 94.6 G instructions for Zipper against 11.9 G for 7-Zip on the codec archive, and 30.0 G with a `BufReader` on each stream. The hypothesis was about half the wall time.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `base` is main at `b98b9ee`, binary SHA-256 prefix `a08cb2c5271e1228`. `bcj2` is `77e17c8` with the buffering and the Deflate64 charge described below, which main has as `fde235c` and `e229ccb`, prefix `061ba087fba822f3`. Both are release builds. 7-Zip is 26.05.
- `7z-bcj2.7z` is the codec scoreboard's archive, Silesia (211.9 MB) through 7-Zip's BCJ2 filter and LZMA2, 49.3 MB. `silesia.7z` is plain solid LZMA2, 49.6 MB, from the [sevenz-rust2 record](../../lzma/sevenz-rust2/README.md).
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range:

| Archive | Threads | `base` | `bcj2` | 7-Zip |
| --- | --- | --- | --- | --- |
| BCJ2 | 1 | 3.715 (3.649 to 3.919) | 1.650 (1.602 to 1.728) | 1.022 (1.011 to 1.029) |
| BCJ2 | default | 3.737 (3.665 to 3.916) | 1.629 (1.598 to 1.751) | not run |
| Plain LZMA2 | 1 | 1.145 (1.126 to 1.150) | 1.144 (1.141 to 1.148) | not run |

CPU time follows wall time, 3.65 s against 1.61 s at one thread. Peak memory rose from 39 to 41 MiB on the BCJ2 archive and stayed at 37 MiB on plain LZMA2. A BCJ2 folder has one LZMA2 main stream, so default threads match one thread. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** Each branch reader is wrapped in a `BufReader` of up to 1 MiB, capped at the branch size. `bcj2_branches_reach_their_decoder_once_per_block_not_once_per_byte` counted 100,000 inner reads before the change and fails if the buffer is planted to 0 bytes.
- **Memory.** The buffer fits inside the 2 MiB per branch that `decoder_memory` already charged. Removing the spare MiB exposed one undercharge. A Deflate64 coder was charged the 64 KiB floor, but its state is 142,368 bytes. It is now charged its size, with a test that failed first.
- **Trailing bytes.** Unused bytes after a branch's end are still refused through the buffer. Planting the end check out was caught by `hostile_7z`.

Eight refused archives now fail with a different message, because the buffer reads ahead into a branch decoder. Each is still refused, as 7-Zip refuses it.

## Limits

- One Mac and one BCJ2 archive.
- The decoder still copies the main stream one byte at a time between jump opcodes. Scanning it a slice at a time could remove most of the remaining 1.6 times gap to 7-Zip. That change would touch `src/bcj2.rs` and its Kani proof, so it is a separate experiment.

## Reproduce

Build `b98b9ee`, and `77e17c8` after `git cherry-pick fde235c e229ccb`, with `cargo build --release`. Then:

```sh
python3 scripts/compare_zip_readers.py target/bench-corpora/codecs/7z-bcj2.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader bcj2 native <bcj2 binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads base 1 --reader-threads bcj2 1 --reader-threads 7zip 1
```

Use `auto` for default threads and `target/bench-work/7z-probe/silesia.7z` for the plain row.
