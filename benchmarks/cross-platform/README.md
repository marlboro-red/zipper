# Cross-platform Silesia checks — 2026-10-29

[GitHub Actions run](https://github.com/marlboro-red/zipper/actions/runs/40815032350), source `60439c3`. This is the **static ZIP scheduling baseline**, before `afb77d0` introduced work stealing. Each native configuration requests two codec workers; compatibility extraction uses each installed 7-Zip backend default. Seven measured shuffled rounds plus warmup; all restored bytes and tree entries verified.

Hosted runners are shared virtual infrastructure, not controlled performance laboratories. Compare paths within one runner/session. Do not treat absolute cross-host differences as processor rankings or latency guarantees.

| Runner | CPU | 7-Zip version | Native ZIP MiB/s | Direct 7-Zip MiB/s | Native range ms | Direct range ms |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| [macos-arm64](60439c3/macos-arm64/environment.txt) | Apple M1 virtual, 3 vCPUs | 26.03 | 610 | 202 | 307.8–416.3 | 889.1–1195.8 |
| [linux-x86_64](60439c3/linux-x86_64/environment.txt) | AMD EPYC 9V45, 4 vCPUs | 23.01 | 693 | 256 | 287.9–363.0 | 767.2–815.4 |

## Native format on the same hosts

| Runner | Level / block KiB | Archive bytes | Compression MiB/s | Extraction MiB/s |
| --- | --- | ---: | ---: | ---: |
| macos-arm64 | level1-block1024 | 73,602,933 | 786 | 1733 |
| macos-arm64 | level1-block4096 | 73,338,765 | 761 | 1472 |
| macos-arm64 | level6-block1024 | 63,138,699 | 185 | 1738 |
| macos-arm64 | level6-block4096 | 61,779,861 | 186 | 1714 |
| linux-x86_64 | level1-block1024 | 73,602,933 | 444 | 2614 |
| linux-x86_64 | level1-block4096 | 73,338,765 | 439 | 2316 |
| linux-x86_64 | level6-block1024 | 63,138,699 | 200 | 2564 |
| linux-x86_64 | level6-block4096 | 61,779,861 | 194 | 2191 |

Raw `sweep.json`, `formats.json`, corpus hashes and environment details are stored beside each runner link. ZIP tests use the same archive for native and direct-backend extraction within each format/round. Backend versions differ across operating systems. Native ZIP preserves bytes/directories, while 7-Zip preserves more metadata; this is not complete feature parity.

The local Apple M5 [Pareto curve](../silesia/README.md) covers more levels, block sizes and reader choices. Cross-platform release correctness is separately exercised by `.github/workflows/ci.yml`; performance results are not used as noisy CI pass/fail thresholds. Re-run with the manually dispatched `.github/workflows/benchmark.yml` workflow.
