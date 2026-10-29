# Real dependency-source extraction

A source-code workload complements the synthetic tiny-file fixtures: 70 locked
crates.io packages, 3,981 regular files and 67,479,735 input bytes. The generator
uses `cargo metadata --locked`, copies dependency trees without following symlinks,
and records package versions, Cargo.lock hash and every file's SHA-256. Source
packages include code, tests, licenses and Cargo package metadata. Only manifests
and measurements are committed, not copies of third-party source trees.

Apple M5, source `ed07524` production reader, seven interleaved measured rounds
after warmup, four Zipper workers. The binary and archive hashes are in the report.
Every run checks every restored byte, file size and full directory membership.
ZIP readers receive identical ZIP bytes. Native is level 1 with 1 MiB blocks;
TAR is uncompressed. No result is a cold-cache or crash-durability measurement.

| Reader / format | Archive bytes | Extraction ms [range] | Peak RSS MiB |
| --- | ---: | ---: | ---: |
| Zipper / .zipper | 12,243,518 | 173.64 [170.45–176.22] | 25.53 |
| Zipper native / ZIP | 14,328,097 | 174.78 [167.86–208.78] | 6.70 |
| Zipper native-fast / ZIP | 14,328,097 | 162.34 [159.90–177.42] | 7.83 |
| 7-Zip / ZIP | 14,328,097 | 442.87 [439.72–500.63] | 5.75 |
| bsdtar / ZIP | 14,328,097 | 359.79 [357.05–380.24] | 6.14 |
| bsdtar / TAR | 71,127,040 | 300.24 [295.44–322.99] | 4.44 |

On this tree, the native archive is 14.5% smaller than the tested ZIP and extracts
1.73× faster than tuned plain TAR. Native-fast extracts the identical ZIP 2.22×
faster than bsdtar. Native `.zipper` has higher memory use and is slower than
native-fast ZIP here; it does not dominate that reader on every metric.

bsdtar uses 1 MiB records and reduced metadata flags. Zipper preserves bytes and
directories, while external tools can still do different metadata work. External
readers get an empty output root before timing; Zipper includes staging and
publication. Whole-process RSS is not an allocation-budget guarantee. These are
local measurements of one real-code tree, not a cross-platform leadership claim.

```sh
python3 scripts/make_dependency_corpus.py /tmp/rust-dependencies
python3 scripts/extraction_matrix.py /tmp/rust-dependencies \
  --zipper target/release/zipper --output results/dependencies.json \
  --native-format --threads 4 --runs 7 --sevenzip 7zz --tar /usr/bin/tar
```

Use the committed Cargo.lock and manifest to identify changes in dependency
contents between runs. The generator intentionally includes only crates.io
registry dependencies, excluding workspace/private packages. The native-format
option adds creation outside timing and measures extraction alongside the
classic-format readers; native creation settings are recorded in JSON.
