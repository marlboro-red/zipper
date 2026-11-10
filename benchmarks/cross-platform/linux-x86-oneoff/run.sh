#!/usr/bin/env bash
# One-off Linux x86-64 rerun of three macOS records on a hosted runner:
# benchmarks/lzma/sevenz-rust2, benchmarks/pgo/lzma-and-readers and the Rust 1.99.1 toolchain timing.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"
out=target/linux-oneoff
work=target/bench-work
mkdir -p "$out" "$work/linux" "$work/7z-probe"
silesia=target/bench-corpora/silesia/silesia
deps=target/bench-corpora/dependencies/dependencies
python3 scripts/fetch_silesia.py "$silesia"
python3 scripts/make_dependency_corpus.py "$deps"
seven=$(command -v 7zz || command -v 7z)
"$seven" | head -3 > "$out/7zip-version.txt"
lscpu > "$out/lscpu.txt"

(cd target/bench-corpora/silesia && "$seven" a -bso0 -bd -t7z -m0=LZMA2 -mx6 "$root/$work/7z-probe/silesia.7z" silesia)
(cd target/bench-corpora/silesia && "$seven" a -bso0 -bd -t7z -m0=LZMA -mx6 "$root/$work/7z-probe/silesia-lzma1.7z" silesia)
(cd target/bench-corpora/silesia && "$seven" a -bso0 -bd -t7z -m0=LZMA2 -mx6 -ms=off "$root/$work/7z-probe/silesia-nonsolid.7z" silesia)
(cd target/bench-corpora/dependencies && "$seven" a -bso0 -bd -t7z -m0=LZMA2 -mx6 "$root/$work/7z-probe/dependencies.7z" dependencies)
(cd target/bench-corpora/dependencies && "$seven" a -bso0 -bd -t7z -m0=LZMA2 -mx6 -ms=off "$root/$work/7z-probe/dependencies-nonsolid.7z" dependencies)
m=$work/linux
(cd target/bench-corpora/silesia && tar -cf "$root/$m/silesia.tar" silesia)
xz -6 -T1 -c "$m/silesia.tar" > "$m/silesia.tar.xz"
gzip -6 -c "$m/silesia.tar" > "$m/silesia.tar.gz"
zstd -q -19 -T0 -c "$m/silesia.tar" > "$m/silesia.tar.zst"
bzip2 -9 -c "$m/silesia.tar" > "$m/silesia.tar.bz2"
lzip -6 -c "$m/silesia.tar" > "$m/silesia.tar.lz"
(cd target/bench-corpora/silesia && "$seven" a -bso0 -bd -tzip -mm=Deflate -mx6 "$root/$m/silesia-deflate.zip" silesia)
(cd target/bench-corpora/silesia && "$seven" a -bso0 -bd -tzip -mm=LZMA "$root/$m/silesia-lzma.zip" silesia)
python3 -c "import sys; sys.path.insert(0, 'scripts'); from pathlib import Path; from codec_scoreboard import zstd_zip; zstd_zip(Path('$silesia'), Path('$m/silesia-zstd.zip'))"

benchmarks/pgo/lzma-and-readers/build.sh "$work/pgo"
CARGO_TARGET_DIR=target/rust-1.99.1 cargo +1.99.1 build --release
cp target/rust-1.99.1/release/zipper "$work/pgo/zipper-1.99.1"
(cd benchmarks/lzma/sevenz-rust2/harness && CARGO_TARGET_DIR="$root/target/sevenz-rust2-harness" cargo +1.96.0 build --release)
mkdir -p "$work/sr2"
cargo test --release --lib --no-run --message-format=json \
  | python3 -c "import json,sys; [print(m['executable']) for m in map(json.loads, sys.stdin) if m.get('executable') and m['target']['name'] == 'zipper' and m['profile']['test']]" > "$out/probe-path.txt"
cp "$(cat "$out/probe-path.txt")" "$work/sr2/probe-asm"
sed -i 's/features = \["std", "optimization"\] }/features = ["std"] }/' Cargo.toml
cargo test --release --lib --no-run --message-format=json \
  | python3 -c "import json,sys; [print(m['executable']) for m in map(json.loads, sys.stdin) if m.get('executable') and m['target']['name'] == 'zipper' and m['profile']['test']]" > "$out/probe-path.txt"
cp "$(cat "$out/probe-path.txt")" "$work/sr2/probe-safe"
git checkout Cargo.toml
sha256sum "$work"/pgo/zipper-* "$work"/sr2/probe-* target/sevenz-rust2-harness/release/sevenz-rust2-extract > "$out/binaries.txt"

python3 benchmarks/lzma/sevenz-rust2/run-probe.py "$work/sr2"
cp benchmarks/lzma/sevenz-rust2/samples-probe.json "$out/samples-probe.json"

compare() {
  local name=$1 format=$2 archive=$3 source=$4 threads=$5
  shift 5
  python3 scripts/compare_zip_readers.py "$archive" "$source" --archive-format "$format" --runs 7 --cooldown 1 \
    --allow-busy --work-dir "$root/$work" "$@" > "$out/$name-t$threads.json"
}
builds() {
  local name=$1 format=$2 archive=$3 threads=$4
  compare "builds-$name" "$format" "$archive" "$silesia" "$threads" \
    --reader plain native "$work/pgo/zipper-plain" --reader pgo native "$work/pgo/zipper-pgo" \
    --reader rust-1.99.1 native "$work/pgo/zipper-1.99.1" \
    --reader-threads plain "$threads" --reader-threads pgo "$threads" --reader-threads rust-1.99.1 "$threads"
}
builds 7z-lzma2-solid 7z "$work/7z-probe/silesia.7z" 1
builds 7z-lzma-solid 7z "$work/7z-probe/silesia-lzma1.7z" 1
builds 7z-nonsolid 7z "$work/7z-probe/silesia-nonsolid.7z" auto
builds tar-xz native "$m/silesia.tar.xz" 1
builds tar-lz native "$m/silesia.tar.lz" 1
builds tar-gz native "$m/silesia.tar.gz" 1
builds tar-zst native "$m/silesia.tar.zst" 1
builds tar-bz2 native "$m/silesia.tar.bz2" 1
builds zip-deflate native "$m/silesia-deflate.zip" 1
builds zip-deflate native "$m/silesia-deflate.zip" auto
builds zip-lzma native "$m/silesia-lzma.zip" 1
builds zip-zstd native "$m/silesia-zstd.zip" 1

for archive in silesia silesia-lzma1 silesia-nonsolid dependencies dependencies-nonsolid; do
  case $archive in silesia*) source=$silesia ;; *) source=$deps ;; esac
  for threads in 1 auto; do
    compare "sevenz-$archive" 7z "$work/7z-probe/$archive.7z" "$source" "$threads" \
      --reader zipper native "$work/pgo/zipper-plain" \
      --reader sevenz-rust2 sevenz-rust2 target/sevenz-rust2-harness/release/sevenz-rust2-extract \
      --reader 7zip 7zip "$seven" \
      --reader-threads zipper "$threads" --reader-threads sevenz-rust2 "$threads" --reader-threads 7zip "$threads"
  done
done
