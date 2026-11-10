# M4. Small-file output path

Extraction of many small files gets faster at matched threads and CPU, starting on macOS.

Scope:

- Trace macOS extraction outside timed runs and name the limiter before changing code.
- Restate the 3× target against default bsdtar at matched parity before the first experiment. Done in [small-file creation cost on macOS](../../benchmarks/native-zip/create-cost/README.md). On this Mac the system-call floor allows at most about 1.33× at one thread and 2.3× at four threads, so 3× is unreachable at M2 parity.
- Test one change at a time, following the design gates. Replicate on Linux and Windows after macOS.

Exit checks:

1. A kept change beats the spread on the M3 small-file workloads at matched threads, with CPU time reported.
2. Linux and Windows show no regression on the same workloads.
