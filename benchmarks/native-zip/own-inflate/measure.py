#!/usr/bin/env python3
"""Per-round instructions and cycles of one decoder in the probe, from /usr/bin/time -l.

usage: measure.py BINARY DECODER[,DECODER...] ZIP [REPEATS] [LOW] [HIGH]
Each sample is (counters at HIGH rounds - counters at LOW rounds) / (HIGH - LOW);
the minimum over REPEATS is reported with the raw samples.
"""
import re
import subprocess
import sys


def counters(binary, decoder, rounds, archive):
    run = subprocess.run(
        ["/usr/bin/time", "-l", binary, decoder, str(rounds), archive],
        capture_output=True,
        text=True,
        check=True,
    )
    instructions = int(re.search(r"(\d+)\s+instructions retired", run.stderr).group(1))
    cycles = int(re.search(r"(\d+)\s+cycles elapsed", run.stderr).group(1))
    return instructions, cycles


def main():
    binary, decoders, archive = sys.argv[1:4]
    repeats = int(sys.argv[4]) if len(sys.argv) > 4 else 3
    low = int(sys.argv[5]) if len(sys.argv) > 5 else 1
    high = int(sys.argv[6]) if len(sys.argv) > 6 else 6
    samples = {decoder: [] for decoder in decoders.split(",")}
    for _ in range(repeats):
        for decoder in samples:
            a = counters(binary, decoder, low, archive)
            b = counters(binary, decoder, high, archive)
            samples[decoder].append(tuple((y - x) / (high - low) / 1e6 for x, y in zip(a, b)))
    for decoder, rows in samples.items():
        instructions = min(row[0] for row in rows)
        cycles = min(row[1] for row in rows)
        raw = " ".join(f"{row[1]:.1f}" for row in rows)
        print(f"{decoder:11} {instructions:9.1f} M instructions  {cycles:8.1f} M cycles  [cycles: {raw}]")


main()
