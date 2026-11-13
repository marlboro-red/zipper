set -u
source ${ZIPPER_CHECKOUT:?}/target/wpath/lock.zsh
cd $W
ulimit -n 8192
C=$base/target/timing/corpora
S=$W/tree-base/scripts
R=$W/results; mkdir -p $R $W/scratch
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

small=(base largest-first openat no-times times-after close-elsewhere preallocate create-close-only)
large=(base write-64k write-1m write-1m-preallocate write-1m-nocache write-4m preallocate close-elsewhere)
for workload in dependencies flat-tiny silesia; do
  step "probe $workload"
  variants=($small); [ $workload = silesia ] && variants=($large)
  fresh=$(mktemp -d $W/scratch/probe.XXXXXX)
  bin/write_probe $C/$workload/$workload $fresh 7 $variants > $R/probe-$workload.json 2> $R/probe-$workload.err \
    || failed "probe $workload" $R/probe-$workload.err
  rmdir $fresh
done

for workload in dependencies flat-tiny silesia; do
  source_dir=$C/$workload/$workload
  if [ ! -s $R/matrix-$workload.json ]; then
    step "matrix $workload"
    python3 $S/extraction_matrix.py $source_dir --zipper bin/zipper-base --output $R/matrix-$workload.json \
      --work-dir $W/scratch --threads 1 4 --runs 7 --sevenzip 7zz --tar /usr/bin/bsdtar > $R/matrix-$workload.out 2> $R/matrix-$workload.err \
      || failed "matrix $workload" $R/matrix-$workload.err
  fi
  for kind in 7z nonsolid.7z; do
    step "7z $workload $kind"
    python3 $S/compare_zip_readers.py arch/$workload.$kind $source_dir --archive-format 7z --runs 7 --work-dir $W/scratch --allow-busy \
      --reader zipper-auto native bin/zipper-base --reader zipper-1 native bin/zipper-base --reader zipper-4 native bin/zipper-base \
      --reader 7zz-auto 7zip /opt/homebrew/bin/7zz --reader 7zz-1 7zip /opt/homebrew/bin/7zz \
      --reader-threads zipper-auto auto --reader-threads zipper-1 1 --reader-threads zipper-4 4 \
      --reader-threads 7zz-auto auto --reader-threads 7zz-1 1 > $R/7z-$workload-$kind.json 2> $R/7z-$workload-$kind.err \
      || failed "7z $workload $kind" $R/7z-$workload-$kind.err
  done
  step "ditto $workload"
  ALLOW_BUSY=1 python3 time_commands.py $S $source_dir $W/scratch 7 \
    "ditto=1:/usr/bin/ditto -x -k $W/arch/$workload.zip {out}" \
    "bsdtar=1:/usr/bin/bsdtar -xf $W/arch/$workload.zip -C {out}" \
    "zipper-1=0:$W/bin/zipper-base --threads 1 extract $W/arch/$workload.zip -o {out}" \
    "zipper-auto=0:$W/bin/zipper-base extract $W/arch/$workload.zip -o {out}" > $R/ditto-$workload.json 2> $R/ditto-$workload.err \
    || failed "ditto $workload" $R/ditto-$workload.err
done
grep -l '"busy_processes": \[\]' $R/7z-*.json $R/ditto-*.json 2>/dev/null | sed 's|^|no busy process recorded: |'
grep -h -A3 '"busy_processes"' $R/7z-*.json $R/ditto-*.json | grep '%' | grep -v fseventsd | sed 's|^|OTHER BUSY PROCESS IN A TIMED RUN: |'
echo "== done $(date +%H:%M:%S)"
