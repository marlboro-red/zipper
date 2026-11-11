#!/bin/zsh
# usage: regress.sh OUT RUNS OLD NEW CORPORA ARCHIVES [COMPARE_FLAGS...]
# Times two Zipper binaries on the workloads a Deflate framing change must not slow down, with
# scripts/compare_zip_readers.py, which checks every extracted byte. CORPORA holds the scoreboard's
# source folders and ARCHIVES their archives, with the paths the lines below name. A row whose
# result file exists is skipped, so a run the busy guard stopped can be resumed.
set -eu
out=$1 runs=$2 old=$3 new=$4 corpora=$5 archives=$6
shift 6
here=${0:A:h}
compare=$here/compare_zip_readers.py
[ -f $compare ] || compare=$here/../../../scripts/compare_zip_readers.py
mkdir -p $out $out/scratch
row() {
  local name=$1 archive=$2 source=$3 format=$4
  shift 4
  [ -s $out/$name.json ] && { python3 $here/rows.py $out/$name.json; return 0 }
  python3 $compare $archive $source --archive-format $format --runs $runs --work-dir $out/scratch \
    --reader old native $old --reader new native $new "$@" > $out/$name.json 2> $out/$name.err \
    || { echo "$name failed"; tail -3 $out/$name.err; return 1 }
  python3 $here/rows.py $out/$name.json
}
row silesia-tar-gz $archives/silesia.tar.gz $corpora/silesia/silesia native "$@"
row random-tar-gz $archives/random.tar.gz $corpora/large/random native "$@"
row dependencies-tar-gz $archives/dependencies.tar.gz $corpora/dependencies/dependencies native "$@"
row silesia-zip-t1 $archives/silesia.zip $corpora/silesia/silesia zip --threads 1 "$@"
row silesia-zip-t4 $archives/silesia.zip $corpora/silesia/silesia zip --threads 4 "$@"
row dependencies-zip $archives/dependencies.zip $corpora/dependencies/dependencies zip "$@"
row silesia-deflate-7z $archives/silesia-deflate.7z $corpora/silesia/silesia 7z "$@"
