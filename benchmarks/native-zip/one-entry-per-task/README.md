# One entry per Rayon task

**Giving each Rayon task one entry made two Rust build folders extract 1.40× to 1.76× faster on Windows at four threads.** Tiny-file workloads did not move. Any task size from 1 to 16 entries gives the same result. Only the default, unbounded split is slow. Windows and one machine only.

## Question

ZIP extraction sorts entries largest first and runs them through `par_iter().try_for_each_init`. Rayon splits that list into contiguous runs. A worker that picks up a run must finish every entry in it before another worker can help. Near the end of an extraction, one worker can be left alone with a long run of medium entries while the others idle.

Does `with_max_len(1)`, which makes each entry its own task, remove that tail? Does it cost anything on archives of tiny files, where each task is short?

The first version created the task state, a reader clone and a 1 MiB scratch buffer, once per task. That made 10,000 tiny files 4% slower. The kept version returns each task's reader and buffer to a small pool for the next task to reuse. The Deflate decompressor is still created per task, because it is not `Send`.

## Setup

AMD Ryzen 7 5700X3D, 8 cores and 16 threads, 64 GiB, Windows 11 Pro 10.0.22631. Archives and output were on an NTFS NVMe volume. Microsoft Defender real-time protection was on, with the benchmark folder excluded. Release binaries came from rustc 1.99.1.

`scripts/compare_zip_readers.py` ran every comparison with four threads, interleaved and shuffled rounds, one warmup and a byte-for-byte check of every extracted tree.

| Workload | Files | Unpacked | ZIP | Rounds |
| --- | ---: | ---: | ---: | ---: |
| ripgrep 14.1.1 `target`, debug and release | 1,700 | 650 MiB | 179 MiB | 11 |
| Switchboard `target/release` | 16,029 | 4.59 GiB | 1.56 GiB | 7 |
| `flat-tiny` scoreboard corpus | 10,000 | 9.1 MiB | 2.9 MiB | 11 |
| `grouped` scoreboard corpus | 4,000 | 15.6 MiB | 0.9 MiB | 11 |

[`make_target_zip.py`](make_target_zip.py) stored each build folder as Deflate level 6. The baseline is `f7189d5`, except for the check against `c1fa6a1` below.

## Results

Median wall milliseconds with the full range, median CPU seconds and peak memory. Raw samples are in [`ripgrep.json`](ripgrep.json), [`switchboard.json`](switchboard.json), [`flat-tiny-fast.json`](flat-tiny-fast.json), [`flat-tiny.json`](flat-tiny.json), [`grouped.json`](grouped.json) and [`ripgrep-task-size.json`](ripgrep-task-size.json).

### Build folders

| Reader | Wall ms [range] | CPU s | Peak MiB | Speedup |
| --- | ---: | ---: | ---: | ---: |
| ripgrep, `native`, before | 783 [720–1326] | 0.45 | 13 | |
| ripgrep, `native`, after | 444 [434–760] | 0.41 | 13 | 1.76× |
| ripgrep, `native-fast`, before | 742 [731–1511] | 0.52 | 66 | |
| ripgrep, `native-fast`, after | 480 [465–529] | 0.44 | 66 | 1.55× |
| Switchboard, `native`, before | 6783 [6304–12296] | 4.52 | 22 | |
| Switchboard, `native`, after | 4666 [4471–10955] | 2.94 | 22 | 1.45× |
| Switchboard, `native-fast`, before | 6655 [6501–6957] | 4.72 | 82 | |
| Switchboard, `native-fast`, after | 4742 [4266–10118] | 3.14 | 83 | 1.40× |

Every binary had one or two runs at 1.3× to 2.6× its median. They overlap some ranges. Counted per run, 10 of 11 ripgrep `native` runs and all 11 `native-fast` runs beat the fastest baseline run. On Switchboard, 5 of 7 `native` runs and 6 of 7 `native-fast` runs did.

After rebasing onto `c1fa6a1`, an 11-round ripgrep check against that commit measured `native` at 761 [728–1019] ms before and 448 [433–1115] ms after, 1.70×. `native-fast` measured 751 [734–1287] ms before and 481 [455–677] ms after, 1.56×. Raw samples are in [`ripgrep-c1fa6a1.json`](ripgrep-c1fa6a1.json).

### Tiny files

| Workload | Before ms [range] | After ms [range] | Ratio |
| --- | ---: | ---: | ---: |
| `flat-tiny`, `native`, automatic threads | 4397 [4360–4595] | 4427 [4352–4712] | 0.99× |
| `grouped`, `native`, automatic threads | 1401 [1330–1548] | 1384 [1320–1410] | 1.01× |
| `flat-tiny`, `native-fast`, 4 threads | 3933 [3772–4077] | 3816 [3661–4071] | 1.03× |

All three ranges overlap, so there is no established change. The version without the pool measured 0.96× on `flat-tiny`.

### Task size

ripgrep, `native`, built from this change with the task size read from the binary's name.

| Entries per task | Wall ms [range] | CPU s |
| --- | ---: | ---: |
| 1 | 442 [433–456] | 0.27 |
| 2 | 442 [433–466] | 0.31 |
| 4 | 452 [435–485] | 0.31 |
| 8 | 436 [432–481] | 0.28 |
| 16 | 441 [431–476] | 0.36 |
| Unbounded, the old default | 730 [723–766] | 0.50 |

## Decision

Kept. Rayon's default split is the limiter on build folders. Any bound up to 16 removes the tail, and the bounds agree within their ranges. One entry per task is the bound that needs no tuning for the archive's shape, and the pool makes it free on tiny files.

The TLA+ checks for progress reports and the ZIP decoder budget cover `read_archive_with_options`. Both passed unchanged with deadlock checking on, and their fingerprints are refreshed.

## Limits

One machine, one NTFS volume, Windows only. Linux and macOS were not measured. Defender was on with the folder excluded. The Switchboard folder is a local build, so its bytes cannot be rebuilt exactly. The ripgrep folder can. The outlier runs appeared in every binary and were not traced. CI was not run. On Windows, `reported_bytes_never_run_ahead_of_bytes_on_disk` in `tests/control.rs` fails at `f7189d5` and with this change alike.

## Reproduce

```powershell
git clone --branch 14.1.1 https://github.com/BurntSushi/ripgrep
cd ripgrep; cargo build --release; cargo build; cd ..
python benchmarks/native-zip/one-entry-per-task/make_target_zip.py ripgrep/target ripgrep-target.zip
python scripts/compare_zip_readers.py ripgrep-target.zip ripgrep/target --runs 11 --threads 4 `
  --reader before native zipper-before.exe --reader after native zipper-after.exe
python scripts/compare_workloads.py --before zipper-before.exe --after zipper-after.exe `
  --workloads flat-tiny grouped --formats zip --runs 11 --output-dir compare
```
