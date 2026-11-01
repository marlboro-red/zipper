# ZIP extraction versus bsdtar

On local macOS fixtures, native Zipper with four workers extracted identical ZIP bytes about twice as fast as single-process bsdtar on small files. On Silesia, native-fast was 1.99× as fast as tuned bsdtar at 174.2 MiB peak RSS, and the streaming `native` reader was 1.59× as fast at 4.9 MiB. Zipper's own format also extracted faster than tuned TAR pipelines on Silesia. These are wall-time comparisons at unequal worker counts.

## Question

How fast does Zipper extract compared with bsdtar? The comparison covers identical ZIP archives, uncompressed TAR archives of the same trees, and tuned TAR pipelines against Zipper's own format.

## Setup

Apple M5, 32 GiB RAM, macOS. The small-file runs record 10 logical CPUs, macOS 26.7.1 and release builds. Each comparison runs seven interleaved measured rounds after one warmup. Every extracted tree is compared directly byte for byte, with file sizes and tree membership checked.

Times include process startup and extraction writes. They exclude explicit extraction sync. Small-file times also exclude verification and cleanup, and bsdtar output-root creation occurs before its timer. Caches are warm.

The tar executable is `/usr/bin/tar`, which is bsdtar 3.5.3 with libarchive 3.7.4. The Zipper compatibility path is still the default and uses 7-Zip 26.05. Native Zipper uses automatic worker selection, which picks four active extraction workers on the small-file workloads.

Reduced-metadata bsdtar uses `-b 2048 -m --no-same-owner --no-same-permissions --no-acls --no-fflags --no-xattrs --no-mac-metadata`. That gives tar a comparison closer to the byte-and-directory-only native reader. Normal bsdtar restores more metadata.

### Fixtures

Silesia has 12 files and 211,938,580 input bytes. Its Deflate ZIP is 70,252,453 bytes.

The small-file fixtures were generated locally. The grouped tree has 4,000 files and 16,366,890 payload bytes. Its ZIP is 1,088,044 bytes and its TAR is 18,454,016 bytes. The original grouped fixture comes from `scripts/make_benchmark_corpus.py`.

The flat tree has 10,000 tiny files and 9,536,000 payload bytes. Its ZIP is 3,383,629 bytes and its TAR is 16,385,536 bytes. Its sizes cycle through 0, 32, 128, 512, and 4,096 bytes. It includes 2,000 empty files and both repetitive and deterministic pseudorandom data.

## Results

### Silesia

All readers extract the same 70,252,453-byte Deflate ZIP. Every extraction passed direct byte comparison, file-size checks, and tree-membership checks.

| Reader | Median extraction | Observed range | Throughput | Speed relative to tuned bsdtar |
|---|---:|---:|---:|---:|
| bsdtar, default | 226.72 ms | 223.97–228.39 ms | 891 MiB/s | 0.80× |
| bsdtar, `-b 2048` (1 MiB) | 182.45 ms | 179.78–184.16 ms | 1,108 MiB/s | 1.00× |
| Zipper `native`, four workers | 114.82 ms | 114.26–115.35 ms | 1,760 MiB/s | 1.59× |
| Zipper `native-fast`, four workers | 91.46 ms | 90.25–91.66 ms | 2,210 MiB/s | 1.99× |

Zipper's default ZIP reader delegates to 7-Zip and took 559.38 ms here. Native stream peaked at 4.9 MiB RSS, and native-fast peaked at 174.2 MiB. bsdtar RSS was not measured.

See the [raw samples](../silesia/zip-vs-tar.json).

### Small files

Each column compares readers extracting one unchanged Deflate ZIP.

| Reader | 4,000 grouped files: median [range] ms | 10,000 flat tiny files: median [range] ms |
| --- | ---: | ---: |
| Zipper default / 7-Zip | 240.09 [238.12–242.33] | 556.65 [552.00–586.04] |
| Zipper native stream | 126.74 [125.19–128.08] | 322.32 [318.22–332.45] |
| Zipper native-fast | 126.41 [125.03–129.16] | 328.35 [313.95–372.00] |
| bsdtar default | 293.36 [287.88–324.60] | 703.32 [699.33–709.60] |
| bsdtar, 1 MiB buffer | 293.17 [290.14–294.62] | 704.14 [698.70–717.63] |
| bsdtar, 1 MiB, reduced metadata | 269.14 [267.21–270.99] | 657.09 [640.86–686.66] |

