#!/bin/zsh
# usage: final_rows.sh OUT RUNS BASE FINAL CORPUS SILESIA_FOLDER ZLIB_REFERENCE
# Times the end-to-end rows of this record with time_tools.py. CORPUS is the folder that
# make_zlib_workloads.py wrote, with random.bin, random.bin.gz and silesia.dmg added as the
# record's Reproduce section says. ZLIB_REFERENCE is the program built from zlib_reference.c.
# A row whose result file exists is skipped, so a run the busy guard stopped can be resumed.
set -eu
out=$1 runs=$2 base=$3 final=$4 corpus=$5 silesia=$6 reference=$7
here=${0:A:h}
mkdir -p $out
tools() {
  local name=$1 plain=$2 archive=$3
  shift 3
  [ -s $out/$name.json ] && return 0
  python3 $here/time_tools.py --plain $plain --archive $archive --work $out/scratch --runs $runs \
    --zipper base=$base --zipper final=$final "$@" > $out/$name.json
}
tools gzip-silesia $corpus/blob.bin $corpus/blob.bin.gz \
  --stdout "libdeflate-gunzip=libdeflate-gunzip -c" --stdout "gzip=/usr/bin/gzip -d -c" \
  --folder "7zz=7zz x -bso0 -bsp0 -o{out} {archive}"
tools gzip-random $corpus/random.bin $corpus/random.bin.gz \
  --stdout "libdeflate-gunzip=libdeflate-gunzip -c" --stdout "gzip=/usr/bin/gzip -d -c"
tools zlib-swf "$corpus/blob~.swf" $corpus/blob.swf \
  --stdout "libdeflate-zlib=$reference 8" --folder "7zz=7zz x -bso0 -bsp0 -o{out} {archive}"
tools zlib-xar $silesia $corpus/silesia.xar \
  --folder "xar=sh -c \"mkdir {out} && xar -xf {archive} -C {out}\"" \
  --folder "7zz=7zz x -bso0 -bsp0 -o{out} {archive}"
tools zlib-dmg $silesia $corpus/silesia.dmg
