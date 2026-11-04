# Relative file creation on Windows

**Opt-in behind the `relative-create` feature, because this machine shows no gain.** Creating each file relative to an open folder handle changed nothing within the ranges with Defender protection off, on with the folder excluded, or scanning every file. With protection off it was 6% to 9% slower on one workload. The gain that motivated it was reported on a different machine and is not reproduced here.

## Question

Microsoft Defender's file system filter asks for the normalized name of every file a process creates. A trace on an Intel Core Ultra 9 285H, writing to a ReFS Dev Drive with a Defender process exclusion, attributed about 3.4 s of CPU to that callback across 25,765 files. On that machine, `native` extraction of a 2.36 GB build archive went from 6.57 s to 5.38 s when each file was created with `NtCreateFile` relative to a handle on its folder. CPU fell 13% to 16% on both engines. Those numbers are an outside report, and their raw samples are not part of this record.

Does the same change help on a second machine? Which Defender settings decide whether it does?

## Setup

AMD Ryzen 7 5700X3D, 8 cores and 16 threads, 64 GiB, Windows 11 Pro 10.0.22631. Archives and output were on an NTFS NVMe volume, not a Dev Drive. Release binaries came from rustc 1.99.1.

Each comparison ran this change against [one entry per Rayon task](../one-entry-per-task/README.md) alone, at four threads, interleaved, with a byte-for-byte check of every tree. Archives come from [`make_target_zip.py`](../one-entry-per-task/make_target_zip.py).

| Defender setting | Workloads |
| --- | --- |
| Real-time protection off | Switchboard `target/release`, 16,029 files, and posh-tabcomplete `target`, 4,708 files, 1.18 GiB |
| On, benchmark folder excluded | Switchboard, and ripgrep 14.1.1 `target`, 1,700 files |
| On, output folder scanned | ripgrep |

The Defender policy was changed by an administrator between sessions. Protection state was read before and after every run and never changed during one.

## Results

Median wall milliseconds with the full range and median CPU seconds. The baseline is one entry per task. Raw samples are in the JSON files beside this page. The labels `one` and `pr1` name the baseline, and `rel` and `pr2` name this change.

| Defender | Workload | Engine | Baseline ms [range] | Relative ms [range] | CPU s | Ratio |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Off | Switchboard | `native` | 4567 [4469–4687] | 4594 [4299–4955] | 2.97 → 3.33 | 0.99× |
| Off | Switchboard | `native-fast` | 4679 [4334–4872] | 4635 [4478–4890] | 3.53 → 3.81 | 1.01× |
| Off | posh-tabcomplete | `native` | 1263 [1250–1290] | 1344 [1298–2646] | 0.69 → 0.75 | 0.94× |
| Off | posh-tabcomplete | `native-fast` | 1254 [1245–1334] | 1380 [1310–1505] | 0.69 → 0.72 | 0.91× |
| Excluded | Switchboard | `native` | 4792 [4575–4876] | 4870 [4655–6015] | 3.30 → 3.58 | 0.98× |
| Excluded | Switchboard | `native-fast` | 4701 [4547–12432] | 4706 [4482–4876] | 3.27 → 3.73 | 1.00× |
| Excluded | ripgrep | `native` | 444 [434–760] | 459 [445–1082] | 0.41 → 0.30 | 0.97× |
| Excluded | ripgrep | `native-fast` | 480 [465–529] | 481 [475–521] | 0.44 → 0.41 | 1.00× |
| Scanned | ripgrep | `native` | 931 [908–961] | 925 [902–934] | 1.77 → 1.73 | 1.01× |
| Scanned | ripgrep | `native-fast` | 922 [872–964] | 1023 [907–1752] | 1.72 → 1.69 | 0.90× |

Only the posh-tabcomplete `native` row is outside the ranges. All seven of its runs were slower than the baseline's slowest. Every other row overlaps.

## Decision

Kept, but off by default. Build with `--features relative-create` to use it. On this machine the folder handles cost more than the name lookups they save. The Defender filter is the limiter only on the machine in the outside report, which differs in CPU, in file system and in the kind of Defender exclusion.

The gate's Windows lint and the Windows release workflow now build and test the feature, so it cannot rot unbuilt. The TLA+ checks for progress reports and the ZIP decoder budget passed unchanged, and their fingerprints are refreshed.

## Limits

One machine. No ReFS Dev Drive and no Defender process exclusion were tested, which are the two settings of the outside report. The outside numbers have no raw samples here. Every run except the folder-excluded ripgrep run used binaries without the task pool of the baseline change, on both sides of each comparison. CI was not run.

## Reproduce

Build `zipper-before.exe` from the one-entry-per-task commit and `zipper-after.exe` from this commit with `cargo build --release --features relative-create`. Then compare them as in [one entry per Rayon task](../one-entry-per-task/README.md), with Defender in the setting under test.

```powershell
python scripts/compare_zip_readers.py ripgrep-target.zip ripgrep/target --runs 11 --threads 4 `
  --reader before native zipper-before.exe --reader after native zipper-after.exe
```
