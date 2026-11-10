# A writer pool for solid 7z folders of small files

Kept. Handing the small files of a solid 7z folder to four writer threads makes the dependency sources extract 1.60 times faster at default threads, 404 ms against 648 ms, with ranges apart. One thread, non-solid folders and Silesia are unchanged, and peak memory rises from 42 to 48 MiB.

## Question

Can a solid folder's worker keep decoding while other threads create and write its files? One worker decoded and wrote each solid folder, so extra threads bought nothing. A research profile of the solid dependency sources put `open` at 31% of samples, close to `decode_symbols` at 32%. The same tree as `.tar.gz` extracts far faster with the tar writer pool. The hypothesis was 1.5 to 2 times faster at default threads and no change at one thread.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `main` is `dc60a40`, binary SHA-256 prefix `2283d2b452c667aa`. `pool` is the change at `dde9552` and `079d3dc` on `c1f75bf`, prefix `a47467210923919e`. No file under `src` differs between `dc60a40` and `c1f75bf`.
- The archives, written by 7-Zip:
  - the dependency sources, 3,981 files and 67.5 MB, as solid LZMA2 7z (6.2 MB) and non-solid (12.2 MB);
  - Silesia (211.9 MB) as solid LZMA2 7z, 49.6 MB.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall milliseconds with the range:

| Archive | Threads | `main` | `pool` |
| --- | --- | --- | --- |
| Solid dependency sources | default | 648 (634 to 666) | 404 (398 to 405) |
| Solid dependency sources | 1 | 639 (628 to 666) | 635 (623 to 645) |
| Non-solid dependency sources | default | 385 (378 to 391) | 389 (374 to 399) |
| Silesia | default | 972 (970 to 982) | 975 (970 to 991) |

CPU time on the solid dependency sources rose from 0.95 s to 1.24 s at default threads, from the writer threads. Peak memory there rose from 42 to 48 MiB, and stayed at 144 MiB on Silesia. The raw JSON is beside this page.

## Decision

Kept.

- **Change.** The tar writer pool moved to `src/writer_pool.rs`. In a solid folder of more than one file, `native_7z::read_archive` decodes each file of at most 1 MiB into a pool chunk and checks its CRC. It then hands the file to one of `FILE_WRITERS` writers, four and never more than `--threads`. One thread, a test without a destination and folders of one file keep the old path.
- **Memory.** Each handed-off file holds a whole reused 1 MiB buffer, so at most 16 MiB of buffers and 16 files are in flight, apart from the 2 GiB decoder budget. The review found the first version over that bound, and a counting allocator test now fails if it returns.
- **Failure.** A CRC mismatch, a failed write, a cancel or a panicking progress callback fails the run and publishes nothing. A new TLA+ model of the hand-off holds 8 rules on Unix and Windows, and each rule failed when planted out.

## Limits

- One machine and one small-file corpus. The four writers and the 1 MiB file limit were not swept.
- Windows writer lanes were not timed for 7z.
- Each file at most 1 MiB holds a whole buffer, so a folder of many files near that size keeps only 16 in flight.

## Reproduce

Build `dc60a40`, and `c1f75bf` after `git cherry-pick dde9552 079d3dc`, with `cargo build --release`. Then, for `1` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/dependencies.7z \
  target/bench-corpora/dependencies/dependencies --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader pool native <pool binary> \
  --reader-threads main auto --reader-threads pool auto
```
