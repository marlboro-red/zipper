#!/bin/zsh
# m2.sh <name>... : runs on the second machine, in ~/zstd-tune. Extracts
# zip-zstd.zip and silesia-3.tar.zst at one thread with each named binary,
# five times in turn, and prints median instructions and cycles of the
# whole process from /usr/bin/time -l. No wall time is reported.
cd ~/zstd-tune || exit 1
for archive in zip-zstd.zip silesia-3.tar.zst; do
  typeset -A cycles instructions
  cycles=() instructions=()
  for repeat in 1 2 3 4 5; do
    for name in "$@"; do
      into=$(mktemp -d ~/zstd-tune/out.XXXXXX)
      out=$(/usr/bin/time -l ./$name extract --threads 1 -o $into/o $archive 2>&1)
      instructions[$name]+="$(print -r -- $out | awk '/instructions retired/ {print $1}') "
      cycles[$name]+="$(print -r -- $out | awk '/cycles elapsed/ {print $1}') "
      chmod -R u+rwx ${into:?}
      rm -r ${into:?}
    done
  done
  for name in "$@"; do
    c=(${(on)=cycles[$name]})
    i=(${(on)=instructions[$name]})
    printf '%-18s %-10s instructions %7.1f M  cycles median %6.1f low %6.1f high %6.1f M\n' $archive $name $((i[3] / 1e6)) $((c[3] / 1e6)) $((c[1] / 1e6)) $((c[5] / 1e6))
  done
done
