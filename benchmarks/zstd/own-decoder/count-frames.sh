#!/bin/zsh
# count-frames.sh <file.zst> : run in a folder that holds the test binaries
# probe-shipped and probe-kept. Decodes the file once in memory with libzstd, with the
# shipped decoder and with the kept decoder, five times in turn, under
# /usr/bin/time -l. Prints instructions, cycles and peak memory of each run.
cd ${0:A:h} || exit 1
for repeat in 1 2 3 4 5; do
  for run in "probe-kept libzstd libzstd" "probe-shipped zipper shipped" "probe-kept zipper kept"; do
    parts=(${=run})
    out=$(ZSTD_PROBE=$1 ZSTD_PROBE_DECODER=$parts[2] ZSTD_PROBE_ROUNDS=1 \
      /usr/bin/time -l ./$parts[1] --ignored --exact zstandard::tests::decode_loop_for_counters --nocapture 2>&1)
    print -r -- "${1:t} $parts[3]" \
      "instructions $(print -r -- $out | awk '/instructions retired/ {print $1}')" \
      "cycles $(print -r -- $out | awk '/cycles elapsed/ {print $1}')" \
      "peak $(print -r -- $out | awk '/peak memory footprint/ {print $1}')"
  done
done
