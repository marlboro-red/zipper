#!/bin/zsh
# Profile one full-folder run of bins/zipper-LABEL on the second machine, outside timed runs.
# Usage: profile.sh LABEL...   Writes prof-LABEL.json.gz and split-LABEL.txt.
set -e
cd "${0:A:h}"
base=${PWD:h:h:h}
until mkdir "$base"/target/remote-run.lock 2>/dev/null; do sleep 5; done
trap 'rmdir "$base"/target/remote-run.lock' EXIT
work=$(mktemp -d "$PWD/work.XXXXXX")
for label in "$@"; do
  mkdir $work/out-$label
  bins/zipper-$label extract --overwrite $PWD/small-files.zip -o $work/out-$label > /dev/null
  ${SAMPLY:-samply} record --save-only --unstable-presymbolicate -r 4000 -o prof-$label.json.gz -- bins/zipper-$label extract --overwrite $PWD/small-files.zip -o $work/out-$label > /dev/null 2>&1
  python3 -I split.py prof-$label.json.gz prof-$label.json.syms.json 14 > split-$label.txt
done
chmod -R u+rwx "$work"
rm -rf "${work:?}"
