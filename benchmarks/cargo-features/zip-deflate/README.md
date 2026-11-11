# Size of a ZIP build with only stored and Deflate entries

Kept. A `zip-deflate` build depends on 28 packages against 40 for `zip`, and Zipper adds 0.98 MB to a small release program against 1.25 MB with `zip`. These are sizes on one Mac with one toolchain, and a program that calls more of Zipper's API than `extract_archive` keeps more code.

## Question

A library consumer reads only plain, unencrypted Deflate ZIP archives. The `zip` feature added 27 packages to their lockfile, most for encrypted entries, and Zipper took their binary from 4.5 MB to 5.4 MB. How many packages and bytes does a build with only stored and Deflate entries save?

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, arm64, Rust 1.99.1 from `rust-toolchain.toml`, on 11 November 2026.
- Zipper is the branch that adds the `zip-deflate`, `zip-encryption`, `zip-legacy`, `zip-deflate64`, `zip-bzip2`, `zip-lzma` and `zip-ppmd` features, based on main at `ea91e9c`, with that branch's `Cargo.lock`.
- Packages are counted with `cargo tree -q --locked -p zipper -e normal --no-default-features --features <features> --prefix none` for the host target, each name once, Zipper included.
- The program below calls `zipper::extract_archive` on its two arguments. It sits in its own workspace and depends on Zipper by path. It was built three ways, without Zipper, with `zip-deflate` and with `zip`. Each build used the default release profile, and again with thin LTO and one codegen unit, the profile of Zipper's own binary. `strip -o` gave the stripped sizes.
- The `zip-deflate` binary extracted `tests/data/zip-corpus/7zip-deflate.zip` and refused `tests/data/zip-methods/bzip2.zip` with `NotBuilt("zip-bzip2")`. The `zip` binary extracted that BZip2 archive to the bytes 7-Zip compressed.

```toml
[package]
name = "minimal-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
zipper = { path = "<zipper checkout>", default-features = false, features = ["zip-deflate"], optional = true }

[features]
deflate = ["dep:zipper"]
zip = ["deflate", "zipper/zip"]
```

```rust
fn main() {
    let arguments: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    #[cfg(feature = "deflate")]
    if let [archive, destination] = &arguments[..] {
        if let Err(error) =
            zipper::extract_archive(archive, destination, zipper::ExtractOptions::default())
        {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("usage: minimal-consumer ARCHIVE DESTINATION ({} arguments)", arguments.len());
    std::process::exit(2);
}
```

## Results

| Build | Packages | Release bytes | Stripped | Thin LTO bytes | Thin LTO stripped |
| --- | ---: | ---: | ---: | ---: | ---: |
| No Zipper | 1 | 434,704 | 342,736 | 427,584 | 339,728 |
| `zip-deflate` | 28 | 1,417,872 | 1,065,792 | 1,271,248 | 990,984 |
| `zip` | 40 | 1,684,976 | 1,283,584 | 1,526,304 | 1,208,240 |

Over the program without Zipper, `zip-deflate` adds 983,168 bytes and `zip` adds 1,250,272, so Zipper's share is 21.4% smaller. Stripped, the share is 23.1% smaller, and with thin LTO 23.2% smaller.

The 12 packages in a `zip` build that a `zip-deflate` build leaves out are `aes`, `cipher`, `cmov`, `cpubits`, `ctr`, `ctutils`, `deflate64`, `hmac`, `inout`, `pbkdf2`, `ppmd-rust` and `sha1`. `zeroize` stays, because `sha2` uses it. `packages-zip.txt` and `packages-zip-deflate.txt` list both sets, and `sizes.tsv` holds the sizes.

## Decision

Kept. `zip-deflate` is the ZIP reader with stored and Deflate entries, and each other method group and the two encryption schemes have their own feature on top of it. `zip` still builds all of them, so existing users see no change. A build refuses an entry it cannot decode with `zipper::NotBuilt` naming the feature, before it writes anything. The gate runs `tests/zip_method_features.rs` in a `zip-deflate` build, runs the consumer crate with only `zip-deflate`, and fails when a `zip-deflate` build depends on `aes`, `ctr`, `deflate64`, `hmac`, `pbkdf2`, `ppmd-rust` or `sha1`.

## Limits

- One Mac and one toolchain. Sizes on Linux, Windows or x86-64 differ.
- The consumer's 0.9 MB came from their own program and platform, so it does not compare with these bytes directly.
- The program calls one function. A program that uses more of the API, such as `native_zip::list`, keeps more of Zipper.
- `zip-deflate` still builds `rayon`, `sha2`, `tempfile` and `walkdir`, and compiles Zipper's LZMA decoder module, as every build does.

## Reproduce

1. Write the two files above into a new folder, with the path of a Zipper checkout of this branch.
2. Copy Zipper's `Cargo.lock` into it.
3. For each of no features, `--features deflate` and `--features zip`, run `cargo +1.99.1 build --release --target-dir target-<name>` with that flag, then `strip -o stripped-<name> target-<name>/release/minimal-consumer`.
4. Repeat with `CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1` for the thin LTO columns.
5. In the Zipper checkout, count packages with the `cargo tree` command in Setup for `zip-deflate` and for `zip`.
