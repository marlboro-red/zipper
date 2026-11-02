# One tar writer per folder on Windows

**Kept on Windows. Sending all files of one folder to one writer makes 10,000 flat tiny files extract 1.44× to 1.56× faster from tar, with ranges apart.** Grouped files gain 1.19× to 1.24× and the mixed tree 1.15× to 1.27×. Of those, only the plain grouped row has separate ranges. Linux, where the change is off, stayed within 3% on every row.

## Question

On the [Linux and Windows tar rows](../tar-scoreboard/README.md), Windows took 691 ms to extract 10,000 flat tiny files from tar, against 516 ms for the ZIP reader. The ZIP reader uses one worker when one folder holds at least 1,024 tiny files, because [parallel creation in one Windows folder is slower than serial](../windows-folder-cap/README.md). A tar stream cannot be counted ahead. Does a fixed rule work instead, where each writer owns a lane and each folder hashes to one lane? The metric is the wall median, before and after, within one runner job.

## Setup

- GitHub Actions run 41457425407 of `.github/workflows/compare.yml`. `before` is `0d369f0`, and `after` is `c7defb8` on a branch. The change is active only on Windows, so Linux runs the same code twice as a control. After the measurement, review made the folder hash ignore case, so that `A/x` and `a/y` share a writer as they share an NTFS folder. It also skipped hashing when only one lane exists.
- `windows-2022` reports `Windows-2022Server-10.0.20348-SP0`, and `ubuntu-24.04` is the same image as in the earlier records. Each runner has 4 logical CPUs.
- `scripts/compare_workloads.py` wrote each scoreboard fixture once, as a ustar archive and its gzip level 6 form. It then ran both binaries with four writers, for 11 shuffled rounds after one warmup, extracting under `target/bench-work`. Every round passed the byte and tree check.
- An earlier run, 41451569203, extracted into the Windows temp folder. It took 3.4 to 6.1 s for flat tiny files, and every Windows range overlapped. That run also mixed in the zlib-rs change, so this record does not use it.

## Results

Wall medians in milliseconds with the range and the ratio of `before` to `after`. Raw samples are in `windows-2022/` and `ubuntu-24.04/`.

| Archive | Windows before | Windows after | Ratio | Linux before | Linux after | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 flat tiny files, gzip | 939.9 [839–1,046] | 603.0 [556–728] | 1.56× | 168.1 [164–173] | 167.5 [166–169] | 1.00× |
| 10,000 flat tiny files, plain | 924.1 [817–1,061] | 639.7 [567–715] | 1.44× | 164.9 [163–171] | 165.8 [163–168] | 0.99× |
| 4,000 grouped files, gzip | 370.9 [339–437] | 310.7 [289–393] | 1.19× | 51.7 [50–53] | 50.9 [50–53] | 1.02× |
| 4,000 grouped files, plain | 382.2 [357–435] | 307.8 [287–349] | 1.24× | 53.2 [51–55] | 52.5 [52–54] | 1.01× |
| Mixed tree, gzip | 421.6 [383–494] | 367.6 [311–442] | 1.15× | 80.2 [79–83] | 82.4 [81–83] | 0.97× |
| Mixed tree, plain | 437.3 [361–485] | 343.1 [305–420] | 1.27× | 64.4 [63–66] | 63.8 [61–68] | 1.01× |
| Dependency sources, gzip | 608.1 [567–654] | 552.9 [509–608] | 1.10× | 199.3 [194–203] | 200.9 [199–205] | 0.99× |
| Dependency sources, plain | 575.4 [548–679] | 524.2 [488–586] | 1.10× | 92.0 [85–97] | 94.0 [87–97] | 0.98× |

## Decision

Kept on Windows. Both flat rows and the plain grouped row moved beyond their ranges, and every other Windows median improved, with overlapping ranges. Linux, running identical code twice, stayed within 3% on every row with overlapping ranges, so the comparison shows no bias between the two binaries.

The rule needs no threshold. A crowded folder goes to one writer, which Windows prefers, and separate folders still spread across writers. A folder that holds most of a large archive does serialize on Windows, as the ZIP reader's crowded-folder cap also does.

## Limits

One run on a shared Windows runner with Windows Defender enabled. Absolute times differ from the scoreboard run, so only the ratios within this run carry weight. Folder hashing can put two busy folders on one writer. With four writers and 40 grouped folders that did not prevent a gain, but it was not measured separately. Physical Windows machines were not measured.

## Reproduce

```sh
gh workflow run compare.yml --ref <candidate branch> -f base=0d369f0 \
  -f workloads="flat-tiny grouped dependencies mixed" -f formats="tar tar.gz" -f runs=11
gh run download <run id>
```
