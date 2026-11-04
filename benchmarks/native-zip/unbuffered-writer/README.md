# Unbuffered writes for large entries on Windows

**Writing whole native-fast entries of 1 MiB and up with `FILE_FLAG_NO_BUFFERING`, on one writer thread, made the Switchboard build folder extract 1.13× faster at four threads with 8% less CPU.** On the ripgrep build folder it cut CPU by 30%, but wall time was 1.09× faster in one session and 0.94× in another. Its runs split between a fast and a slow mode that was not traced. Windows and one machine only.

## Question

`native-fast` decodes an entry whole into memory, then writes it through the system file cache. The cache copies every byte once more, and the lazy writer spends CPU flushing it to disk. Writing large entries unbuffered skips that copy.

An unbuffered write needs a sector-aligned address, offset and length. The decoded entry goes into a 4 KiB-aligned buffer, padded with zeros to a whole number of sectors. One writer thread reopens the file with `FILE_FLAG_NO_BUFFERING`, writes the padded buffer, and trims the file to its size. Any failure of the unbuffered path falls back to buffered writes of the same bytes, for example on a read-only file or a larger sector size. The entry keeps its memory reservation until the write ends, so queued entries stay inside the shared entry-buffer budget.

Does this lower wall time and CPU on build folders, where most bytes sit in a few large files?

## Setup

The same machine, workloads and archives as [one entry per Rayon task](../one-entry-per-task/README.md). Defender real-time protection was on, with the benchmark folder excluded. Both readers ran `native-fast` at four threads, interleaved, with a byte-for-byte check of every tree. The baseline is the one-entry-per-task commit. The first two sessions ran on `f7189d5`, and the last ran after rebasing onto `c1fa6a1`.

## Results

Median wall milliseconds with the full range, median CPU seconds and peak memory. Raw samples are in [`ripgrep.json`](ripgrep.json), [`switchboard.json`](switchboard.json) and [`ripgrep-c1fa6a1.json`](ripgrep-c1fa6a1.json).

| Workload | Rounds | Baseline ms [range] | Unbuffered ms [range] | CPU s | Speedup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Switchboard `target/release` | 9 | 4869 [4617–10897] | 4301 [4057–9955] | 3.30 → 3.05 | 1.13× |
| ripgrep `target` | 21 | 477 [464–981] | 437 [421–1399] | 0.44 → 0.31 | 1.09× |
| ripgrep `target`, on `c1fa6a1` | 11 | 481 [455–677] | 510 [425–1147] | 0.47 → 0.31 | 0.94× |

Peak memory did not change. Every range overlaps because of runs at 2× the median on both sides. Counted per run, 6 of 9 Switchboard runs beat the fastest baseline run. On ripgrep, 14 of 21 runs did in the first session and 2 of 11 in the second. The rest sat near 550 ms, slower than most baseline runs. The slow mode did not follow any particular reader in the round order.

Entries under 1 MiB take the old path. On `flat-tiny` the [one-entry-per-task record](../one-entry-per-task/README.md) measured an earlier revision of this change at 0.99× of its baseline, with overlapping ranges.

## Decision

Inconclusive, proposed as a draft. The CPU saving holds in every session, and fewer copies through the cache are its likely source, but no profile confirms it. Wall time improves on the larger folder and is unsettled on the smaller one, where the slow mode decides the median.

The writer is a scoped OS thread outside the Rayon pool, like the write-behind writer. Its queue is unbounded, so workers never block on it, and memory stays bounded by the entry reservations. A guard sends the stop signal on every exit, including a pool build error or a panic. A writer failure stops the workers at their next entry or streamed chunk. The writer checks cancellation before each file.

The TLA+ checks for progress reports and the ZIP decoder budget passed unchanged, and the writer helpers are now in their coverage lists. Neither model includes the new writer thread.

## Limits

One machine, one NTFS NVMe volume, Windows only. No profile was taken. The slow mode was not traced. The writer does not stop partway through a file on cancellation, so one entry, bounded by the entry-buffer budget, can delay it. CI was not run. On Windows, `reported_bytes_never_run_ahead_of_bytes_on_disk` in `tests/control.rs` fails at the baseline and with this change alike.

## Reproduce

Build `zipper-before.exe` from the one-entry-per-task commit and `zipper-after.exe` from this commit. Make the archives as in [one entry per Rayon task](../one-entry-per-task/README.md).

```powershell
python scripts/compare_zip_readers.py ripgrep-target.zip ripgrep/target --runs 21 --threads 4 `
  --reader before native-fast zipper-before.exe --reader after native-fast zipper-after.exe
```
