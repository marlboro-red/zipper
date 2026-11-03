# Real dependency-source extraction

On a tree of 70 crates.io packages, the native archive is 14.5% smaller than the tested ZIP. With one thread, Zipper extracts the identical ZIP 1.20× to 1.33× faster than bsdtar and uses less CPU. The earlier 2.22× used four Zipper workers against single-process bsdtar, and that speed costs more CPU than bsdtar uses.

## Question

How do Zipper and the classic-format readers compare on real source code rather than synthetic tiny-file fixtures?

## Setup

The workload has 70 locked crates.io packages, 3,981 regular files, and 67,479,735 input bytes. `scripts/make_dependency_corpus.py` builds it with `cargo metadata --locked`. The generator copies dependency trees without following symlinks. It records package versions, the Cargo.lock hash, and the SHA-256 of every file. Source packages include code, tests, licenses, and Cargo package metadata. The generator intentionally includes only crates.io registry dependencies and excludes workspace and private packages. The repository commits only manifests and measurements, not copies of third-party source trees.

The run used these settings:

- Apple M5, source `ed07524` production reader.
- Seven interleaved measured rounds after warmup, with four Zipper workers.
- ZIP readers receive identical ZIP bytes.
- Native is level 1 with 1 MiB blocks. TAR is uncompressed.
- bsdtar uses 1 MiB records and reduced metadata flags.
- The native-format option adds creation outside timing and measures extraction alongside the classic-format readers. The JSON records the native creation settings.

The report holds the binary and archive hashes. Every run checks every restored byte, file size, and full directory membership.

## Results

| Reader / format | Archive bytes | Extraction ms [range] | Peak RSS MiB |
| --- | ---: | ---: | ---: |
| Zipper / .zipper | 12,243,518 | 173.64 [170.45–176.22] | 25.53 |
| Zipper native / ZIP | 14,328,097 | 174.78 [167.86–208.78] | 6.70 |
| Zipper native-fast / ZIP | 14,328,097 | 162.34 [159.90–177.42] | 7.83 |
| 7-Zip / ZIP | 14,328,097 | 442.87 [439.72–500.63] | 5.75 |
| bsdtar / ZIP | 14,328,097 | 359.79 [357.05–380.24] | 6.14 |
| bsdtar / TAR | 71,127,040 | 300.24 [295.44–322.99] | 4.44 |

The native archive extracts 1.73× faster than tuned plain TAR with four workers. Native `.zipper` uses more memory than native-fast ZIP here. Their extraction ranges overlap, so this run does not rank their speed.

### Matched thread counts

A second session at `40cfbed` ran every Zipper reader at 1 and 4 threads beside the single-process readers. It recorded CPU time (user plus system) for every process. Seven interleaved rounds followed one warmup, and every round passed the full byte and tree check.

| Reader / format | Threads | Wall ms [range] | CPU ms | CPU per wall second | Peak RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Zipper native-fast / ZIP | 4 | 154.04 [148.08–194.91] | 467.01 | 3.03 | 8.58 |
| Zipper / .zipper | 4 | 162.04 [156.79–196.50] | 499.04 | 3.08 | 25.95 |
| Zipper native / ZIP | 4 | 164.28 [160.33–235.30] | 495.63 | 3.02 | 6.70 |
| Zipper / .zipper | 1 | 237.59 [208.06–272.71] | 239.15 | 1.01 | 11.58 |
| Zipper native-fast / ZIP | 1 | 260.21 [254.75–307.71] | 258.30 | 0.99 | 6.92 |
| bsdtar / TAR | 1 | 288.34 [283.84–311.95] | 285.49 | 0.99 | 4.45 |
| Zipper native / ZIP | 1 | 289.38 [283.45–326.28] | 286.52 | 0.99 | 5.61 |
| bsdtar / ZIP | 1 | 346.32 [345.28–385.47] | 344.50 | 0.99 | 6.16 |
| 7-Zip / ZIP | 1 | 450.20 [437.30–479.04] | 440.08 | 0.98 | 5.81 |

- **Same thread count.** Native-fast at one thread is 1.33× faster than bsdtar on the same ZIP and uses 25% less CPU. Native at one thread is 1.20× faster. Both ranges are separate from bsdtar's.
- **Four workers.** Native-fast is 2.25× faster than bsdtar in wall time but uses 36% more CPU in total.
- **7-Zip.** It restores timestamps and attributes that Zipper does not, so its row is not a matched comparison.



## Decision

No decision was recorded. This record adds a real-code workload beside the synthetic tiny-file fixtures. Report the matched-thread rows with any speed claim against bsdtar or 7-Zip.

## Limits

- No result is a cold-cache or crash-durability measurement.
- Zipper preserves bytes and directories. bsdtar runs with reduced-metadata flags. 7-Zip still restores timestamps and attributes.
- The first table ran four Zipper workers against single-process readers and recorded wall time only. The matched-thread table corrects that.
- External readers get an empty output root before timing. Zipper's time includes staging and publication.
- Whole-process RSS is not an allocation-budget guarantee.
- These are local measurements of one real-code tree, not a cross-platform leadership claim.

## Reproduce

```sh
python3 scripts/make_dependency_corpus.py /tmp/rust-dependencies
python3 scripts/extraction_matrix.py /tmp/rust-dependencies \
  --zipper target/release/zipper --output thread-parity.json \
  --native-format --threads 1 4 --runs 7 --sevenzip 7zz --tar /usr/bin/tar
```

This reproduces both tables. The first table came from `--threads 4` alone at `ed07524`. The script now refuses external readers unless `--threads` includes 1.

Use the committed Cargo.lock and manifest to identify changes in dependency contents between runs.
