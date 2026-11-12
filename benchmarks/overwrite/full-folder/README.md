# Overwrite into a folder that already holds the archive

**Kept four changes and reverted two. `--overwrite` into a folder that holds all 28,000 files of the archive now takes 2.16 s against 1.20 s into an empty folder, 1.80 times, down from 5.19 times. It does not reach 1.5 times, because removing the 28,000 old files takes 0.64 s on four threads, cannot start before the new files are in place, and gets slower with more threads on APFS.**

## Question

`zipper extract --overwrite` into a folder that already holds every file of the archive took 6.5 s for 28,000 small files on an Apple M2 Max, against 1.2 s into an existing empty folder. M29 asks for that gap to be at most 1.5 times, with every safety rule kept. Where does the time go, and which change closes the gap? The metric is the median wall time of the full-folder run against the empty-folder run of the same binary, with CPU time beside it.

## Setup

- The fixture is [`make_archive.py`](make_archive.py) with seed 27874. It writes `tree/pkgNNN/subK/fileNNN.txt`, 28 packages of 10 folders with 100 files each, 28,000 files of random repeated text lines. `scripts/extraction_matrix.py`'s `write_zip` zips it with Deflate level 6, 14,379,844 bytes, with an entry for each of the 309 folders. Its SHA-256 starts `3b37ded0fbc48f48`.
- Every timing and profile ran on the second machine, an Apple M2 Max with 12 cores, macOS 27.0, APFS, on 12 November 2026. The timed folder sits under a folder that Finder and Spotlight watch, and `fseventsd`, `mds` and Finder used CPU during the runs of every binary.
- [`time_overwrite.py`](time_overwrite.py) runs each binary once as a warmup and then interleaves them, reversing the order every round. Mode `full` leaves the destination holding every file from the run before. Mode `empty` empties the destination before each run. It compares the last tree of each binary with the source byte for byte. [`session.sh`](session.sh) holds the lock that keeps a remote gate off the machine, and runs both modes.
- The profiles used `samply record -r 4000` on one full-folder run per binary, outside every timed run. [`split.py`](split.py) splits the samples by stage from the function names on the stack. Times are samples at 0.25 ms, so they add up to a thread's busy time.
- [`operations.py`](operations.py) and [`removal_threads.py`](removal_threads.py) time single filesystem operations from Python on 28,000 files of 1.5 kB in 280 folders. They explore, and no verdict rests on them alone.
- The binaries, with their SHA-256 prefixes. The first six were built from consecutive commits of the branch `m29-fable-on-6d15ca2`, before the work was rebased onto main. On main the same changes are `61038e0`, `5f5f266`, `9a1c336`, `4537885`, `e946e05` and `86066d1`, with the same `src/staging.rs` at each. The last two are on the branch `m29-overwrite`, with `3255a0a`, the commit they were timed against.
  - `6d15ca2`, the base, `7e4368d7093e2c97`;
  - `4c9496d`, the folder set, `27d650e39c4819d9`;
  - `8adc362`, the folder replaced whole, `6c72b42d7866918a`;
  - `cfc63dd`, removal on four threads, `28597532274c3250`;
  - `01c52d0`, the plan without a second `lstat`, `d45bbdd6c22ca06f`;
  - `244114f`, the fixes a review asked for, `064946d717041f75`;
  - `5330bca`, swaps on Rayon, `5b392d972fee9e0d`;
  - `6e84ce0`, removal on Rayon, `8323b3c3f4024807`.

## Results

### Where the base spends its time

The main thread of `6d15ca2` in one profiled full-folder run, in [`split-6d15ca2.txt`](split-6d15ca2.txt). Four decode threads wrote the staged files in about 1.08 s each before publication began.

| Stage | Main-thread time | Per file |
| --- | ---: | ---: |
| Swap each old file with its staged copy, `renameatx_np` | 2,716 ms | 97 µs |
| Remove the swapped-out old files when `TempDir` drops, `unlinkat` | 1,334 ms | 48 µs |
| `lstat` in the swap, three per file | 496 ms | 18 µs |
| Find the folders the merge created, `Path::starts_with` for 309 folders by 28,000 steps | 306 ms | 11 µs |
| `lstat` in the plan, two per file | 109 ms | 4 µs |

No set-aside copy is made, since APFS swaps names. Every stage but the decode is serial on the calling thread. The empty-folder run moves the top folder with one rename.

### What single operations cost

[`operations.py`](operations.py), one run, in [`operations.txt`](operations.txt). [`removal_threads.py`](removal_threads.py), three runs per row, in [`removal-threads.txt`](removal-threads.txt).

