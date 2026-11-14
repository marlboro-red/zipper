#!/bin/zsh
# On the second machine: build a tree, then count instructions of `test` and byte-check `extract` against bsdtar.
set -e
S=${SCRATCH:?set SCRATCH to a work folder}
TREE=${1:?tree}
LABEL=${2:?label}
cd $S/$TREE
CARGO_TARGET_DIR=$S/target-$LABEL cargo build --release 2>&1 | tail -1
BIN=$S/target-$LABEL/release/zipper
cp $BIN $S/out/zipper-$LABEL
cd $S/out
for archive in ${=ARCHIVES:-silesia.tar.lz4}; do
  echo "== $LABEL $archive test"
  /usr/bin/time -l $BIN --threads 1 test $archive 2>&1 >/dev/null | egrep 'instructions|cycles|maximum resident'
  rm -rf $S/out/x-$LABEL $S/out/ref-$LABEL
  echo "== $LABEL $archive extract"
  /usr/bin/time -l $BIN --threads 1 extract $archive -o $S/out/x-$LABEL 2>&1 >/dev/null | egrep 'instructions|cycles|maximum resident'
  mkdir $S/out/ref-$LABEL
  bsdtar -xf $archive -C $S/out/ref-$LABEL
  diff -r $S/out/x-$LABEL $S/out/ref-$LABEL && echo "bytes match bsdtar"
  chmod -R u+rwx $S/out/x-$LABEL $S/out/ref-$LABEL
  rm -rf $S/out/x-$LABEL $S/out/ref-$LABEL
done
