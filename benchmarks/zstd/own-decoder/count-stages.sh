#!/bin/zsh
# count-stages.sh <name>... : run in a folder that holds the named zipper
# binaries, zip-zstd.zip and silesia-3.tar.zst. Extracts each archive at one
# thread with each binary, once unrecorded and then seven times in turn, under
# /usr/bin/time -l. Prints one line a run with the retired instructions, the
# cycles and the peak memory footprint of the whole process, then the median,
# lowest and highest cycles of each binary. It reports no wall time.
cd ${0:A:h} || exit 1
for archive in zip-zstd.zip silesia-3.tar.zst; do
  typeset -A cycles instructions
  cycles=() instructions=()
  for repeat in 0 1 2 3 4 5 6 7; do
    for name in "$@"; do
      into=$(mktemp -d ${0:A:h}/out.XXXXXX)
      out=$(/usr/bin/time -l ./$name extract --threads 1 -o $into/o $archive 2>&1)
      i=$(print -r -- $out | awk '/instructions retired/ {print $1}')
      c=$(print -r -- $out | awk '/cycles elapsed/ {print $1}')
      if (( repeat > 0 )); then
        instructions[$name]+="$i "
        cycles[$name]+="$c "
        print -r -- "$archive $name instructions $i cycles $c peak $(print -r -- $out | awk '/peak memory footprint/ {print $1}')"
      fi
      chmod -R u+rwx ${into:?}
      rm -r ${into:?}
    done
  done
  for name in "$@"; do
    c=(${(on)=cycles[$name]})
    i=(${(on)=instructions[$name]})
    printf 'median %-18s %-12s instructions %7.1f M  cycles %6.1f low %6.1f high %6.1f M\n' $archive $name $((i[4] / 1e6)) $((c[4] / 1e6)) $((c[1] / 1e6)) $((c[7] / 1e6))
  done
done
