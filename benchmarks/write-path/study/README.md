# Where the time goes when Zipper writes files

No write-path change is kept from this study. On macOS the kernel's file creation limits every small-file workload, and Zipper at one thread already runs within 2% of a bare create, write and close loop on the same tree. One gap was found. A non-solid 7z of small files at default threads runs 13% to 35% slower than the same binary at six threads.

## Question

Can Zipper write extracted files faster, and can it write them in parallel while it decodes? The study asks what limits each workload. It then tests each candidate on a probe that writes the same tree with no archive and no decoder, so a candidate is judged against the write cost alone. The sentence to fill was "the write path of workload W is limited by L, and candidate C moves it by N".

## Setup

- One machine, an Apple M2 Max with 12 cores, 8 fast and 4 slow, on macOS 27.0, on 13 and 14 November 2026.
- Zipper at `b49293f`, release build, binary SHA-256 prefix `68313e5aed1f9ace`. The phase timings use the same commit with `phases.patch` applied, prefix `916e430e2162ddde`.
- 7-Zip 26.06, the system bsdtar 3.5.3 and the system `ditto`. The call traces also ran bsdtar 3.8.6.
- Three scoreboard trees, each packed as ZIP, tar, tar.gz, solid 7z and non-solid 7z by `prep.py`:
  - the dependency sources, 3,981 files in 997 folders, 67.5 MB;
  - 10,000 flat tiny files in one folder, 9.5 MB;
  - Silesia, 12 files, 211.9 MB.
- `write_probe.rs` loads a tree into memory and writes it with plain `open`, `write`, `futimens` and `close` calls, on 1 to 8 threads. Its variants change one thing each, such as `openat`, preallocation, the write size or where the file is closed.
- `trace_calls.c` is a library that counts and clocks each file system call of a tool. Its rows are not timings, because the tracer adds two clock reads per call.
- Every timed row has seven rounds after one warmup, in shuffled order. `scripts/compare_zip_readers.py`, `scripts/extraction_matrix.py` and `time_commands.py` check every extracted byte.
- `fseventsd` used 100% to 195% of a core in every session, and `data/busy-services.txt` lists it per step. The timed runs passed `--allow-busy`, and each JSON keeps its busy list.
- Sessions 1 to 3 wrote to the internal APFS volume. Session 4 wrote to an APFS disk image. Rows are compared only within one session.

## Results

### The limiter of each workload

Phase times of Zipper in milliseconds, the median of four runs from `data/phases.txt`:

| Archive | Threads | Open and plan | Stage and folders | Files and links | Folder times | Rename |
| --- | --- | --- | --- | --- | --- | --- |
| Dependency sources, ZIP | default | 2.8 | 27.8 | 161.9 | 14.3 | 0.1 |
| Dependency sources, ZIP | 1 | 2.9 | 27.4 | 297.4 | 17.4 | 0.1 |
| Dependency sources, tar | default | 0.2 | in the next column | 164.7 | 13.8 | 0.1 |
| Flat tiny files, ZIP | default | 3.3 | 2.1 | 284.6 | 0.1 | 0.1 |
| Silesia, ZIP | default | 0.5 | 0.1 | 81.1 | 0.1 | 0.1 |
| Silesia, solid 7z | default | 0.4 | 0.1 | 939.5 | 0.1 | 0.1 |

The process start and exit add about 4 ms outside these phases, from `data/inside-outside.json`.

Calls of Zipper at one thread on the dependency sources ZIP, from `data/traces.json`:

| Call group | Calls | Traced ms |
| --- | --- | --- |
| Create a file | 3,981 | 159.2 |
| Write | 4,359 | 68.9 |
| Close | 4,985 | 32.8 |
| Times and modes | 4,978 | 43.9 |
| Make a folder | 998 | 36.7 |
| Lookups | 2,020 | 14.6 |
| Archive reads | 5,259 | 5.9 |

That run spent 65 ms in user code and 356 ms in the kernel. bsdtar 3.8.6 made 51,880 lookups and 30,418 closes on the same archive, and 7-Zip made 21,448 folder calls.

- **Dependency sources and flat tiny files.** Kernel file creation limits them. One create costs about 40 microseconds, and at four threads the same 3,981 creates take 362 traced ms in total against 159 at one.
- **Silesia as ZIP, tar.gz and 7z.** Decoding limits them. The ZIP used 206 ms of user time against 48 ms of kernel time, and the solid 7z 1,533 ms against 46 ms.
- **Silesia as plain tar.** The copy to the page cache limits it, at 44.8 ms for 211.9 MB.

