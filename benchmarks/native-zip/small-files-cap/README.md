# A macOS worker cap for ZIPs of mostly small files

Kept. On macOS, ZIP extraction now uses four workers when the files average at most 64 KiB. The dependency sources extract 11% faster and the mixed tree 10% faster at default threads, with ranges apart and CPU time about halved. Silesia, which has large files, keeps every worker and is unchanged.

## Question

Which ZIPs should the macOS four-worker cap cover? The cap applied only when every file was at most 16 KiB, so the dependency sources and the mixed tree, each with a few larger files, ran ten or twelve workers. A research profile put 81 to 95% of their extraction time in the kernel, mostly `open`, and the scoreboard of 2 November showed four threads beating the default on both. The hypothesis was a faster default on archives of mostly small files, with no change on archives of large files.

## Setup

Two machines, compared only within each machine:

- **Thread sweep.** Apple M5 with 10 cores, macOS 26.7.1, on 11 November 2026. main at `8ba0aa0`, binary SHA-256 prefix `09444d3789d2dad5`, at 2, 3, 4, 6 and 8 threads and the default. A non-solid 7z of the dependency sources ran at 1 to 8 threads too.
- **The change.** A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. `main` is `dc60a40`, prefix `2283d2b452c667aa`, and `cap` is `5197fe3`, prefix `abacbfe879eadb73`, built before a rebase that changed no file under `src`, both at default threads.
- Archives, from the scoreboard corpora:
  - the dependency sources, 3,981 files averaging 16,950 bytes, 14.3 MB as ZIP;
  - the mixed tree, 4,002 files averaging 12,474 bytes, 20.9 MB;
  - 10,000 flat tiny files averaging 954 bytes, 3.0 MB;
  - Silesia, 12 files averaging 17.7 MB, 68.2 MB.
- No build, Kani or TLC ran during either session, and no timed run lists a busy process. `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte.

## Results

The change on the M2 Max, median wall milliseconds with the range and CPU seconds:

| Archive | `main` | `cap` |
| --- | --- | --- |
| Dependency sources | 378 (361 to 392), 2.09 s CPU | 335 (322 to 343), 1.14 s CPU |
| Mixed tree | 239 (236 to 272), 1.65 s CPU | 216 (213 to 221), 0.75 s CPU |
| Flat tiny files | 426 (419 to 440) | 428 (422 to 442) |
| Silesia | 119 (115 to 122) | 116 (115 to 120) |

The flat tiny files were already capped, and Silesia does not meet the rule, so neither changes.

The sweep on the M5, median wall milliseconds:

| Archive | 2 | 3 | 4 | 6 | 8 | Default |
| --- | --- | --- | --- | --- | --- | --- |
| Dependency sources | 245.5 | 205.7 | 208.1 | 232.1 | 251.7 | 263.1 |
| Mixed tree | 184.6 | 158.6 | 152.3 | 171.8 | 192.0 | 197.1 |
| Flat tiny files | 425.7 | 380.7 | 357.3 | 432.6 | 526.9 | 376.8 |
| Silesia | 132.5 | 105.3 | 88.3 | 88.1 | 91.4 | 81.8 |

On the M5, four workers beat the default by 21% on the dependency sources and 23% on the mixed tree, with ranges apart, while Silesia was fastest at the default. The non-solid 7z reached 0.280 s at four threads against 0.295 s at the default, with ranges overlapping. Its CPU time halved. The raw JSON is in `m2-max/` and `m5-sweep/` beside this page.

## Decision

Kept for ZIP.

- **Rule.** `CreationBound::SmallMean` in `src/native_zip.rs` caps workers at four on macOS when an archive has at least 1,024 regular files that average at most 64 KiB. The sum saturates, so a declared size cannot overflow it. A unit test covers the boundaries, two large files among many small ones, a Silesia shape and declared sizes of `u64::MAX`.
- **Unchanged.** Windows keeps its rule, which needs every file at most 16 KiB and 1,024 of them in one folder. Linux has no cap. `--threads N` overrides it.
- **7z.** The non-solid 7z gained no wall time, so its worker count is unchanged.

## Limits

- Two Macs and four archives. The threshold of 64 KiB sits between the mixed tree's mean and Silesia's, and an archive near it was not timed.
- The cap is about APFS file creation. Other macOS file systems, such as an external exFAT disk, were not timed.

## Reproduce

Build `dc60a40` and `5197fe3` with `cargo build --release`. Then, for each archive and its source folder:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/dependencies.zip \
  target/bench-corpora/dependencies/dependencies --archive-format zip --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader cap native <cap binary> \
  --reader-threads main auto --reader-threads cap auto
```

For the sweep, give one binary six readers with `--reader-threads` of 2, 3, 4, 6, 8 and `auto`.
