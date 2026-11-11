"""Where one process spends its time, from repeated `sample` runs and from tiny-input runs.

Usage: sample_split.py --work DIR --rounds N --name PROCESS_NAME [--tiny TINY_INPUT] -- COMMAND...

`{out}` in COMMAND becomes a fresh path under DIR. With `--stdout`, the command's stdout goes there.
It samples COMMAND N times at 1 ms with macOS `sample`, sums the samples by the function on top of
each stack, and prints each function's share. With `--tiny` it also times COMMAND 40 times on that
input, with the big input replaced, which gives process start-up plus the fixed work around a decode.
Run it outside timed runs.
"""
import argparse
import collections
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--work", type=Path, required=True)
parser.add_argument("--rounds", type=int, default=12)
parser.add_argument("--name", required=True)
parser.add_argument("--stdout", action="store_true")
parser.add_argument("--tiny", nargs=2, metavar=("BIG", "TINY"))
parser.add_argument("command", nargs="+")
arguments = parser.parse_args()
work = arguments.work.resolve()
work.mkdir(parents=True, exist_ok=True)
out = work / "split-out"


def clean():
    if out.is_dir():
        shutil.rmtree(out)
    out.unlink(missing_ok=True)


def run(command):
    clean()
    command = [part.replace("{out}", str(out)) for part in command]
    start = time.perf_counter()
    if arguments.stdout:
        with out.open("wb") as sink:
            subprocess.run(command, check=True, stdout=sink)
    else:
        subprocess.run(command, check=True, capture_output=True)
    return time.perf_counter() - start


top = collections.Counter()
for round_number in range(arguments.rounds):
    report = work / f"split-sample-{round_number}.txt"
    sampler = subprocess.Popen(["sample", arguments.name, "3", "1", "-wait", "-file", str(report)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.5)
    run(arguments.command)
    sampler.wait()
    text = report.read_text(errors="replace")
    section = text.split("Sort by top of stack, same collapsed (when >= 5):", 1)[1].split("Binary Images:", 1)[0]
    for line in section.splitlines():
        found = re.match(r"\s+(.*?)\s+(?:\(in (.*?)\)\s+)?(\d+)\s*$", line)
        if found:
            top[found.group(1)] += int(found.group(3))
    report.unlink()
total = sum(top.values())
print(f"samples {total} over {arguments.rounds} runs, functions with 5 or more samples in a run")
for name, count in top.most_common(25):
    print(f"{100 * count / total:6.1f}%  {count:6}  {name}")
if arguments.tiny:
    big, tiny = arguments.tiny
    command = [tiny if part == big else part for part in arguments.command]
    rows = [run(command) for _ in range(41)][1:]
    print(f"tiny input: median {statistics.median(rows) * 1000:.2f} ms, range {min(rows) * 1000:.2f} to "
          f"{max(rows) * 1000:.2f}, 40 runs")
clean()
