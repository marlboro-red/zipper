#!/bin/zsh
# On the second machine: peak resident size per binary and archive, runs interleaved. Args: label=binary ...
S=${SCRATCH:?set SCRATCH to a work folder}
cd $S/out
for archive in mem2/*.lz4 mem/*.lz4 hostile/*.lz4 ${=EXTRA}; do
  typeset -A peaks
  peaks=()
  for run in 1 2 3; do
    for pair in "$@"; do
      label=${pair%%=*}; binary=${pair#*=}
      rm -rf $S/out/m
      peak=$(/usr/bin/time -l $binary --threads 1 extract $archive -o $S/out/m 2>&1 >/dev/null | awk '/maximum resident/ {print $1}')
      peaks[$label]="${peaks[$label]} $peak"
    done
  done
  for pair in "$@"; do label=${pair%%=*}; echo "$archive $label${peaks[$label]}"; done
done
chmod -R u+rwx $S/out/m 2>/dev/null
rm -rf $S/out/m