### The floor from the probe

Median wall milliseconds of the probe's `base` variant, with the range and CPU milliseconds, on the internal volume in session 2:

| Threads | Dependency sources | Flat tiny files |
| --- | --- | --- |
| 1 | 280.5 (273.2 to 314.7), 271 CPU | 463.2 (453.2 to 487.3), 458 CPU |
| 2 | 194.7 (187.0 to 222.1), 336 CPU | 281.3 (274.7 to 299.2), 552 CPU |
| 3 | 169.8 (162.5 to 189.2), 383 CPU | 207.9 (206.1 to 211.8), 606 CPU |
| 4 | 192.4 (176.2 to 217.3), 517 CPU | 265.7 (263.3 to 274.5), 1,025 CPU |
| 6 | 199.5 (196.1 to 212.6), 833 CPU | 310.1 (308.0 to 326.2), 1,717 CPU |
| 8 | 228.7 (218.9 to 236.8), 1,202 CPU | 400.1 (398.9 to 403.2), 2,907 CPU |

More threads past three or four buy no wall time and burn CPU in the kernel. Zipper's tar reader at one thread took 283.9 ms on the dependency sources and 471.7 ms on the flat tiny files in session 1. That is within 2% of the one-thread probe, measured in another session.

The probe's variants at one thread on the dependency sources, against `base` at 280.5 ms:

| Variant | Median ms (range) | Reading |
| --- | --- | --- |
| `create-close-only` | 146.6 (141.0 to 152.1) | Creating and closing empty files is half the cost. |
| `no-times` | 254.6 (240.9 to 263.1) | `futimens` costs 9%, and Zipper must keep it. |
| `openat` from a folder handle | 287.7 (279.4 to 295.1) | No gain. |
| `preallocate` | 293.0 (284.4 to 307.7) | No gain. |
| `close-elsewhere` | 266.4 (255.5 to 308.3) | Ranges overlap. |
| `times-after` | 291.7 (273.2 to 316.1) | No gain. |
| `largest-first` | 280.5 (255.1 to 300.2) | No gain. |

The same variants at four threads and on the flat tiny files read the same way. On Silesia every probe range spans 50 ms to several seconds, so the write size, preallocation and `F_NOCACHE` rows are inconclusive.

### Writer and worker counts

Zipper's wall milliseconds by `--threads`, median with range and CPU milliseconds, on the internal volume in session 3:

| Archive | 2 | 3 | 4 | 5 | Default |
| --- | --- | --- | --- | --- | --- |
| Flat tiny files, ZIP | 383.6 | 281.1 (274.5 to 285.5), 802 CPU | 336.3 (330.8 to 340.6), 1,262 CPU | 346.7 | 338.5 (335.6 to 343.5), 1,272 CPU |
| Flat tiny files, tar | 375.9 | 274.6 (272.7 to 276.4), 831 CPU | 333.9 (330.9 to 363.6), 1,313 CPU | 344.5 | 335.1 (330.8 to 337.8), 1,317 CPU |
| Dependency sources, ZIP | 269.7 | 221.7 (213.3 to 230.2), 543 CPU | 218.7 (209.7 to 232.0), 690 CPU | 224.6 | 236.5 (227.8 to 245.0), 816 CPU |
| Dependency sources, tar | 211.9 | 194.4 (189.7 to 198.0), 596 CPU | 195.8 (192.2 to 204.6), 770 CPU | 205.0 | 207.8 (201.7 to 209.8), 887 CPU |

On this volume three threads beat the default by 17% to 18% on the flat tiny files, with ranges apart. On the dependency sources three and four overlap.

Session 4 repeated the probe on the APFS disk image, and there four threads were fastest. The dependency sources took 144.2 ms (143.3 to 146.0) at four against 155.2 (152.1 to 159.5) at three. The flat tiny files took 275.7 (270.5 to 290.4) against 296.6 (286.8 to 311.7), with ranges that overlap. The [small-files cap record](../../native-zip/small-files-cap/README.md) found four ahead of three on an M5 too. So the best count is three or four, and it changes with the volume and the machine.

7z by `--threads` on the disk image in session 4:

