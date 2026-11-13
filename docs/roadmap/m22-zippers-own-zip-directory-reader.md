# M22. Zipper's own ZIP directory reader

Zipper reads the ZIP central directory with its own parser, so the `zip` crate leaves the release build.

The native ZIP reader already checks the end record for ambiguity, every local header, ZIP64 fields and data descriptors itself, with Kani proofs. It still lists entries through the `zip` crate's `ZipArchive`, which alone brings `indexmap`, `memchr`, `nix` and `typed-path` into the release build. An audit on 5 November 2026 found no other dependency to remove outside M21, which removes liblzma.

Scope:

- The end record, the ZIP64 end record and locator, the central directory entries, their extra fields and names, read by Zipper's own code within the documented limits.
- The `zip` crate stays a test dependency for writing archives.

Exit checks:

1. A differential test lists every archive in the ZIP corpora and the real-world set with the new parser and with the `zip` crate, and finds the same entries or the same refusal.
2. Kani proves the central directory entry, its extra fields and the ZIP64 records never panic or overflow, up to a bound.
3. Every test in `tests/hostile_zip.rs` and `tests/zip_fuzz.rs` passes, and an adversarial review finds no layout the new parser accepts that the old one refused.
4. `cargo tree -e normal` shows no `zip` crate, and the scoreboard shows no slower listing.

Done so far, by 8 November 2026:

- The reader in `src/zip_directory.rs` lists every ZIP entry in the release build, for `zipper list`, both native engines, encrypted entries, split sets and self-extractors. The `zip` crate is a dev-dependency only. Its removal took `zip`, `indexmap`, `hashbrown`, `equivalent`, `memchr`, `nix` and `typed-path` out of `cargo tree -e normal`.
- A differential test agrees with the `zip` crate on the corpora, the encrypted set, hand-built shapes and damaged archives, or both refuse. It also compares what `native_zip::list` gives, and lists the corpus split set against the same tree in one piece. Its ignored real-world case listed all 95 ZIPs of the real-world set the same.
- The ten Kani proofs of `[zip-directory]`, run by `cargo xtask verify-sync zip-directory`.
- Every test in `tests/hostile_zip.rs` and `tests/zip_fuzz.rs` passes.
- An adversarial review of the wired path ran about 6,850 archives on `zipper list` and both engines. It confirmed no layout that the new path accepts and the `zip` crate refuses, and no listing that differs. Peak memory of `zipper list` stayed at or below the old build's on archives at the entry and metadata limits.
- A run of 5,000 damaged copies of each archive found one empty archive of ZIP64 records without sentinels that the new reader listed and the old one refused. The reader now refuses it, as the old reader and 7-Zip do, and a built case covers it.
- One layout stays accepted on purpose. A ZIP64 end record without sentinels after a directory with entries lists its real entries, as the `zip` crate and Python list them, though the old reader refused it. Refusing it would risk archives that real writers produce.
Closed on 8 November 2026:

- Listing takes the same time as with the `zip` crate, as the [listing record](../../benchmarks/native-zip/own-directory-listing/README.md) shows, and the `[zip-directory]` proofs passed on the final code. M22 closed on 8 November 2026, in `46a8df0`.
