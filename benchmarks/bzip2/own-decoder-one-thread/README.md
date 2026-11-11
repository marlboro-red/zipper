# Zipper's own BZip2 decoder at one thread and in ZIP

Kept. Zipper's own BZip2 decoder replaces the `bzip2` crate at one thread and in ZIP, xar and DMG, and Silesia BZip2 extracts 1.66 times faster at one thread, faster than 7-Zip 26.06. ZIP BZip2 at default threads extracts 1.47 times faster. Peak memory rises, and ZIP now charges each BZip2 entry to its decoder budget.

## Question

Does Zipper's own block decoder, built for [parallel BZip2](../block-parallel/README.md), also beat the `bzip2` crate on one thread? It took about half the crate's instructions in counters. ZIP, xar and DMG still decoded BZip2 through the crate, one stream per entry. The hypothesis was a one-thread time near 7-Zip's, which took 2,314 ms against main's 3,488 ms.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `main` is `57decf8`, binary SHA-256 prefix `5cab7805f4af7072`. `own` is `57decf8` with the commits main has from `9c898aa` to `7b85a02`, prefix `11c4817632e4b70a`. 7-Zip is 26.06, prefix `f45be05e8a8dbc25`.
- The archives are the codec scoreboard's Silesia (211.9 MB) as `.tar.bz2`, as BZip2 7z and as a ZIP of BZip2 entries.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall milliseconds with the range and peak memory:

| Archive | Threads | `main` | `own` | 7-Zip |
| --- | --- | --- | --- | --- |
| 7z | 1 | 3,488 (3,487 to 3,499), 11 MiB | 2,102 (2,098 to 2,105), 25 MiB | 2,314 (2,311 to 2,319) |
| ZIP | 1 | 3,493 (3,483 to 3,496), 9 MiB | 2,100 (2,097 to 2,104), 32 MiB | 2,314 (2,307 to 2,338) |
| `.tar.bz2` | 1 | 3,480 (3,476 to 3,748), 14 MiB | 2,082 (2,067 to 2,212), 22 MiB | not run |
| ZIP | default | 1,088 (1,074 to 1,167), 62 MiB | 740 (731 to 780), 94 MiB | 1,645 (1,640 to 1,789) |
| `.tar.bz2` | default | 442 (434 to 451), 100 MiB | 441 (429 to 462), 96 MiB | not run |

Every changed row has ranges apart. The tar row at default threads already used the block decoder on main. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** `ParallelBzip2` decodes in place when it has no block workers, and ZIP, xar and DMG use it with a rule for what may follow the stream. ZIP and DMG refuse anything after it, and xar hashes the rest. At four decoders or fewer a block's inverse transform is walked from both ends. The `bzip2` crate is now only a test oracle.
- **Memory.** ZIP charges each BZip2 entry 13.7 MiB, or 17.1 MiB when walked from both ends, through the decoder budget that XZ, LZMA and PPMd use. On 96 entries of 6 MiB, peak memory at 10 threads is 97 to 98 MiB against 53 MiB on main, and at 64 threads about 1.7 times main's 310 MiB, under the reservation.
- **Acceptance.** The 4 MiB limit on a block's compressed bytes now holds at every thread count, so a padded code table the crate refused is accepted up to that limit.
- **Review.** An adversarial review ran 1,200 damaged streams, 13,930 blocks of the two-ended walk and crafted ZIP, DMG and xar layouts, and every verdict matched libbzip2. Its one confirmed finding, ZIP memory that no budget charged, led to the charge above, with a test that failed first.

## Limits

- One machine and one corpus. The two-ended walk's threshold of four decoders comes from counters on an Apple M5.
- Peak memory is about 1.7 times main's for ZIP BZip2 at high thread counts. The budget bounds it, but a streaming block decoder would hold less.

## Reproduce

Build `57decf8`, and `57decf8` after `git cherry-pick 9c898aa^..7b85a02`, with `cargo build --release`. Then, for `1` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-corpora/codecs/zip-bzip2.zip \
  target/bench-corpora/silesia/silesia --archive-format zip --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader own native <own binary> \
  --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads main 1 --reader-threads own 1 --reader-threads 7zip 1
```
