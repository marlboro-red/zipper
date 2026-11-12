#!/bin/zsh
# Timing session, run from <checkout>/target/timing/<folder>/ beside the scripts, small-files.zip and source/tree.
# Usage: session.sh NAME RUNS LABEL...
# Each LABEL names bins/zipper-LABEL. Writes NAME-full.json, NAME-empty.json and NAME-session.txt.
set -e
cd "${0:A:h}"
base=${PWD:h:h:h}
name=$1; runs=$2; shift 2
until mkdir "$base"/target/remote-run.lock 2>/dev/null; do sleep 5; done
trap 'rmdir "$base"/target/remote-run.lock' EXIT
{ uptime; ps -Ao pcpu,comm -r | head -5; sw_vers; sysctl -n machdep.cpu.brand_string hw.ncpu; } > $name-session.txt
labels=()
for label in "$@"; do
  shasum -a 256 bins/zipper-$label >> $name-session.txt
  labels+=($label=$PWD/bins/zipper-$label)
done
shasum -a 256 small-files.zip >> $name-session.txt
work=$(mktemp -d "$PWD/work.XXXXXX")
python3 -I time_overwrite.py full $PWD/small-files.zip $PWD/source/tree $work/full $runs $labels > $name-full.json
python3 -I time_overwrite.py empty $PWD/small-files.zip $PWD/source/tree $work/empty $runs $labels > $name-empty.json
uptime >> $name-session.txt
chmod -R u+rwx "$work"
rm -rf "${work:?}"
python3 -I summary.py $name-full.json $name-empty.json
