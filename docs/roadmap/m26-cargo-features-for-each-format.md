# M26. Cargo features for each format

A Rust program that depends on Zipper builds only the readers it names.

The `zip`, `7z`, `tar`, `more-formats`, `all-formats` and `cli` features shipped on 10 November 2026. A `zip` and `7z` build compiles 49 crates against 69 for the default. A build without a reader refuses that format with `zipper::NotBuilt`, and the gate lints each feature alone and runs the ZIP and 7z corpus tests in a `zip` and `7z` build.

Scope:

- Split `more-formats` into a feature for each reader, or for each group of readers that share code.
- After M25, check that no format feature compiles C. Until then, put each C library behind its own feature.
- Document every feature in `docs/usage.md`, and keep `Reader` with every variant in every build.

Exit checks:

1. The gate lints every feature alone and the default, and builds a consumer crate with only `zip` and `7z`.
2. Each format's corpus tests pass in a build with only that format's feature.
3. `cargo tree` for a `zip` and `7z` build lists no crate that only another format uses.
