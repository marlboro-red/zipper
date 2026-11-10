#!/bin/zsh
# count.sh <test-binary> <rounds> <file.zst>...
#
# Decodes each file in memory with libzstd and with Zipper's decoder, five
# times each in turn, under /usr/bin/time -l. Prints one line a run with the
# retired instructions, the cycles and the peak memory footprint. It reports
# no wall time, so a busy machine changes the cycles by a few percent and the
# instructions not at all.
binary=$1
rounds=$2
shift 2
for file in "$@"; do
  for repeat in 1 2 3 4 5; do
    for decoder in libzstd zipper; do
      out=$(ZSTD_PROBE=$file ZSTD_PROBE_DECODER=$decoder ZSTD_PROBE_ROUNDS=$rounds \
        /usr/bin/time -l $binary --ignored --exact zstandard::tests::decode_loop_for_counters --nocapture 2>&1)
      print -r -- "${file:t} $decoder" \
        "instructions $(print -r -- $out | awk '/instructions retired/ {print $1}')" \
        "cycles $(print -r -- $out | awk '/cycles elapsed/ {print $1}')" \
        "peak $(print -r -- $out | awk '/peak memory footprint/ {print $1}')"
    done
  done
done
