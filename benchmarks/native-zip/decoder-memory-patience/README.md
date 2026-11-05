# Patience for ZIP decoder memory

**Kept a one-second wait for decoder memory. A PPMd archive extracts in a median of 9.50 s, against 9.36 s at `80af2b5`, with overlapping ranges.** Without the wait, it took 14.33 s against 10.82 s in the review's session. Four rounds per binary, one Mac.

## Question

A ZIP worker used to wait for decoder memory without a limit, which a progress callback that runs Rayon work could turn into a hang. The fix records an entry for another pass when its memory is short. Without any wait, the 2 threads beyond the 8 that fit in the budget recorded entries in microseconds, and each pass then waited for its slowest entry. How long should a worker wait before it records an entry, so that the hang stays impossible and PPMd archives keep their speed? The metric is the wall median of the default CLI against `80af2b5`.

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1, load average 1.39 at the start. Release builds.
- The archive is 175 MB. It holds 300 PPMd files of 2 MB and one PPMd file of 64 MiB, written by `7zz -mm=PPMd`. Its SHA-256 prefix is `47096cce4bff68b8`. Each PPMd entry takes 256 MiB of the 2 GiB budget, so 8 fit at once.
- `head` is `80af2b5` with the working tree's `src/lzma.rs`, binary prefix `03559fb16fc7`. `100ms` and `1000ms` are this change with `DECODER_MEMORY_PATIENCE` at 100 ms and 1 s, prefixes `f17e46730fbb` and `f31f9d122fee`.
- `zipper extract -o` with the default engine ran 4 interleaved rounds of the three binaries. A script compared every file's CRC and size with the archive and looked for extra files after each run.

## Results

Wall seconds. Raw samples are in [`samples.txt`](samples.txt).

| Binary | Rounds | Median | Range |
| --- | ---: | ---: | --- |
| `head` | 4 | 9.36 | 9.01 to 9.74 |
| `100ms` | 4 | 9.96 | 9.76 to 10.71 |
| `1000ms` | 4 | 9.50 | 9.32 to 10.50 |

Every run passed the byte check. The adversarial review measured the version without any wait on the same archive in another session. It took 14.33 s, range 12.87 to 15.07, against 10.82 s, range 9.26 to 12.00, for `head`.

## Decision

Kept 1 s. Its range overlaps `head`, so it has no established cost. The 100 ms wait still sits above `head` in every round. A bounded wait cannot deadlock, because a worker that gives up records its entry and returns, as the TLA+ models in `verification/progress-callback/` and `verification/zip-decoder-budget/` check. A callback caught in the old hang now waits up to 1 s per entry instead of forever.

## Limits

One Mac, one archive, four rounds per binary. The rounds overlap, so the result is "no measured cost", not "as fast". The no-wait numbers come from a different session. Machines with more threads record more entries per pass and were not measured. XZ archives with mixed sizes showed the same pass pattern in the review, with timings that overlapped `head`.

## Reproduce

```sh
zipper extract -o out ppmd_bal.zip
```

Build `80af2b5` and this change with `DECODER_MEMORY_PATIENCE` at 100 ms and 1 s. Run each binary four times in rotation on the archive above, and check every file's CRC against the archive.
