# Real dependency-source extraction

On a tree of 70 crates.io packages, the native archive is 14.5% smaller than the tested ZIP and extracts 1.73× faster than tuned plain TAR. Native-fast extracts the identical ZIP 2.22× faster than bsdtar, and native `.zipper` does not beat it on every metric.

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

Native `.zipper` uses more memory and is slower than native-fast ZIP here. It does not dominate that reader on every metric. A [native level and block-size sweep on this corpus](level-sweep.md) measures the size and speed trade.

## Decision

No decision was recorded. This record adds a real-code workload beside the synthetic tiny-file fixtures.

## Limits

- No result is a cold-cache or crash-durability measurement.
- Zipper preserves bytes and directories, while external tools can still do different metadata work.
- External readers get an empty output root before timing. Zipper's time includes staging and publication.
- Whole-process RSS is not an allocation-budget guarantee.
- These are local measurements of one real-code tree, not a cross-platform leadership claim.

## Reproduce

```sh
python3 scripts/make_dependency_corpus.py /tmp/rust-dependencies
python3 scripts/extraction_matrix.py /tmp/rust-dependencies \
  --zipper target/release/zipper --output results/dependencies.json \
  --native-format --threads 4 --runs 7 --sevenzip 7zz --tar /usr/bin/tar
```

Use the committed Cargo.lock and manifest to identify changes in dependency contents between runs.
