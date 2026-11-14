#!/bin/zsh
# On the second machine: peak resident size of each labelled binary on each hostile stream, `test` at one thread, interleaved.
S=${SCRATCH:?set SCRATCH to a work folder}
cd $S/hostile
for round in 1 2 3; do
  for input in clears.bin.Z longest.bin.Z random.bin.Z truncated.tar.Z zeros.bin.Z ../out/tar-compress.tar.Z; do
    for label in "$@"; do
      /usr/bin/time -l $S/out/zipper-$label --threads 1 test $input >/dev/null 2>$S/hostile/time.txt
      code=$?
      rss=$(grep -a 'maximum resident' $S/hostile/time.txt | awk '{print $1}')
      echo "$round $input $label exit=$code rss=$rss"
    done
  done
done
rm -f $S/hostile/time.txt
