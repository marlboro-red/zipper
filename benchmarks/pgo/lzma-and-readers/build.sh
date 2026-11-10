#!/bin/zsh
# Builds the plain and the PGO release binaries of one commit into $1.
# Trains on the dependency sources only, so no measured Silesia archive is in the profile.
set -euo pipefail
work=${1:?usage: build.sh WORK_DIR}
root=$(git rev-parse --show-toplevel)
cd "$root"
profdata=$(rustc +1.96.0 --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/llvm-profdata
deps=target/bench-corpora/dependencies
mkdir -p "$work/train" "$work/raw"

cargo build --release
cp target/release/zipper "$work/zipper-plain"

RUSTFLAGS="-Cprofile-generate=$PWD/$work/raw" CARGO_TARGET_DIR=target/pgo-generate cargo build --release
instrumented=target/pgo-generate/release/zipper

(cd "$deps" && tar -cf "$root/$work/train/deps.tar" dependencies)
xz -6 -k -T1 -c "$work/train/deps.tar" > "$work/train/deps.tar.xz"
gzip -6 -c "$work/train/deps.tar" > "$work/train/deps.tar.gz"
zstd -q -19 -c "$work/train/deps.tar" > "$work/train/deps.tar.zst"
bzip2 -9 -c "$work/train/deps.tar" > "$work/train/deps.tar.bz2"
lzip -6 -c "$work/train/deps.tar" > "$work/train/deps.tar.lz"
(cd "$deps" && 7zz a -bso0 -bd -t7z -m0=LZMA2 -mx6 "$root/$work/train/deps.7z" dependencies)
(cd "$deps" && 7zz a -bso0 -bd -t7z -m0=LZMA -ms=off -mx6 "$root/$work/train/deps-lzma-nonsolid.7z" dependencies)
(cd "$deps" && 7zz a -bso0 -bd -tzip -mm=Deflate -mx6 "$root/$work/train/deps.zip" dependencies)
(cd "$deps" && 7zz a -bso0 -bd -tzip -mm=LZMA "$root/$work/train/deps-lzma.zip" dependencies)

for archive in "$work"/train/deps*.*; do
  [[ $archive == *.tar ]] && continue
  for threads in 1 4; do
    out=$(mktemp -d "$PWD/target/bench-work/pgo-train.XXXXXX")
    "$instrumented" --threads $threads extract "$archive" -o "$out/x"
    rm -r "${out:?}"
  done
done

"$profdata" merge -o "$work/merged.profdata" "$work"/raw/*.profraw
RUSTFLAGS="-Cprofile-use=$PWD/$work/merged.profdata" CARGO_TARGET_DIR=target/pgo-use cargo build --release
cp target/pgo-use/release/zipper "$work/zipper-pgo"
shasum -a 256 "$work/zipper-plain" "$work/zipper-pgo" "$work/merged.profdata"