| Operation on 28,000 files in 280 folders | Time |
| --- | ---: |
| Swap each file with its copy in another tree | 2.57 s |
| Rename each file over its copy | 2.86 s |
| Swap the 280 folders | 0.047 s |
| Swap the two top folders | under 1 ms |
| `lstat` each file | 0.059 s |
| Unlink each file, 1 thread | 0.99 to 1.05 s |
| Unlink each file, 2 threads | 0.71 to 0.74 s |
| Unlink each file, 4 threads | 0.52 to 0.55 s |
| Unlink each file, 5 threads | 0.65 to 0.66 s |
| Unlink each file, 8 threads | 0.87 to 0.88 s |
| Unlink each file, 12 threads | 1.22 to 1.24 s |

A swap of folders costs nothing next to 28,000 swaps of files. Unlinks are fastest on four threads, and twelve threads are slower than one. Unlinking in inode order was no faster than in listing order.

### Each change, timed

Median wall time in seconds over 11 interleaved runs, with the range, and median CPU time. Raw samples are in [`session-full.json`](session-full.json) and [`session-empty.json`](session-empty.json). Every binary's last tree matched the source.

| Binary | Full folder | Empty folder | Full over empty | CPU, full |
| --- | --- | --- | ---: | ---: |
| `6d15ca2`, base | 6.298 (6.244 to 6.492) | 1.213 (1.183 to 1.232) | 5.19 | 8.818 |
| `4c9496d`, folder set | 5.974 (5.925 to 6.096) | 1.209 (1.191 to 1.272) | 4.94 | 8.504 |
| `8adc362`, folder replaced whole | 2.810 (2.736 to 2.889) | 1.214 (1.193 to 1.258) | 2.31 | 5.750 |
| `cfc63dd`, removal on four threads | 2.168 (2.112 to 2.249) | 1.209 (1.191 to 1.258) | 1.79 | 6.807 |
| `01c52d0`, plan without a second `lstat` | 2.124 (2.086 to 2.410) | 1.201 (1.193 to 1.244) | 1.77 | 6.742 |
| `244114f`, review fixes | 2.157 (2.123 to 2.264) | 1.200 (1.178 to 1.235) | 1.80 | 6.796 |

The first three steps are apart from their neighbours' ranges. The last three overlap one another, so this session shows no difference among them.

The first session, 9 runs, timed the two changes that were reverted, on the commit before the base. Raw samples are in [`first-session-full.json`](first-session-full.json) and [`first-session-empty.json`](first-session-empty.json).

| Binary | Full folder | Empty folder | CPU, full |
| --- | --- | --- | ---: |
| `3255a0a` | 6.354 (6.272 to 6.565) | 1.225 (1.194 to 1.240) | 8.867 |
| `b6d635a`, folder set | 6.094 (5.987 to 6.282) | 1.215 (1.203 to 1.237) | 8.576 |
| `5330bca`, swaps on Rayon | 5.853 (5.799 to 5.924) | 1.207 (1.193 to 1.234) | 30.609 |
| `6e84ce0`, removal on Rayon | 5.890 (5.788 to 6.290) | 1.205 (1.195 to 1.223) | 36.889 |

A third session of 7 runs compared two and four removal threads, with builds from before the commits. The folder swap alone took 2.819 s (2.769 to 2.858), two threads 2.452 s (2.427 to 2.467) and four threads 2.186 s (2.115 to 2.258). CPU time was 5.761, 6.175 and 6.792 s. Raw samples are in [`threads-full.json`](threads-full.json) and [`threads-empty.json`](threads-empty.json).

### Where the kept code spends its time

One profiled full-folder run of `244114f`, in [`split-244114f.txt`](split-244114f.txt).

| Stage | Wall time | CPU time, all threads |
| --- | ---: | ---: |
| Decode and write the staged files, four threads | 1,081 ms | 4,269 ms |
| Remove the old files, four threads | 644 ms | 2,482 ms |
| Plan the merge, with one `lstat` and one access control list read per replaced file | 147 ms | 138 ms |
| Read the new folder, swap it and check the old one | 62 ms | 58 ms |

The removal is 0.64 s of the 0.96 s that the full folder costs over the empty one. Its four threads each stay busy for the whole of it, so they contend, and the single-operation table shows a fifth thread makes it slower.

### Peak memory

[`mem.py`](mem.py) ran each binary under `/usr/bin/time -l` into a folder that held every file, on an Apple M5 under the load of other builds. The head here is a build of the source of `244114f` from before the commit. Raw lines are in [`memory.txt`](memory.txt).

