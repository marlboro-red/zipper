# M26. Cargo features for each format

A Rust program that depends on Zipper builds only the readers it names.

The `zip`, `7z`, `tar`, `more-formats`, `all-formats` and `cli` features shipped on 10 November 2026. A `zip` and `7z` build then compiled 49 crates against 69 for the default. A build without a reader refuses that format with `zipper::NotBuilt`, and the gate lints each feature alone and runs the ZIP and 7z corpus tests in a `zip` and `7z` build.

Scope:

- Split `more-formats` into a feature for each reader, or for each group of readers that share code.
- After M25, check that no format feature compiles C. Until then, put each C library behind its own feature.
- Document every feature in `docs/usage.md`, and keep `Reader` with every variant in every build.

Exit checks:

1. The gate lints every feature alone and the default, and builds a consumer crate with only `zip` and `7z`.
2. Each format's corpus tests pass in a build with only that format's feature.
3. `cargo tree` for a `zip` and `7z` build lists no crate that only another format uses.

Done so far, by 11 November 2026:

- Every reader has its own feature, with `more-formats` as their umbrella and `zstd` behind its own feature. The shared tree writer, the volume trait, the dictionary limit and the output-name helpers moved to their own modules, so no feature pulls in another reader's code.
- The gate lints each feature alone with unused crate dependencies denied, and runs `tests/consumer`, a crate with only `zip` and `7z` that extracts a corpus ZIP and 7z in process. `cargo xtask format-tests` passes each format's tests in a build with only its feature, and `cargo xtask release-checks` runs it.
- `zip-deflate` builds the ZIP reader for stored and Deflate entries only, and `zip-encryption`, `zip-legacy`, `zip-deflate64`, `zip-bzip2`, `zip-lzma` and `zip-ppmd` each add one group on top. `zip` builds them all. A build refuses an entry it cannot decode with `NotBuilt` naming the feature, writing nothing. A `zip-deflate` build has 28 packages against 40 for `zip`, as the [ZIP feature size record](../../benchmarks/cargo-features/zip-deflate/README.md) measures. The gate runs `tests/zip_method_features.rs` and the consumer crate in a `zip-deflate` build.
- `zip`, `tar` and `squashfs` build without the C `zstd` library and refuse Zstandard data with `NotBuilt("zstd")`, writing nothing. A `zip` and `7z` build had 54 crates against 79 for the default, and compiled no C on Linux or Windows.

Checked on 13 November 2026:

1. Exit check 1 is met. The gate's `feature subsets` step runs `lint_feature_subsets` in `xtask/src/main.rs`, and its `consumer crate` step builds and runs `tests/consumer`, since `2c5a1bc` and `5a73fe6`.
2. Exit check 2 is not met for RAR. `FORMAT_TESTS` in `xtask/src/main.rs` names every reader in `more-formats` except `rar`, so `tests/rar_corpus.rs` and `tests/hostile_rar.rs` never run in a `rar`-only build.
3. Exit check 3 holds, though no gate step checks it. `cargo tree --edges normal --target all` lists 45 crates for a `zip` and `7z` build, exactly the crates of `zip` alone and `7z` alone, against 67 for the default.

## Closed on 14 November 2026

- Exit check 1 is met, as checked on 13 November.
- Exit check 2 is met since `954d0f7`. `FORMAT_TESTS` runs `tests/rar_corpus.rs` and `tests/hostile_rar.rs` in a build with only `rar`. `cargo xtask format-tests` now fails before any build when a member of `all-formats` or `more-formats` has no entry, and `every_format_feature_runs_its_tests_in_a_build_with_only_that_feature` in `xtask/src/main.rs` checks that rule in the gate. The second machine passed `cargo xtask format-tests` at that commit, RAR included.
- Exit check 3 holds, as checked on 13 November. No gate step checks it.
- `cargo xtask verify --owed` lists no owed section.
