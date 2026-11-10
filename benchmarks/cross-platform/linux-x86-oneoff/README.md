# Linux x86-64 rerun of the sevenz-rust2, PGO and Rust 1.99.1 timings

One hosted Linux run agrees with the three macOS records. Zipper's 7z extraction beat sevenz-rust2 by 1.12 to 1.20 times at one thread, PGO again helped only BZip2, and Rust 1.99.1 was 2.5% to 3.2% slower on gzip tar and ZIP Deflate at one thread. That last gap is from one run on a shared runner.

## Question

Do the results of the [sevenz-rust2](../../lzma/sevenz-rust2/README.md), [PGO](../../pgo/lzma-and-readers/README.md) and [Rust 1.99.1](../../toolchain/rust-1.99.1/README.md) records, all measured on one Apple M5, hold on x86-64 Linux?

## Setup

- One [GitHub Actions run](https://github.com/marlboro-red/zipper/actions/runs/42753147354) on `ubuntu-24.04`, on 10 November 2026. The runner had an AMD EPYC 9V74 with 4 vCPUs, as [`lscpu.txt`](lscpu.txt) shows. It is a shared hosted machine, so rows are compared only within this run.
- [`run.sh`](run.sh) did everything on that runner. It fetched Silesia and the dependency sources, built every archive with the Ubuntu `7zip` package, version 23.01 ([`7zip-version.txt`](7zip-version.txt)), and built every binary from `bd44b35`.
- The binaries and their SHA-256 hashes are in [`binaries.txt`](binaries.txt):
  - `plain`, the release build with Rust 1.96.0;
  - `pgo`, built by [`build.sh`](../../pgo/lzma-and-readers/build.sh) with a profile from the dependency sources, as in the PGO record;
  - `rust-1.99.1`, the release build with Rust 1.99.1;
  - the sevenz-rust2 0.24.2 extraction program of the sevenz-rust2 record;
  - `probe-asm` and `probe-safe`, the release test binary with lzma-rust2's `optimization` feature on and off.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 1` ran each workload after one warmup round, in shuffled order, and checked every extracted byte. It ran with `--allow-busy`, so a busy runner would record its load in the JSON and keep going. Every JSON lists no busy process.
- The Silesia archives are 47 to 65 MiB. The dependency sources are 8.2 MiB solid and 15.3 MiB non-solid.

## Results

Median wall seconds with the range. "Default" means Zipper's default thread count on 4 vCPUs.

Raw LZMA decode of the probe stream, 24 rounds per build:

| Build | Zipper ms | lzma-rust2 ms |
| --- | --- | --- |
| `probe-asm` | 416.2 (410.9 to 430.1) | 464.7 (462.0 to 478.5) |
| `probe-safe` | 417.0 (411.1 to 440.1) | 478.2 (472.6 to 495.8) |

Zipper took 0.90 times lzma-rust2's time with its assembly, against 0.79 on the M5. Without its assembly, lzma-rust2 was 2.9% slower here, against 0.8% on the M5.

7z extraction:

| Archive | Threads | Zipper | sevenz-rust2 | 7-Zip 23.01 |
| --- | --- | --- | --- | --- |
| Silesia LZMA2 solid | 1 | 1.812 (1.767 to 1.839) | 2.026 (2.003 to 2.065) | 2.228 (2.218 to 2.252) |
| Silesia LZMA2 solid | default | 1.119 (1.105 to 1.141) | 1.372 (1.365 to 1.377) | 1.467 (1.455 to 1.471) |
| Silesia LZMA solid | 1 | 1.799 (1.765 to 1.858) | 2.089 (2.037 to 2.141) | 2.264 (2.220 to 2.282) |
| Silesia LZMA solid | default | 1.851 (1.787 to 1.885) | 2.117 (2.078 to 2.176) | 2.281 (2.242 to 2.289) |
| Silesia non-solid | 1 | 1.870 (1.828 to 1.883) | 2.150 (2.097 to 2.193) | 2.253 (2.218 to 2.329) |
| Silesia non-solid | default | 0.709 (0.693 to 0.717) | 2.372 (2.331 to 2.408) | 2.282 (2.254 to 2.325) |
| Dependencies solid | 1 | 0.548 (0.544 to 0.562) | 0.657 (0.648 to 0.666) | 0.756 (0.750 to 0.767) |
| Dependencies solid | default | 0.542 (0.533 to 0.548) | 0.718 (0.713 to 0.724) | 0.769 (0.761 to 0.774) |
| Dependencies non-solid | 1 | 0.902 (0.900 to 0.905) | 1.041 (1.038 to 1.058) | 1.097 (1.094 to 1.099) |
| Dependencies non-solid | default | 0.355 (0.351 to 0.357) | 1.455 (1.447 to 1.463) | 1.099 (1.096 to 1.104) |

Peak memory on solid Silesia at default threads was 145 MiB for Zipper, 333 MiB for sevenz-rust2 and 256 MiB for 7-Zip.

Build settings, at one thread unless the row says default:

| Workload | `plain` | `pgo` | `rust-1.99.1` |
| --- | --- | --- | --- |
| 7z LZMA2 solid | 1.847 (1.793 to 1.914) | 1.883 (1.821 to 1.950) | 1.832 (1.818 to 1.891) |
| 7z LZMA solid | 1.904 (1.885 to 1.932) | 1.943 (1.887 to 1.964) | 1.907 (1.894 to 1.964) |
| 7z non-solid, default | 0.720 (0.708 to 0.742) | 0.731 (0.719 to 0.734) | 0.717 (0.707 to 0.723) |
| tar xz | 1.915 (1.880 to 1.958) | 1.934 (1.919 to 1.968) | 1.902 (1.891 to 1.986) |
| tar lzip | 1.830 (1.767 to 1.872) | 1.851 (1.836 to 1.953) | 1.830 (1.786 to 1.886) |
| tar BZip2 | 4.828 (4.813 to 4.871) | 4.690 (4.682 to 4.776) | 4.861 (4.856 to 4.954) |
| tar gzip | 0.280 (0.279 to 0.283) | 0.282 (0.282 to 0.290) | 0.287 (0.287 to 0.288) |
| tar zstd | 0.228 (0.225 to 0.243) | 0.240 (0.225 to 0.260) | 0.238 (0.229 to 0.255) |
| ZIP Deflate | 0.310 (0.310 to 0.312) | 0.315 (0.313 to 0.315) | 0.320 (0.320 to 0.322) |
| ZIP Deflate, default | 0.128 (0.119 to 0.134) | 0.118 (0.115 to 0.127) | 0.127 (0.120 to 0.130) |
| ZIP LZMA | 1.828 (1.794 to 1.916) | 1.865 (1.805 to 1.928) | 1.789 (1.760 to 1.924) |
| ZIP zstd | 0.233 (0.231 to 0.253) | 0.233 (0.232 to 0.233) | 0.232 (0.232 to 0.233) |

The raw JSON is beside this page, one file per row group.

## Decision

No change. The three records keep their verdicts:

- sevenz-rust2. Zipper extracts every 7z workload faster than sevenz-rust2 on this runner too, with separated ranges.
- PGO stays reverted. BZip2 tar gained 2.9%, with separated ranges, and no LZMA workload gained.
- Rust 1.99.1 stays. Ten workloads overlap. gzip tar was 2.5% slower and one-thread ZIP Deflate 3.2% slower, each with separated ranges. Both use the Deflate decoder, so a Deflate check goes into the next Linux timing.

## Limits

- One run on one shared hosted runner. The repository has seen 11% drift between sessions on the same archive, and up to 2× between hosted Windows runs, so these ratios hold only within this run.
- 7-Zip is Ubuntu's 23.01 build, not the 26.05 used on macOS. Whether this build includes 7-Zip's assembly decoder was not checked, so its rows do not show how fast 7-Zip can be on x86-64.
- Zipper extracted Silesia faster than 7-Zip here and slower than 7-Zip 26.05 on the M5. That difference is one runner and one 7-Zip build, not a cross-platform claim.
- The `pgo` profile was trained on this runner, so it fits this CPU.

## Reproduce

1. Check out `bd44b35` and copy in [`run.sh`](run.sh) and the current [`build.sh`](../../pgo/lzma-and-readers/build.sh).
2. On `ubuntu-24.04`, install Rust 1.96.0 with the `llvm-tools` component, Rust 1.99.1, and the `7zip`, `lzip` and `zstd` packages.
3. Run `run.sh` from the repository root. It writes the JSON to `target/linux-oneoff/`.
