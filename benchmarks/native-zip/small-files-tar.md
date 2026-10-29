# Small-file ZIP extraction versus bsdtar

Apple M5, 10 logical CPUs, 32 GiB RAM, macOS 26.7.1; release builds. Seven interleaved measured rounds after one warmup. Every extracted tree is compared directly byte for byte, with file sizes and tree membership checked. These fixtures were generated locally; this does not measure a user-supplied archive or Windows performance.

## Identical ZIP bytes

Each column compares readers extracting one unchanged Deflate ZIP. Native Zipper uses automatic worker selection (four active extraction workers on these workloads). The compatibility path is still the default and uses 7-Zip 26.05. bsdtar is 3.5.3 with libarchive 3.7.4.

| Reader | 4,000 grouped files: median [range] ms | 10,000 flat tiny files: median [range] ms |
| --- | ---: | ---: |
| Zipper default / 7-Zip | 240.09 [238.12–242.33] | 556.65 [552.00–586.04] |
| Zipper native stream | 126.74 [125.19–128.08] | 322.32 [318.22–332.45] |
| Zipper native-fast | 126.41 [125.03–129.16] | 328.35 [313.95–372.00] |
| bsdtar default | 293.36 [287.88–324.60] | 703.32 [699.33–709.60] |
| bsdtar, 1 MiB buffer | 293.17 [290.14–294.62] | 704.14 [698.70–717.63] |
| bsdtar, 1 MiB, reduced metadata | 269.14 [267.21–270.99] | 657.09 [640.86–686.66] |

Native streaming is **2.12× / 2.04× faster than reduced-metadata bsdtar**, or **2.31× / 2.18× faster than normal bsdtar**, for grouped/flat fixtures respectively. Its throughput is about 31,600 / 31,000 files per second. Native-fast ranges overlap streaming, so no fast-mode advantage is established here.

Reduced-metadata bsdtar uses `-b 2048 -m --no-same-owner --no-same-permissions --no-acls --no-fflags --no-xattrs --no-mac-metadata`. This gives tar a comparison closer to our byte-and-directory-only native reader, but does not establish full feature equivalence. Normal bsdtar restores more metadata.

Raw samples: [grouped files](small-files-vs-tar-minimal.json), [flat tiny files](tiny-flat-vs-tar-minimal.json). Earlier full-metadata sessions remain in [grouped before](small-files-vs-tar-before.json), [grouped after](small-files-vs-tar-after.json), and [flat](tiny-flat-vs-tar.json).

## Uncompressed TAR on the same trees

This table changes the archive format, unlike the identical-ZIP table above. TAR and ZIP are both created by 7-Zip; bsdtar extracts the resulting archives.

| Reader / format | 4,000 grouped files ms | 10,000 flat tiny files ms |
| --- | ---: | ---: |
| bsdtar / TAR | 272.22 | 665.67 |
| bsdtar / TAR, 1 MiB | 268.79 | 658.99 |
| bsdtar / TAR, 1 MiB, reduced metadata | 244.32 | 615.29 |

The grouped tree has 16,366,890 payload bytes: ZIP is 1,088,044 bytes and TAR is 18,454,016 bytes. The flat tree has 9,536,000 payload bytes: ZIP is 3,383,629 bytes and TAR is 16,385,536 bytes. Its sizes cycle through 0, 32, 128, 512, and 4,096 bytes, including 2,000 empty files and both repetitive and deterministic pseudorandom data.

## Bottleneck and retained change

The [separate sampling profile](small-files-profile.txt) shows file creation, writes, reads, and mutex waits dominate the archive-processing stacks; Deflate and CRC account for few samples. Aggregated top-of-stack counts include `open` 2,567, `write` 607, `read` 528, input mutex waits 505, and `close` 495. These are samples across threads, not wall-time percentages; startup and idle samples are also present. The profiling driver repeats extraction without deleting between iterations, then cleans up after profiling.

The [grouped worker sweep](small-files-thread-sweep.json) measured 237.95 / 158.72 / 124.92 / 156.30 / 165.02 / 170.64 ms at 1 / 2 / 4 / 8 / 10 / 16 workers. The [flat sweep](tiny-flat-thread-sweep.json) measured 575.03 / 394.07 / 309.56 / 460.42 / 488.00 ms at 1 / 2 / 4 / 8 / 10 workers. More concurrent tiny-file creates made these macOS workloads slower.

Automatic native ZIP extraction now caps concurrency at four workers on macOS when an archive contains at least 1,024 regular files, all at most 16 KiB. Explicit `--threads N` overrides the policy. Lower caller pools remain upper bounds. Larger entries, integrity-only verification, and Windows/Linux keep the previous policy. This is a measured local heuristic, not a claim that four is optimal on every Mac, filesystem, or storage device.

| Automatic native stream | Before median [range] ms | After median [range] ms | Latency reduction |
| --- | ---: | ---: | ---: |
| 4,000 grouped | 198.96 [189.20–203.39] | 163.64 [148.81–173.36] | 17.8% |
| 10,000 flat | 522.14 [481.23–551.86] | 360.39 [348.09–382.27] | 31.0% |

These [grouped](auto-workers-small-files.json) and [flat](auto-workers-tiny-flat.json) revision trials interleave old/new executables and include 250 ms of untimed cooldown after output cleanup. Their timings differ from the continuous comparator sessions above; compare revisions within each trial. The grouped trial also confirms an explicit 10-worker override retains the higher-concurrency path. Baseline source is `99a04e4`; executable hashes are in each report. Preliminary capped-pool trials (`limited-workers-*.json`) predate the explicit-override interface and are not the final API measurements.

## Rejected experiments

- [Positional archive reads](positional-small-files.json): no four-worker gain; ten-worker median regressed from 162.86 to 203.57 ms. Independent review also found a Windows huge-offset sentinel edge case. Fully reverted.
- [Directory-local batches](directory-batches-small-files.json): timing ranges overlap; no reliable gain. Reverted.
- [Cached parent-directory handles](directory-handles-small-files.json): no speed gain, and review reproduced a permission regression under umask 0444. Reverted along with its direct dependency.

All retained changes preserve CRC/decoded-size checks, exclusive output creation, staging cleanup on failure, and publication after all workers complete. Regression coverage includes 1,030 tiny entries, one/ten-thread callers, both native modes, exact bytes, invalid options, and corrupt-CRC cleanup. Independent final review found no blockers.

## Use and reproduce

```sh
zipper --zip-engine native extract files.zip -o restored
# Explicit concurrency override:
zipper --zip-engine native --threads 4 extract files.zip -o restored

python3 scripts/make_tiny_corpus.py /tmp/zipper-corpora/tiny-flat
python3 scripts/compare_formats.py /tmp/zipper-corpora/tiny-flat --formats zip tar \
  --native-fast --native-auto --tar-reader tar --tar-minimal --reuse-archives --runs 7
cargo build --release --example profile_zip
# Run this process and attach the platform profiler outside benchmark runs:
 target/release/examples/profile_zip /tmp/zipper-corpora/small-files.zip native 30 4
```

The original grouped fixture comes from `scripts/make_benchmark_corpus.py`. Times include process startup and extraction writes, but not explicit output sync, verification, or cleanup. bsdtar output-root creation occurs before its timer. Caches are warm; storage/thermal isolation is absent. Native in-process peak RSS is retained in JSON; backend process-tree memory is not compared. This change is on main; it does not replace the already-published Windows alpha asset.
