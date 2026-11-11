# BZip2 blocks decoded in parallel

Kept. Decoding the independent blocks of a BZip2 stream on several threads makes Silesia `.tar.bz2` extract 7.9 times faster at default threads and the BZip2 7z 5.0 times faster, with ranges apart. The 7z now extracts 4.2 times faster than 7-Zip 26.06. One thread is unchanged, and peak memory at default threads rises from about 15 to about 100 MiB. ZIP BZip2 entries are not changed.

## Question

Can Zipper decode BZip2 blocks in parallel, as lbzip2 and 7-Zip do, with every check kept? The [before-and-after record](../../research-report/before-after/README.md) put BZip2 at 1.51 times 7-Zip's time on ZIP and 7z, decoded on one thread through the `bzip2` crate. Each BZip2 block starts with a 48-bit magic at any bit position and decodes alone, so the hypothesis was a gain close to the thread count on large streams, with no change at one thread.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `main` is `25dcb88`, binary SHA-256 prefix `b754ba8cd9129d26`. `blocks` is `c77497e`, prefix `ceaa3f8652428ed8`. 7-Zip is 26.06, prefix `f45be05e8a8dbc25`.
- The archives are the codec scoreboard's Silesia (211.9 MB) as `.tar.bz2` (54.6 MB) and as BZip2 7z (54.6 MB).
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall milliseconds with the range, CPU seconds and peak memory:

| Archive | Threads | `main` | `blocks` | 7-Zip |
| --- | --- | --- | --- | --- |
| `.tar.bz2` | 1 | 3,475 (3,468 to 3,554), 14 MiB | 3,480 (3,469 to 3,533), 14 MiB | not run |
| `.tar.bz2` | 4 | 3,480 (3,472 to 3,496), 14 MiB | 976 (974 to 984), 47 MiB | not run |
| `.tar.bz2` | default | 3,483 (3,479 to 3,486), 15 MiB | 442 (429 to 467), 102 MiB | not run |
| 7z | 1 | 3,484 (3,481 to 3,494), 11 MiB | 3,486 (3,482 to 3,512), 11 MiB | 2,314 (2,312 to 2,434) |
| 7z | 4 | 1,888 (1,882 to 2,008), 15 MiB | 1,294 (1,290 to 1,353), 33 MiB | 1,603 (1,600 to 1,703) |
| 7z | default | 1,889 (1,885 to 1,892), 16 MiB | 379 (373 to 385), 98 MiB | 1,603 (1,599 to 1,607) |

CPU time at default threads was 3.68 s for the tar and 3.30 s for the 7z, against 3.50 s and 3.66 s on main. The 7z on main already ran its two folders at once. The raw JSON is beside this page.

## Decision

Kept for tar, single `.bz2` files, RPM and 7z.

- **Change.** `src/bzip2.rs` is Zipper's own BZip2 block decoder in safe Rust. `ParallelBzip2` in `src/bzip2_streams.rs` cuts the stream at block magics, decodes the blocks on up to 16 workers and passes them on in order. A block must end exactly at its cut. One that runs past it was cut at a false magic, so the reader drops the work started after it and decodes that block to its true end. Every block CRC and stream CRC is checked. One thread keeps the `bzip2` crate.
- **Memory.** `bzip2_streams::memory(threads)` reserves 12.3 MiB per worker plus 22.3 MiB, charged to the 7z decoder budget. A block of more than 4 MiB of compressed bytes is refused with more than one thread. The review found 534 MiB on 70 planted false magics, now 28 MiB at two threads.
- **Proofs.** A TLA+ model of the speculative reader holds 9 rules, 7 Kani proofs cover the scanner, bit reader and block headers, and 16 code plants and 8 model plants were each caught.

## Limits

- One machine and one corpus. The gain depends on the block count, and Silesia has about 240 blocks of 900 kB.
- ZIP BZip2 entries still decode on one thread each. Giving them block workers needs the ZIP decoder budget and progress models extended.
- One thread still uses the crate. Zipper's own decoder takes about half its instructions, which is the next change.
- Resident memory depends on the allocator, and Linux was not measured.

## Reproduce

Build `25dcb88` and `c77497e` with `cargo build --release`. Then, for `1`, `4` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-corpora/codecs/7z-bzip2.7z \
  target/bench-corpora/silesia/silesia --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader blocks native <blocks binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads main 4 --reader-threads blocks 4 --reader-threads 7zip 4
```

Use `target/bench-corpora/codecs/tar-bzip2.tar.bz2` with `--archive-format native` for the tar rows.