For the grouped and flat fixtures respectively, native streaming is **2.12× / 2.04× faster than reduced-metadata bsdtar**. It is **2.31× / 2.18× faster than normal bsdtar**. Its throughput is about 31,600 / 31,000 files per second. Native-fast ranges overlap streaming, so no fast-mode advantage is established here.

Raw samples are in [grouped files](small-files-vs-tar-minimal.json) and [flat tiny files](tiny-flat-vs-tar-minimal.json). Earlier full-metadata sessions remain in [grouped before](small-files-vs-tar-before.json), [grouped after](small-files-vs-tar-after.json), and [flat](tiny-flat-vs-tar.json).

### TAR

#### Uncompressed TAR on the small-file trees

This table changes the archive format, unlike the identical-ZIP tables above. 7-Zip creates both the TAR and the ZIP, and bsdtar extracts the resulting archives.

| Reader / format | 4,000 grouped files ms | 10,000 flat tiny files ms |
| --- | ---: | ---: |
| bsdtar / TAR | 272.22 | 665.67 |
| bsdtar / TAR, 1 MiB | 268.79 | 658.99 |
| bsdtar / TAR, 1 MiB, reduced metadata | 244.32 | 615.29 |

#### Zipper's own format versus tuned TAR pipelines on Silesia

This comparison uses 1 MiB tar record buffers (`-b 2048`), alongside default-buffer controls, in seven interleaved rounds. Zipper and zstd/pzstd use level 1. Creation uses matched macOS `F_FULLFSYNC`, and extraction checks SHA-256 file manifests. Compressed extraction uses `--ignore-zeros` so tar consumes all padding and the decoder completes without SIGPIPE. Source is e382a55.

| Archive / extractor | Archive bytes | Median extraction | Observed range | Throughput |
|---|---:|---:|---:|---:|
| `.zipper` / Zipper | 73,602,933 | 35.37 ms | 34.33–36.29 ms | 5,715 MiB/s |
| Uncompressed `.tar` / bsdtar, 1 MiB | 212,860,928 | 43.84 ms | 36.55–46.87 ms | 4,610 MiB/s |
| `.tar.zst` / pzstd + bsdtar, 1 MiB | 73,435,731 | 56.24 ms | 55.55–58.01 ms | 3,594 MiB/s |
| `.tar.zst` / zstd + bsdtar, 1 MiB | 73,317,325 | 131.42 ms | 129.22–134.09 ms | 1,538 MiB/s |

**Against these tuned baselines, Zipper extracts 1.24× as fast as plain bsdtar and 1.59× as fast as tar+pzstd.** Its archive is about 0.23% larger than pzstd's. Plain tar has substantial run-to-run spread.

The same run measured default-buffer plain tar at 95.11 ms and default-buffer tar+pzstd at 106.52 ms.

See the [raw tuned samples](../silesia/tar-tuned.json) and the [earlier default-buffer samples](../silesia/tar-current.json).

### Small-file bottleneck and worker count

The [separate sampling profile](small-files-profile.txt) shows that file creation, writes, reads, and mutex waits dominate the archive-processing stacks. Deflate and CRC account for few samples. Aggregated top-of-stack counts include `open` 2,567, `write` 607, `read` 528, input mutex waits 505, and `close` 495. These are samples across threads, not wall-time percentages. Startup and idle samples are also present. The profiling driver repeats extraction without deleting between iterations, then cleans up after profiling.

The [grouped worker sweep](small-files-thread-sweep.json) measured 237.95 / 158.72 / 124.92 / 156.30 / 165.02 / 170.64 ms at 1 / 2 / 4 / 8 / 10 / 16 workers. The [flat sweep](tiny-flat-thread-sweep.json) measured 575.03 / 394.07 / 309.56 / 460.42 / 488.00 ms at 1 / 2 / 4 / 8 / 10 workers. More concurrent tiny-file creates made these macOS workloads slower.

| Automatic native stream | Before median [range] ms | After median [range] ms | Latency reduction |
| --- | ---: | ---: | ---: |
| 4,000 grouped | 198.96 [189.20–203.39] | 163.64 [148.81–173.36] | 17.8% |
| 10,000 flat | 522.14 [481.23–551.86] | 360.39 [348.09–382.27] | 31.0% |

