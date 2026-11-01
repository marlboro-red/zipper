# Cross-platform Silesia checks on 2026-10-29

On hosted macOS and Linux runners, native ZIP extraction of Silesia was faster than direct 7-Zip within each runner, with separated ranges. This run is the static ZIP scheduling baseline, recorded before work stealing.

## Question

How do the native ZIP reader and the native format perform on hosted macOS arm64 and Linux x86_64 runners?

## Setup

The [GitHub Actions run](https://github.com/marlboro-red/zipper/actions/runs/40815032350) built source `60439c3`. This is the **static ZIP scheduling baseline**, before `afb77d0` introduced work stealing.

- Each native configuration requests two codec workers.
- Compatibility extraction uses the default of each installed 7-Zip backend.
- Each configuration ran seven measured rounds in shuffled order, plus a warmup.
- ZIP tests use the same archive for native and direct-backend extraction within each format and round.
- The run verified all restored bytes and tree entries.

Raw `sweep.json`, `formats.json`, corpus hashes, and environment details are stored beside each runner link.

## Results

### Native ZIP versus direct 7-Zip

| Runner | CPU | 7-Zip version | Native ZIP MiB/s | Direct 7-Zip MiB/s | Native range ms | Direct range ms |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| [macos-arm64](60439c3/macos-arm64/environment.txt) | Apple M1 virtual, 3 vCPUs | 26.03 | 610 | 202 | 307.8–416.3 | 889.1–1195.8 |
| [linux-x86_64](60439c3/linux-x86_64/environment.txt) | AMD EPYC 9V45, 4 vCPUs | 23.01 | 693 | 256 | 287.9–363.0 | 767.2–815.4 |

### Native format on the same hosts

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

## Decision

Performance results are not used as CI pass or fail thresholds because they are noisy. `.github/workflows/ci.yml` exercises cross-platform release correctness separately. The local Apple M5 [Pareto curve](../silesia/README.md) covers more levels, block sizes, and reader choices.

## Limits

- Hosted runners are shared virtual infrastructure, not controlled performance laboratories. Compare paths within one runner and session.
- Do not treat absolute differences across hosts as processor rankings or latency guarantees.
- Backend versions differ across operating systems.
- Native ZIP preserves bytes and directories, while 7-Zip preserves more metadata. The comparison does not show complete feature parity.

## Reproduce

Dispatch the `.github/workflows/benchmark.yml` workflow manually.
