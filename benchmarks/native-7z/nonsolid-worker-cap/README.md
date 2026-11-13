# A macOS worker cap of six for non-solid 7z archives of small files

Kept with a cap of six. On a 12-core Mac, six folder workers extract 10,000 flat tiny files in 365.5 ms against 607.2 at the old default, and the dependency sources in 260.5 ms against 317.0, with ranges apart. One machine was timed, and six ran as `--threads 6` on a build whose own default capped at four.

## Question

How many folder workers should a non-solid 7z of many small files use on macOS by default? Each file of such an archive is its own folder, so at default threads every core creates files at once. The [write-path study](../../write-path/study/README.md) found that this runs slower than six threads, and its traces put the cost in the kernel's file creation. The first version of the change reused the ZIP reader's cap of four. The hypothesis was a faster default on both non-solid study trees, with less CPU time.

## Setup

- One machine, the second machine of the study, an Apple M2 Max with 12 cores on macOS 27.0, on 14 November 2026. The binaries ran from `<target>`, which stands for the second machine's build folder in the JSON.
- `zipper-base` is `b49293f`, binary SHA-256 prefix `68313e5aed1f9ace`, the study's baseline. `zipper-cap` is this branch with a cap of four, prefix `f94d456695fefead`.
- The archives are the study's non-solid 7z files, each folder one file:
  - 10,000 flat tiny files in one folder, 9,536,000 bytes, 1,783,614 bytes packed;
  - the dependency sources, 3,981 files in 997 directories, 67,479,735 bytes, 12,158,891 bytes packed.
- `scripts/compare_zip_readers.py` ran 9 rounds after one warmup, in shuffled order, with no cooldown, and checked every extracted byte, size and path. Neither session lists a busy process. The one-minute load average was 2.9 at the start of session 1 and 3.2 at the start of session 2.
- `--threads 6` and the other fixed counts ran on `zipper-cap`, where an explicit count is never capped. The cap of six itself was not built for these sessions.

## Results

Session 1, flat tiny files, median wall milliseconds with the range, CPU seconds and peak resident MiB:

| Reader | Wall ms | CPU s | Peak MiB |
| --- | --- | --- | --- |
| `zipper-base`, default | 607.2 (586.4 to 643.4) | 5.19 | 12.9 |
| `zipper-base`, `--threads 4` | 420.1 (412.1 to 426.8) | 1.34 | 11.3 |
| `zipper-base`, `--threads 6` | 371.7 (365.0 to 377.7) | 1.66 | 11.6 |
| `zipper-cap`, default, which caps at four | 419.5 (414.1 to 428.0) | 1.33 | 11.5 |
| `zipper-cap`, `--threads 6` | 365.5 (363.8 to 375.4) | 1.66 | 11.5 |

Session 2, dependency sources, the same columns:

| Reader | Wall ms | CPU s | Peak MiB |
| --- | --- | --- | --- |
| `zipper-base`, default | 317.0 (301.5 to 341.7) | 2.65 | 25.8 |
| `zipper-cap`, `--threads 4` | 321.6 (312.7 to 340.2) | 1.10 | 16.1 |
| `zipper-cap`, `--threads 5` | 285.7 (276.1 to 304.7) | 1.20 | 17.0 |
| `zipper-cap`, `--threads 6` | 260.5 (256.7 to 282.7) | 1.29 | 18.0 |
| `zipper-cap`, `--threads 8` | 255.5 (242.3 to 280.0) | 1.56 | 20.5 |
| `zipper-cap`, default, which caps at four | 323.7 (317.9 to 334.3) | 1.21 | 15.9 |

- Four workers beat the default on the flat tiny files but not on the dependency sources, where the ranges overlap.
- Six workers beat both the default and four on both trees, with ranges apart. CPU time falls from 5.19 to 1.66 s on the flat tiny files and from 2.65 to 1.29 s on the dependency sources.
- Five, six and eight workers overlap on the dependency sources, so no best count among them is established.
- Peak memory is lower at every fixed count than at the old default.

The raw JSON is in `data/` beside this page.

## Decision

Kept with a cap of six.

- `CREATION_BOUND_FOLDER_WORKERS` in `src/native_7z.rs` is 6. The rule comes from `src/creation_bound.rs`, which the ZIP reader also uses. It needs at least 1,024 selected regular files with a stream, averaging at most 64 KiB, and every selected folder must hold at most one stream.
- The ZIP reader keeps its own cap of four.
- `--threads N` is never capped, and Linux and Windows keep every worker.

## Limits

- One machine was timed.
- The flat tiny files were not timed at eight workers.
- A session of the flat tiny files at four, five and eight workers was refused by the busy guard when `fseventsd` loaded the machine.
- A review measured 5,000 files of 200 bytes on a 10-core laptop at 148 ms with four workers, 201 ms with six and 265 ms with ten. Those numbers come from that review and appear in no record.
- Linux and Windows are not capped, and neither was measured.
- The six-worker rows ran as `--threads 6`, which also sizes the Rayon pool to six. The shipped default keeps the full pool, so an LZMA2 folder of at least 8 MiB could split into segments there, which these rows would not show.

## Reproduce

Build `b49293f` as `zipper-base`, and this branch with a cap of four as `zipper-cap`, each with `cargo build --release`. Build the study's non-solid archives with its `prep.py`, as its [Reproduce section](../../write-path/study/README.md#reproduce) describes. Then, for each archive and its source folder:

```sh
python3 scripts/compare_zip_readers.py <non-solid 7z> <source folder> \
  --archive-format 7z --runs 9 --cooldown 0 \
  --reader old-auto native <zipper-base> --reader new-6 native <zipper-cap> \
  --reader-threads old-auto auto --reader-threads new-6 6
```

Add a reader for each other row, with its binary and its `--reader-threads` count.