| Archive | 1 | 2 | 3 | 4 | 6 | Default |
| --- | --- | --- | --- | --- | --- | --- |
| Flat tiny files, non-solid | 720.0 | 444.4 | 406.3 | 409.8 | 370.7 (369.3 to 377.2), 1,653 CPU | 568.5 (562.3 to 575.6), 4,590 CPU |
| Flat tiny files, solid | 630.5 | 420.8 | 393.7 | 367.5 | 365.9 | 367.8 (359.6 to 371.1), 1,213 CPU |
| Dependency sources, non-solid | 792.0 | 448.9 | 359.2 | 307.8 | 253.5 (251.1 to 257.4), 1,258 CPU | 293.4 (291.4 to 297.4), 2,417 CPU |
| Dependency sources, solid | 508.2 | 308.7 | 284.9 | 290.9 | 290.7 | 295.5 (288.5 to 301.6), 826 CPU |

A non-solid 7z gives each file its own folder, so at default threads twelve workers create files at once. The traces show the same thing. The 10,000 creates took 3,328 traced ms at default threads against 770 at four. The solid archives use the four-writer pool and do not have this gap.

### Against other tools

Median wall milliseconds on the ZIPs in session 2, from `data/ditto-*.json` and `data/matrix-*.json`:

| Tool | Dependency sources | Flat tiny files | Silesia |
| --- | --- | --- | --- |
| Zipper, default threads | 247.0 (0.79 s CPU) | 318.6 (1.19 s CPU) | 117.8 (0.44 s CPU) |
| Zipper, 1 thread | 424.3 (0.41 s CPU) | 554.5 (0.55 s CPU) | 273.1 (0.25 s CPU) |
| bsdtar | 528.8 | 761.6 | 465.2 |
| `ditto` | 765.8 | 1,405.7 | 295.3 |
| 7-Zip | 539.9 | 633.6 | 770.3 |

The 7-Zip row comes from the matrix runs, where Zipper at default threads took 225.4, 311.6 and 131.7 ms. The other rows share one run per tree.

## Decision

Nothing is changed. The findings, in the order of what they are worth:

- **Already done.** Zipper already writes in parallel while it decodes. The tar reader and solid 7z folders hand files to a pool of four writers. ZIP workers each decode and write one entry, capped at four when small files dominate, and a large ZIP entry writes behind its decoder. The mode is set by `open`, so each file costs four calls, which are create, write, `futimens` and close.
- **Worth building.** A non-solid 7z of small files should send its small files to the writer pool, as a solid folder does. The six-thread rows put the gain at 13% on the dependency sources and 35% on the flat tiny files, with CPU time halved or better.
- **Not supported.** A fixed count of three writers wins 17% on the flat tiny files on one volume and loses 8% on the dependency sources on another. A count that adapts at run time could take that gain, and it needs a design review before any build.
- **Rejected by the probe.** `openat`, preallocation, closing on another thread, setting times in a second pass and writing the largest files first show no gain on small files.
- **Not possible.** `clonefile` copies an existing file and extraction has none. macOS has no call that creates several files at once.

## Limits

- One machine and macOS only. Linux and Windows were not measured, and their caps are different.
- `fseventsd` was busy in every session, so each timing carries its load. What it does to the cost of a create was not measured.
- The floor comparison of the tar reader with the probe crosses two sessions. Session 3 ran right after a gate, with the load average near 5.
- The phase build adds seven clock reads per run, and its rows have four measured runs, not seven.
- The Silesia probe rows are too noisy to read, so nothing here says whether a larger write or preallocation helps large files.
- 7-Zip restores timestamps and attributes, and no flag turns that off. `ditto` also restores extended attributes.
- The six-thread rows bound the non-solid 7z gain from a cap. The writer pool design was not built, so its own gain is unmeasured.
- The JSON beside this page has the machine's paths replaced by `<checkout>`, `<target>` and `<home>`.

## Reproduce

Set `ZIPPER_CHECKOUT` to the checkout, add a worktree of `b49293f` at `target/wpath/tree-base`, and build `b49293f` with `cargo build --release` as `bin/zipper-base` under `target/wpath`. Apply `phases.patch` and build again as `bin/zipper-phases`. Then, from `target/wpath` with the files of this folder copied in:

```sh
clang -O2 -dynamiclib -o bin/syscount.dylib trace_calls.c
rustc -O -o bin/write_probe write_probe.rs
python3 prep.py "$PWD/tree-base/scripts" "$ZIPPER_CHECKOUT/target/timing/corpora" arch
python3 trace_tools.py "$PWD" "$PWD/arch" 3 > traces.json
python3 summarize_traces.py traces.json
zsh session-main.zsh
zsh session-threads.zsh
zsh session-7z-threads.zsh
```

Each session takes the remote run lock from `lock.zsh` and stops at its first failed step.
