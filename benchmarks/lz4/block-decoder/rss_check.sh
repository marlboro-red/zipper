#!/bin/zsh
# Build this tree's release binary and print REGRESSED when the linked-frame repros pass the old code's peak by 1 MB.
S=${SCRATCH:?set SCRATCH to a work folder}
cargo build --release 2>&1 | tail -1
for archive in linked-raw-then-expand:16500000 random16-linked:15500000; do
  name=${archive%%:*}; limit=${archive#*:}
  rm -rf $S/out/rss
  peak=$(/usr/bin/time -l ./target/release/zipper --threads 1 extract $S/out/mem2/$name.lz4 -o $S/out/rss 2>&1 >/dev/null | awk '/maximum resident/ {print $1}')
  if [ $peak -gt $limit ]; then echo "$name $peak REGRESSED"; else echo "$name $peak within $limit"; fi
done
rm -rf $S/out/rss
