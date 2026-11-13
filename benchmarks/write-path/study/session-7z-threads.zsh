set -u
source ${ZIPPER_CHECKOUT:?}/target/wpath/lock.zsh
cd $W
C=$base/target/timing/corpora
S=$W/tree-base/scripts
R=$W/results; mkdir -p $R
SCR=${ZIPPER_SCRATCH:-$W/scratch}
mkdir -p $SCR
echo "scratch volume: $(df $SCR | tail -1 | awk '{print $NF}' | sed 's|.*/||') $(diskutil info $(df $SCR | tail -1 | awk '{print $1}') | grep -E 'File System Personality|Protocol' | tr -s ' ' | tr '\n' ';')"
# fseventsd has used a whole core on this machine for days. It is recorded, not waited for.
busy() { ps -Ao pcpu=,comm= | awk '$1 >= 25' | grep -v -E 'ps$|awk$|fseventsd$' | head -3 }
quiet() {
  for attempt in {1..8}; do
    [ -z "$(busy)" ] && return 0
    sleep 15
  done
  echo "stopping, still busy: $(busy)"; exit 1
}
step() {
  echo "== $1 $(date +%H:%M:%S) load $(sysctl -n vm.loadavg)"
  ps -Ao pcpu=,comm= | awk '$1 >= 25' | grep -E 'fseventsd$' | sed "s|^|$1: busy system service |" >> $R/busy-services.txt
  quiet
}
failed() { echo "FAILED $1"; tail -5 $2; exit 1 }

for workload in flat-tiny dependencies; do
  for kind in nonsolid.7z 7z; do
    step "7z threads $workload $kind"
    fresh=$(mktemp -d $SCR/s4.XXXXXX)
    python3 $S/compare_zip_readers.py arch/$workload.$kind $C/$workload/$workload --archive-format 7z --runs 7 --work-dir $fresh --allow-busy \
      --reader t1 native bin/zipper-base --reader t2 native bin/zipper-base --reader t3 native bin/zipper-base --reader t4 native bin/zipper-base \
      --reader t6 native bin/zipper-base --reader auto native bin/zipper-base \
      --reader-threads t1 1 --reader-threads t2 2 --reader-threads t3 3 --reader-threads t4 4 --reader-threads t6 6 --reader-threads auto auto \
      > $R/threads-$workload-$kind.json 2> $R/threads-$workload-$kind.err || failed "7z threads $workload $kind" $R/threads-$workload-$kind.err
    rmdir $fresh
  done
done
for workload in dependencies flat-tiny; do
  step "probe threads $workload"
  fresh=$(mktemp -d $SCR/s4.XXXXXX)
  bin/write_probe $C/$workload/$workload $fresh 7 base create-close-only no-times > $R/probe-threads-$workload.json 2> $R/probe-threads-$workload.err \
    || failed "probe threads $workload" $R/probe-threads-$workload.err
  rmdir $fresh
done
echo "== done $(date +%H:%M:%S)"
