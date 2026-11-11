#!/usr/bin/env python3
"""Interleave probe binaries and print each one's milliseconds a round.

usage: pm.py MODE ROUNDS WINDOW REPEATS ZIP NAME[,NAME...]
A sample is one process, which times ROUNDS decodes of every entry itself.
Prints the median, minimum and maximum over REPEATS, then every sample.
"""
import os
import random
import statistics
import subprocess
import sys

mode, rounds, window, repeats, archive, names = sys.argv[1:7]
here = os.path.dirname(os.path.abspath(__file__))
names = names.split(",")
samples = {name: [] for name in names}
for name in names:
    subprocess.run([f"{here}/probe-{name}", mode, "1", window, archive], capture_output=True, check=True)
for _ in range(int(repeats)):
    order = names[:]
    random.shuffle(order)
    for name in order:
        run = subprocess.run([f"{here}/probe-{name}", mode, rounds, window, archive],
                             capture_output=True, text=True, check=True)
        samples[name].append(float(run.stdout))
base = statistics.median(samples[names[0]])
for name in names:
    rows = samples[name]
    median = statistics.median(rows)
    print(f"{name:14} median {median:9.3f}  min {min(rows):9.3f}  max {max(rows):9.3f}  x{median / base:.4f}  "
          + " ".join(f"{row:.2f}" for row in rows))