| Archive | Threads | Head | `b6d635a`, the folder set |
| --- | ---: | --- | --- |
| 100,000 empty files in one folder | 1 | 72.1 to 76.3 MB | 94.9 to 97.0 MB |
| 100,000 empty files in one folder | 8 | 75.4 to 75.8 MB | 95.0 to 98.6 MB |
| The fixture | 1 | 21.6 MB | 33.0 MB |
| The fixture | 8 | 24.2 MB | 36.5 MB |

Three runs per cell on the large archive, one on the fixture. `b6d635a` has a second, lower level on the large archive. One earlier run here gave 70.0 MB at 8 threads, and the reviewer's two runs at 1 thread gave 68.6 MB. Against that level the head is 4 to 8 MB higher. What puts `b6d635a` on one level or the other was not found.

## Decision

Kept, in this order:

1. `4c9496d` finds the folders a merge created through a set of moved paths. It took 0.32 s off the full-folder run, ranges apart.
2. `8adc362` replaces an existing folder in one step when the archive names every entry in it. It swaps the staged folder with the existing one, so the fixture needs one swap where it needed 28,000. The run fell from 5.97 to 2.81 s and CPU time from 8.50 to 5.75 s. The [usage page](../../../docs/usage.md#extract-into-an-existing-folder) states when a folder qualifies and what the new folder keeps. A run replaces folders only while it holds an exclusive lock on the destination, as [concurrent extractions](../../../docs/reference/concurrent-overwrite.md) describes.
3. `cfc63dd` unlinks the old files on four threads after a merge that leaves 1,024 or more. The run fell from 2.81 to 2.17 s for 1.1 s more CPU time. Four is the count the single-operation table found fastest.
4. `01c52d0` reads a staged entry's kind from the folder listing. A profile showed 57 ms less in the plan. Its range overlaps the commit before it, so it is kept for the system calls it removes, not for a proven gain.

`244114f` is the head after a review. On macOS it reads the access control list of every replaced file, which a profile puts at 34 ms, because a folder swap would otherwise carry away a file whose list denies delete. Its range overlaps the two commits before it.

Reverted:

1. `5330bca` swapped runs of 16 or more replaced files on Rayon. Wall time fell by 4% and CPU time rose 3.6 times.
2. `6e84ce0` emptied the staging folder on Rayon at full width. Wall time did not change and CPU time rose again. Twelve threads unlink slower than one on this machine, which is why the earlier [parallel cleanup](../parallel-cleanup/README.md) failed too.

M29's exit check asks for 1.5 times, or a verdict that names what keeps the gap larger. The gap is 1.80 times, and the removal of the old files on APFS keeps it there. The old files must stay until the last cancel check, so their removal cannot overlap the decode, and nothing else is left for it to overlap. At 0.64 s on four threads it alone puts the full folder at 1.54 times the empty one. The exit check is met on its second arm, and no further gain is sought.

## Limits

- One machine, one day, APFS, one archive shape. Every folder of the fixture qualifies for the swap. A folder that holds one file the archive does not name is merged file by file at the old cost, about 150 µs per replaced file here.
- Linux was not measured. The swap and the lock are built for it and its CI tests run them, but no timing shows what they gain there.
- Finder, Spotlight and `fseventsd` were busy during every run, so absolute times are likely higher than on an unwatched folder. Each binary met the same load, in interleaved order.
- The single-operation tables come from Python loops, one run or three. They chose the thread count, and the timed sessions then confirmed four threads against one and two only.
- The profile is one run per binary.
- Peak memory was measured on another machine, an Apple M5, under the load of other builds.
- The folder swap gives an existing folder a new inode. A program whose working folder is the old one loses the files it creates there afterwards, as the usage page says.

## Reproduce

1. Write the fixture with `python3 benchmarks/overwrite/full-folder/make_archive.py . OUT`. It gives `OUT/tree` and `OUT/small-files.zip`.
2. Build a release binary at each commit and name each `bins/zipper-COMMIT`. From main, use `61038e0`, `5f5f266`, `9a1c336`, `4537885`, `e946e05` and `86066d1`, and pass those names to the commands below.
3. Put the scripts of this folder, `small-files.zip`, `source/tree` and `bins/` in `target/timing/NAME/` of a checkout, and run `zsh session.sh session 11 6d15ca2 4c9496d 8adc362 cfc63dd 01c52d0 244114f` there. It prints the summary and writes `session-full.json` and `session-empty.json`.
4. Profile with `SAMPLY=/path/to/samply zsh profile.sh 244114f`.
5. Time the single operations with `python3 -I operations.py WORK` and `python3 -I removal_threads.py WORK`, where `WORK` is a scratch folder the scripts create and delete.
6. Measure peak memory with `python3 -I mem.py ARCHIVE WORK 8 head=BINARY base=BINARY`.
