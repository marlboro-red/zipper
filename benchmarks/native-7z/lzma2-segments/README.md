# Parallel LZMA2 segments in solid 7z folders

**Kept. Decoding a solid LZMA2 folder's segments on separate threads makes solid Silesia extract 1.59× faster at default threads, 1,307 ms to 824 ms, with ranges apart.** It still takes 1.24× as long as 7-Zip, and peak memory rises from 38 MiB to 338 MiB. Non-solid archives and the solid dependency sources, whose stream has no second segment, are unchanged.

## Question

The native 7z reader decoded each folder on one thread. A solid archive has one folder, so solid Silesia took 1.85× as long as 7-Zip in a first probe, while non-solid archives were faster than 7-Zip. 7-Zip compresses LZMA2 in blocks, and each block starts by resetting the dictionary, so it decodes the blocks at once. Does splitting the stream at those resets, read from chunk headers without decoding, close the gap? The metric is the wall median against the current reader on the same archive, with 7-Zip as the reference.

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1. `before` is the reader at `9d40913` plus `fb51bd7`, SHA-256 prefix `599ec3095dc8`. `after` adds the change with the review fixes below, prefix `3c533574e848`. 7-Zip is `7zz` 26.05, prefix `60e4afd68d86`.
- The archives were written by `7zz a` with default settings, LZMA2 at level 5, over the scoreboard's Silesia and dependency-source trees, once solid and once with `-ms=off`. Solid Silesia has two segments, of 128 MiB and 74 MiB of output. The solid dependency sources have one.
- `scripts/compare_zip_readers.py --archive-format 7z` ran each reader at its default thread count, for 7 shuffled rounds after one warmup, extracting under `target/bench-work`. Every round passed the byte and tree check.
- The design was checked first as a TLA+ model in `verification/7z-lzma2-segments/`. The model holds output order, completeness, reported failures, the memory budget, the thread window and no leak after an early drop.

## Results

Wall milliseconds as median [range], median CPU time and peak memory. Raw samples are in `raw/`.

| Archive | Bytes | before | after | 7-Zip | before ÷ after | 7-Zip ÷ after |
| --- | ---: | --- | --- | --- | ---: | ---: |
| Silesia, solid | 49,597,394 | 1,307 [1,304–1,316] · 1.31 s CPU · 38 MiB | 824 [822–832] · 1.33 s CPU · 338 MiB | 667 [667–675] · 1.07 s CPU · 256 MiB | 1.59× | 0.81× |
| Silesia, non-solid | 49,608,000 | 425 [419–431] · 1.61 s CPU · 217 MiB | 425 [420–428] · 1.61 s CPU · 216 MiB | 1,047 [1,045–1,055] · 1.04 s CPU · 76 MiB | 1.00× | 2.46× |
| Dependency sources, solid | 6,156,722 | 453 [446–465] · 0.54 s CPU · 42 MiB | 459 [445–485] · 0.54 s CPU · 42 MiB | 452 [446–476] · 0.45 s CPU · 75 MiB | 0.99× | 0.98× |
| Dependency sources, non-solid | 12,169,886 | 317 [307–332] · 1.83 s CPU · 69 MiB | 318 [308–322] · 1.83 s CPU · 71 MiB | 616 [597–625] · 0.60 s CPU · 7 MiB | 1.00× | 1.94× |

## Decision

Kept. On solid Silesia the after range lies wholly below the before range, CPU time is unchanged, and the other three rows move by less than their ranges.

The limiter is now the larger segment. Sequential decoding of both segments took 1,311 ms, and the larger segment holds 63% of the output, which predicts about 830 ms for decoding it alone. The measured 824 ms matches. Closing the rest of the gap needs either more segments, which only the writer controls, or a faster LZMA decoder. 7-Zip spends 1.07 s of CPU against our 1.34 s on the same work.

The extra memory is the decoded segments held until their turn, their packed bytes and each decoder's dictionary. All three count against a 512 MiB budget per folder, which the folder reserves from the reader's shared 2 GiB decoder budget. A segment that would not fit, or any chunk layout the scan does not expect, falls back to sequential decoding.

An adversarial review of the first version found four resource problems, each fixed and retested on its archive. A stream of 8 million one-byte segments took 83 s, and now takes 0.52 s, because segments merge until each holds at least 8 MiB. The scan's list reached 1.5 GiB, and now stays at 6 MiB peak. One folder peaked at 1 GiB and eight at 5 GiB against the stated budgets; they now peak at 514 MiB and 1.3 GiB. A refused thread spawn panicked, and now fails or degrades, with the window capped at 64. Decoders also stop early once the reader is dropped. The review's 718 differential runs found no input where parallel and sequential decoding disagreed.

## Limits

One Mac, one writer. The final run started with a load average of 166 from a thread-limit test seconds before, decaying to 40, and Firefox used about 40% of one core. Its medians agree within 1% with an earlier run at a load of 1.8, whose `after` binary lacked the review fixes. Archives from other writers can reset the dictionary at other points or never. Only 7-Zip's level 5 was measured, which sets the block size. With one folder, its segment decoders get the whole thread count, and the folder's own worker waits on them, so one thread more than `--threads` exists. Linux and Windows were not measured. The decoder CPU comparison with 7-Zip was not profiled.

## Reproduce

```sh
cd target/bench-corpora/silesia && 7zz a ../../bench-work/7z-probe/silesia.7z silesia && cd -
python3 scripts/compare_zip_readers.py target/bench-work/7z-probe/silesia.7z target/bench-corpora/silesia/silesia \
  --archive-format 7z --runs 7 --work-dir "$PWD/target/bench-work" \
  --reader before native BEFORE --reader after native AFTER --reader 7zip 7zip "$(command -v 7zz)" \
  --reader-threads before auto --reader-threads after auto --reader-threads 7zip auto
```
