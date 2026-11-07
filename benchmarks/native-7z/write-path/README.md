# 7z write path with one CRC pass and no extra copy

**Kept two changes of three. Hashing each byte once and writing straight from the decode buffer made solid Silesia 7z extract 1.25% faster at one thread, with ranges apart at each step.** Allocating the LZMA window zeroed once was rejected before timing, because it raised memory 12 times on many small xz entries. One Apple M2 Max.

## Question

A profile of a one-thread solid Silesia extraction found 95% of the gap to 7-Zip in the LZMA decoder. Outside it, Zipper did three things 7-Zip does not:

- it computed a whole-folder CRC over every byte as well as each file's CRC, even when the folder declares none;
- it copied each buffer again through a `BufWriter`;
- it zero-filled the decoder window in steps.

Do the changes that remove them make extraction faster? The metric is the median wall time of `zipper extract` at one thread.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0, release builds. macOS background services were active, as in the [kernel record](../../lzma/7zip-study-kernel/README.md).
- Three binaries, with SHA-256 prefixes:
  - `46a8df0`, the parent, `c05cb51aa9a4217a`;
  - `bb0d16c`, which computes a folder CRC only when it is declared and not already covered, and folds it from the file CRCs, `44e57566171336eb`;
  - `33559b5`, which on top of that fills the 1 MiB buffer and writes it straight to the file, `c6ab6f17e4340b45`.
- These commits were rebased onto main after the timing as `a50cfec` and `1eed195`, which keep the 7z AES checks around the same code.
- `scripts/compare_zip_readers.py --archive-format 7z --runs 15 --cooldown 3 --allow-busy` ran each archive at one thread in rotating order, after one warmup round. Every run checks each extracted byte. Archives come from the [7z switch record](../own-decoder-shipped/README.md).
- A new test covers a declared folder CRC with and without file CRCs, and a wrong password on an encrypted solid folder by any declared CRC. Eight planted breaks were caught, and an adversarial review of 90 cases found nothing.

## Results

Median wall time in seconds, with the range over 15 runs. Raw results are in the JSON files beside this page.

| Archive | `46a8df0` | `bb0d16c` | `33559b5` |
| --- | --- | --- | --- |
| `silesia.7z` | 1.4846 (1.4820 to 1.4902) | 1.4746 (1.4726 to 1.4788) | 1.4661 (1.4637 to 1.4725) |
| `silesia.7z`, test only | 1.4531 (1.4510 to 1.4564) | 1.4448 (1.4419 to 1.4491) | 1.4428 (1.4410 to 1.4561) |
| `silesia-nonsolid.7z` | 1.4793 (1.4773 to 1.5559) | 1.4700 (1.4687 to 1.4733) | 1.4640 (1.4619 to 1.4667) |
| `dependencies-nonsolid.7z` | 0.9068 (0.8929 to 0.9273) | 0.9105 (0.8963 to 0.9515) | 0.9061 (0.8855 to 0.9393) |

The builder counted about 135 M fewer retired instructions on `silesia.7z` after both changes, and peak footprint fell from 37.4 to 36.3 MB.

The third change allocated the window with `vec![0; …]` once instead of `resize(…, 0)`. The workload was a ZIP of 2,000 small xz entries with 8 MiB dictionaries. It raised retired instructions from about 1.77 to 2.77 G, and peak memory from 3.4 to 11.9 MB at one thread and from 6.3 to 76 MB at eight. The allocator re-zeroes each reused window in full.

## Decision

Kept both timed changes. Each lies wholly below its parent on both Silesia archives, and together they take 18 ms off solid Silesia. Many small files are not slower, with every range overlapping. The test-only run gains from the CRC change alone, since it writes nothing.

Rejected the zeroed window without timing it. Its memory and work grow with the dictionary instead of the output, which breaks the memory budget's intent on many small streams. The patch was not committed.

## Limits

- One machine and one session, with background services active. Linux and Windows were not measured.
- The timed binaries predate the rebase onto the 7z AES code. That code wraps encrypted folders only, and none of these archives is encrypted.
- 7-Zip is still about 11% faster at one thread. The rest of the gap is in the LZMA decoder.

## Reproduce

Build release binaries at `46a8df0`, `bb0d16c` and `33559b5`. Then run `scripts/compare_zip_readers.py` on each archive with `--archive-format 7z --runs 15 --cooldown 3`, the three binaries as `--reader NAME native PATH`, and `--reader-threads NAME 1` for each. Add `--operation test` for the test-only row.
