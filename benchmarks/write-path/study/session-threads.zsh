set -u
source ${ZIPPER_CHECKOUT:?}/target/wpath/lock.zsh
cd $W
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

for workload in flat-tiny dependencies; do
  for kind in zip tar; do
    step "threads $workload $kind"
    python3 $S/compare_zip_readers.py arch/$workload.$kind $C/$workload/$workload --archive-format $kind --runs 7 --work-dir $W/scratch --allow-busy \
      --reader t2 native bin/zipper-base --reader t3 native bin/zipper-base --reader t4 native bin/zipper-base \
      --reader t5 native bin/zipper-base --reader auto native bin/zipper-base \
      --reader-threads t2 2 --reader-threads t3 3 --reader-threads t4 4 --reader-threads t5 5 --reader-threads auto auto \
      > $R/threads-$workload-$kind.json 2> $R/threads-$workload-$kind.err || failed "threads $workload $kind" $R/threads-$workload-$kind.err
  done
done
step "inside and outside"
python3 - $W > $R/inside-outside.json <<'EOF' || failed "inside and outside" /dev/null
import json, os, shutil, subprocess, sys, time
work = sys.argv[1]
rows = []
for archive in ["flat-tiny.zip", "dependencies.zip"]:
    for threads in ["1", "4"]:
        for round_index in range(8):
            out = f"{work}/scratch/inside-out"
            start = time.perf_counter()
            ran = subprocess.run([f"{work}/bin/zipper-phases", "--threads", threads, "extract", f"{work}/arch/{archive}", "-o", out],
                                 env=dict(os.environ, ZIPPER_PHASES="1"), capture_output=True, text=True, check=True)
            outside = (time.perf_counter() - start) * 1e3
            inside = sum(float(line.split()[2]) for line in ran.stderr.splitlines() if line.startswith("phase "))
            shutil.rmtree(out)
            if round_index:
                rows.append({"archive": archive, "threads": threads, "outside_ms": outside, "inside_ms": inside})
print(json.dumps(rows))
EOF
echo "== done $(date +%H:%M:%S)"
