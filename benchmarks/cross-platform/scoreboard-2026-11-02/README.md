# Scoreboard on Linux and Windows runners

**On hosted Linux and Windows runners, native at one thread beats default bsdtar on every scoreboard workload, by 1.21× to 2.08×.** At four threads, Linux wins on every workload, by 1.55× to 4.38×. Windows loses on 10,000 flat tiny files (0.82×), because parallel file creation there is slower than serial creation.

## Question

M5 sped up macOS. Do Linux and Windows show the same lead over bsdtar, where is each platform's file-creation floor, and how much headroom is left?

## Setup

- GitHub Actions run 41386011519, workflow `.github/workflows/scoreboard.yml`, at `fedeec5`.
- `ubuntu-24.04` reports `Linux-7.0.0-1017-azure-x86_64-with-glibc2.39`. `windows-2022` reports `Windows-2022Server-10.0.20348-SP0`. Each runner has 4 logical CPUs.
- `cargo xtask bench` ran the seven fixed workloads, seven measured rounds after one warmup, every extraction byte-verified. bsdtar on Linux comes from `libarchive-tools`; on Windows it is `C:/Windows/System32/tar.exe`. 7-Zip comes from the `7zip` package and `C:/Program Files/7-Zip/7z.exe`.
- `scripts/probe_create_cost.py` measured file creation on the same runner, with seven rounds. On Windows it sets times by path, which reopens each file, so its full-sequence floor is high.

## Results

Medians in milliseconds, with the ratio to default bsdtar on the same ZIP. Raw JSON is in `ubuntu-24.04/` and `windows-2022/`.

| Workload | Linux native, 1 thread | Linux native, 4 threads | Linux bsdtar | Windows native, 1 thread | Windows native, 4 threads | Windows bsdtar |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Dependency sources | 283.5 (1.58×) | 132.0 (3.39×) | 447.4 | 617.3 (1.39×) | 398.1 (2.15×) | 857.1 |
| 10,000 flat tiny files | 302.9 (1.36×) | 235.8 (1.74×) | 410.6 | 483.4 (1.51×) | 892.4 (0.82×) | 728.8 |
| 4,000 grouped files | 104.8 (1.63×) | 41.0 (4.15×) | 170.3 | 239.2 (1.56×) | 270.9 (1.37×) | 372.1 |
| 256 MiB compressible | 162.1 (1.21×) | 126.8 (1.55×) | 196.1 | 192.5 (1.66×) | 137.5 (2.32×) | 318.7 |
| 256 MiB random | 92.4 (1.90×) | 56.6 (3.10×) | 175.5 | 139.9 (2.08×) | 98.8 (2.95×) | 291.3 |
| Mixed tree | 131.9 (1.71×) | 51.5 (4.38×) | 225.5 | 284.4 (1.62×) | 258.3 (1.78×) | 460.7 |
| Silesia | 376.1 (1.55×) | 161.8 (3.61×) | 583.7 | 543.5 (1.59×) | 219.7 (3.93×) | 862.6 |

File creation, median microseconds per file:

| Variant | Linux wall | Linux CPU | Windows wall | Windows CPU |
| --- | ---: | ---: | ---: | ---: |
| Create, close, 1 thread | 21.2 | 21.2 | 26.4 | 26.6 |
| Create, write, set time, close, 1 thread | 27.2 | 27.2 | 56.3 | 56.2 |
| Same, 2 threads | 17.3 | 34.4 | 90.3 | 106.2 |
| Same, 4 threads | 22.6 | 88.9 | 107.8 | 165.6 |
| Create, close, 4 threads, one shared directory | 22.2 | 87.8 | 60.2 | 56.2 |
| Create, close, 4 threads, one directory per thread | 9.3 | 33.0 | 28.0 | 71.9 |

## Decision

No code change in this record. It sets two experiments:

1. Windows needs a small-file worker cap like the macOS one. Four threads made flat tiny files 1.85× slower than one thread, and the probe shows parallel creation slower than serial there.
2. Linux has decoder headroom. `native-fast`, which decodes with libdeflate, beat `native` on dependency sources (102.6 against 132.0 ms) and Silesia (141.4 against 161.8 ms) at four threads.

On Linux, creation contends inside one directory but scales across directories, which explains why grouped files scale and flat files do not. At four threads, flat tiny files ran 4% above the shared-directory floor.

## Limits

One run of each hosted runner. These are shared virtual machines with uncontrolled neighbours, Windows Defender enabled, and no drift session yet, so treat differences under about 10% as noise. The Windows probe reopens each file to set its time, so its full-sequence floor overstates what Zipper pays; native at one thread ran below it. The scoreboard has no row for the default thread choice, so this record does not measure the default directly.

## Reproduce

```sh
gh workflow run scoreboard.yml -f runs=7
gh run download <run id>
```
