# Small-file creation cost on macOS

**On this Mac, native extraction of 10,000 tiny files takes 5% to 6.2% longer than its system calls alone.** A 3× win over default bsdtar is below that floor at every thread count. M4 cannot reach it on macOS without restoring less.

## Question

What limits extraction of many small files on macOS, and can Zipper reach 3× the speed of default bsdtar at the same metadata parity?

## Setup

Apple M5, 10 logical CPUs, macOS 26.7.1. The probe ran in `target/bench-work/probe` on the internal APFS volume, with warm caches.

`scripts/probe_create_cost.py` runs no Zipper code. Each variant creates 10,000 files with `O_CREAT|O_EXCL`, 512 bytes in four files of five, and optionally sets a modification time with `futimens`. It runs 7 interleaved, shuffled rounds after one warmup and records wall and CPU time per file. Threads split the files evenly. Python releases its lock inside each system call.

The Zipper numbers come from the M3 scoreboard in [Results](../../../docs/results.md), session 2 at `8f4dfe8`, which measured the same binary as `77564d9`. Its counters show one create, at most one write and one time call per flat-tiny file, and decoding under 1% of the time.

A one-thread `/usr/bin/time` run of native extraction on the flat-tiny ZIP spent 0.41 s in the kernel and 0.02 s in user code, out of 0.45 s.

## Results

Medians of 7 rounds, with the wall range. Raw samples are in [`macos.json`](macos.json).

| Variant | Threads | Wall µs per file | CPU µs per file |
| --- | ---: | ---: | ---: |
| Create, close | 1 | 30.9 [30.2–32.8] | 30.9 |
| Create, write, close | 1 | 42.3 [41.3–43.7] | 40.4 |
| Create, write, set time, close | 1 | 49.8 [48.1–53.8] | 48.2 |
| Create, write, set time, close | 2 | 34.3 [33.5–35.3] | 65.5 |
| Create, write, set time, close | 4 | 28.7 [28.4–30.3] | 103.6 |
| Create, write, set time, close | 8 | 41.5 [41.0–46.1] | 247.7 |
| Create, close, one shared directory | 4 | 18.2 [18.1–20.1] | 71.8 |
| Create, close, one directory per thread | 4 | 18.0 [17.6–18.6] | 70.4 |

Comparison with the flat-tiny scoreboard rows, 10,000 files:

| Row | Wall ms | System-call floor ms | Above the floor |
| --- | ---: | ---: | ---: |
| `native-1` | 523.1 | 497.9 | 5.0% |
| `native-4` | 304.8 | 286.9 | 6.2% |
| `bsdtar-zip` | 661.7 | 497.9 | 32.9% |

The 3× target against default bsdtar is 220.6 ms. The lowest floor is 286.9 ms, at 4 threads.

## Decision

The limiter on macOS is the kernel cost of creating each file and restoring its metadata. Creation alone costs 31 µs per file. Parallel creation contends across the volume, not per directory, so CPU per file doubles at 4 threads and wall time worsens at 8.

The M4 target is restated. Under the metadata parity M2 set, Zipper can gain at most about 5% at one thread and 6.2% at four threads on this Mac. The 3× target is unreachable here without restoring less, which the goal forbids. Default bsdtar spends 33% above the same floor, which is where Zipper's lead of 1.27× at one thread and 2.17× at four comes from.

No code changes. The remaining small-file work moves to Linux and Windows, where the floor has not been measured.

## Limits

One Mac, one APFS volume and warm caches. Security software that hooks file creation would raise the floor, and none was checked. The probe calls the kernel from Python, which adds well under 1 µs per call, so the floor is slightly high and Zipper's margin above it slightly low. Zipper's percentages use the M3 session-2 medians, which have their own drift of about 2%. The probe writes 512 bytes where flat-tiny writes 32 to 4,096, so its write cost is an approximation. The floor may differ on other macOS versions, other volumes and other file sizes.

## Reproduce

```sh
mkdir -p target/bench-work/probe
python3 scripts/probe_create_cost.py --root target/bench-work/probe \
  --output benchmarks/native-zip/create-cost/macos.json
cargo xtask bench --only flat-tiny
```