These [grouped](auto-workers-small-files.json) and [flat](auto-workers-tiny-flat.json) revision trials interleave old and new executables. They include 250 ms of untimed cooldown after output cleanup. Their timings differ from the continuous comparator sessions above, so compare revisions within each trial. The grouped trial also confirms that an explicit 10-worker override keeps the higher-concurrency path. Baseline source is `99a04e4`, and executable hashes are in each report. Preliminary capped-pool trials (`limited-workers-*.json`) predate the explicit-override interface and are not the final API measurements.

### Rejected small-file experiments

- [Positional archive reads](positional-small-files.json) gave no four-worker gain, and the ten-worker median regressed from 162.86 to 203.57 ms. Independent review also found a Windows huge-offset sentinel edge case. Fully reverted.
- [Directory-local batches](directory-batches-small-files.json) had overlapping timing ranges and no reliable gain. Reverted.
- [Cached parent-directory handles](directory-handles-small-files.json) gave no speed gain, and review reproduced a permission regression under umask 0444. Reverted along with its direct dependency.

## Decision

Automatic native ZIP extraction now caps concurrency at four workers on macOS when an archive contains at least 1,024 regular files, all at most 16 KiB. Explicit `--threads N` overrides the policy. Lower caller pools remain upper bounds. Larger entries, integrity-only verification, and Windows and Linux keep the previous policy. This change is on main. It does not replace the already-published Windows alpha asset.

All retained changes preserve CRC and decoded-size checks, exclusive output creation, staging cleanup on failure, and publication after all workers complete. Regression coverage includes 1,030 tiny entries, one-thread and ten-thread callers, both native modes, exact bytes, invalid options, and corrupt-CRC cleanup. Independent final review found no blockers.

Zipper's native readers stay opt-in. The default ZIP reader still delegates to 7-Zip.

## Limits

These are local workload results, not general performance guarantees. They do not measure a user-supplied archive or Windows performance.

The four-worker cap is a measured local heuristic. It is not a claim that four is optimal on every Mac, filesystem, or storage device.

Zipper's native readers preserve bytes and directories, not full archive metadata. The reduced-metadata bsdtar settings do not establish full feature equivalence. Metadata capabilities also differ between `.zipper` and TAR.

bsdtar uses a single extraction process. These results are not a comparison against several coordinated tar extraction processes.

Every Zipper row compared with bsdtar or 7-Zip ran four workers. The Silesia rows requested four, and the small-file rows used automatic selection, which picked four. bsdtar and 7-Zip each ran as one process, with 7-Zip using its default thread allocation. The reports record wall time only, not CPU time. The one-worker native sweep measured 237.95 ms on the grouped fixture and 575.03 ms on the flat fixture, against 269.14 and 657.09 ms for reduced-metadata bsdtar. That sweep used an older binary in a separate session, so those one-worker ratios of about 1.13× and 1.14× are approximate. The [matched-thread re-run](../rust-dependencies/README.md) compares readers at equal worker counts.

The Silesia bsdtar rows ran without the reduced-metadata flags, so bsdtar restored metadata that Zipper does not. 7-Zip restores timestamps and attributes that Zipper does not.

Earlier 2.7×/3× claims compared the default-buffer TAR settings. They must not be presented as gains over the stronger tuned baselines.

Storage and thermal isolation are absent. Native in-process peak RSS is retained in JSON. Backend process-tree memory is not compared.

## Reproduce

Run these from the repository root. The Silesia commands need the fetched Silesia corpus.

```sh
# Silesia, identical ZIP:
python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia \
  --formats zip --native-fast --tar-reader tar --reuse-archives \
  --runs 7 --threads 4

# Silesia, own format versus tuned TAR pipelines:
python3 scripts/compare_zstd.py /tmp/zipper-corpora --corpus silesia \
  --include-plain-tar --tuned-tar --runs 7

# Small files:
python3 scripts/make_tiny_corpus.py /tmp/zipper-corpora/tiny-flat
python3 scripts/compare_formats.py /tmp/zipper-corpora/tiny-flat --formats zip tar \
  --native-fast --native-auto --tar-reader tar --tar-minimal --reuse-archives --runs 7

# Profile outside benchmark runs, attaching the platform profiler to this process:
cargo build --release --example profile_zip
 target/release/examples/profile_zip /tmp/zipper-corpora/small-files.zip native 30 4
```

To extract with the automatic policy, or with an explicit concurrency override:

```sh
zipper --zip-engine native extract files.zip -o restored
zipper --zip-engine native --threads 4 extract files.zip -o restored
```
