# Create each 7z output folder once

Kept, with no measured speed change. The 7z reader now makes each output folder once instead of once per item, but on the dependency sources every timing range overlaps the old code's. The change is kept because it is six lines and matches the ZIP reader.

## Question

Does skipping the repeated `create_dir_all` calls make small-file 7z extraction faster? A research profile counted 4,978 calls for 997 folders on the dependency sources, each repeat a failed `mkdir` and a `stat`, and put `std::sys::fs::metadata` at 1.3% of samples. The hypothesis was about 2% less CPU time.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `base` is main at `b98b9ee`, binary SHA-256 prefix `a08cb2c5271e1228`. `folders` is `ffdf359`, prefix `c993bab40ebb5947`. Both are release builds.
- The archives are the dependency sources, 67.5 MB in 3,981 files, as solid LZMA2 7z (6.2 MB) and non-solid LZMA2 7z (12.2 MB), from the [sevenz-rust2 record](../../lzma/sevenz-rust2/README.md).
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.

## Results

Median wall seconds with the range:

| Archive | Threads | `base` | `folders` |
| --- | --- | --- | --- |
| Solid | 1 | 0.454 (0.437 to 0.469) | 0.446 (0.436 to 0.477) |
| Solid | default | 0.441 (0.436 to 0.456) | 0.447 (0.410 to 0.481) |
| Non-solid | 1 | 0.666 (0.652 to 0.702) | 0.654 (0.646 to 0.669) |
| Non-solid | default | 0.305 (0.299 to 0.312) | 0.302 (0.292 to 0.332) |

CPU time matched within 0.01 s on every row. The raw JSON is beside this page.

## Decision

Kept as a cleanup, not as a speedup. `tests/counters.rs` checks that a 7z extraction makes at most one folder call per folder, and it failed when the check was planted out. The set holds one path per folder, at most the item names the header already holds.

## Limits

- One Mac and one small-file corpus. The removed calls fail fast on APFS, and a file system where a failed `mkdir` costs more may show a gain.
- A check of only the previous folder left 16 calls for 10 folders on the corpus archive, because 7z lists folders and files in no folder order. That is why the reader keeps a set.

## Reproduce

Build `b98b9ee` and `ffdf359` with `cargo build --release`. Then, for each archive and for `1` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/dependencies.7z \
  target/bench-corpora/dependencies/dependencies --archive-format 7z --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader folders native <folders binary> \
  --reader-threads base 1 --reader-threads folders 1
```
