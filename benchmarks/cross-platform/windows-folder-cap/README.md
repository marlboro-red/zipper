# Windows worker cap for crowded folders

**Kept. The Windows default uses one worker only for a crowded folder.** A crowded folder holds at least 1,024 tiny files, and other archives use every worker. On the last hosted run, the default extracted 10,000 flat tiny files in 423.0 ms against 545.7 ms at four threads. It extracted grouped files as fast as four threads. Hosted Windows runners varied by nearly 2× between runs, so only rows within one run are compared.

## Question

The [first Linux and Windows scoreboard](../scoreboard-2026-11-02/README.md) showed four threads extracting 10,000 flat tiny files in 892.4 ms on Windows, against 483.4 ms at one thread. Which automatic worker count is right for small files on Windows?

## Setup

- GitHub Actions, workflow `.github/workflows/scoreboard.yml`, `windows-2022` and `ubuntu-24.04` runners with 4 logical CPUs, seven measured rounds after one warmup, every extraction byte-verified.
- The scoreboard gained a `native-auto` row, with no `--threads` flag, so it measures the default.
- Run 41390181010 at `5af9dc6` capped Windows at one worker for every archive of at least 1,024 files of at most 16 KiB.
- Run 41394196775 at `c2c47a3` applied that cap only when one folder holds at least 1,024 of those files.
- The flat workload keeps 10,000 files in one folder. The grouped workload spreads 4,000 files over 40 folders.

## Results

Windows medians in milliseconds with the wall range and median CPU milliseconds. Raw JSON for both platforms is in the two run folders.

| Run | Workload | `native-auto` | `native-1` | `native-4` | bsdtar |
| --- | --- | ---: | ---: | ---: | ---: |
| 41390181010, every tiny archive capped | Flat | 828.3 [803.3–877.7], CPU 812.5 | 811.4, CPU 812.5 | 1,046.1 [1,037.5–1,107.2], CPU 1,531.2 | 1,289.1 |
| 41390181010, every tiny archive capped | Grouped | 470.6 [459.7–485.7], CPU 437.5 | 476.4, CPU 437.5 | 312.9 [304.5–338.4], CPU 828.1 | 683.0 |
| 41394196775, crowded folders capped | Flat | 423.0 [386.7–460.7], CPU 406.2 | 423.3, CPU 390.6 | 545.7 [518.0–573.8], CPU 718.8 | 658.4 |
| 41394196775, crowded folders capped | Grouped | 167.1 [158.6–172.3], CPU 437.5 | 205.4, CPU 203.1 | 172.4 [168.7–211.9], CPU 437.5 | 291.9 |

The creation probe on both runs put four threads in one shared folder at about twice the cost per file of four threads in separate folders, 60.2 against 28.0 µs and 94.4 against 45.4 µs.

In run 41394196775, the default beat default bsdtar on every workload on both platforms, by 1.56× to 4.45× on Windows and 1.40× to 3.73× on Linux. Linux has no cap, and its `native-auto` rows match `native-4`.

## Decision

Kept the crowded-folder rule. Capping every tiny-file archive made grouped files slower than four threads, so that first version was replaced. With the rule, the flat default stays at the one-thread time and beats four threads with ranges apart, and the grouped default matches four threads.

## Limits

Shared hosted runners with Windows Defender enabled. The two Windows runs differed by nearly 2× in absolute times, so the evidence is the ordering of rows within each run, not the numbers across runs. One run per rule. The 1,024-file threshold comes from the existing macOS cap, not from a sweep. Physical Windows machines were not measured.

## Reproduce

```sh
gh workflow run scoreboard.yml -f runs=7
gh run download <run id>
```
