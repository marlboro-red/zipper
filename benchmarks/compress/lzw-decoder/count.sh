#!/bin/zsh
# On the second machine: build a tree, count instructions of `test` and `extract`, and byte-check `extract` against the plain tar.
set -e
S=${SCRATCH:?set SCRATCH to a work folder}
TREE=${1:?tree}
LABEL=${2:?label}
cd $S/$TREE
CARGO_TARGET_DIR=$S/target-$LABEL cargo build --release 2>&1 | tail -1
BIN=$S/target-$LABEL/release/zipper
cp $BIN $S/out/zipper-$LABEL
cd $S/out
for run in 1 2 3; do
  echo "== $LABEL test $run"
  /usr/bin/time -l $BIN --threads 1 test tar-compress.tar.Z 2>&1 >/dev/null | egrep 'instructions|cycles|maximum resident'
done
rm -rf $S/out/x-$LABEL $S/out/ref
echo "== $LABEL extract"
/usr/bin/time -l $BIN --threads 1 extract tar-compress.tar.Z -o $S/out/x-$LABEL 2>&1 >/dev/null | egrep 'instructions|cycles|maximum resident'
mkdir $S/out/ref
tar -xf silesia.tar -C $S/out/ref
diff -r $S/out/x-$LABEL $S/out/ref && echo "bytes match the plain tar"
chmod -R u+rwx $S/out/x-$LABEL $S/out/ref
rm -rf $S/out/x-$LABEL $S/out/ref
