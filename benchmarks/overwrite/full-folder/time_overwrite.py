"""Interleaved timing of `zipper extract --overwrite` for two binaries.

Usage: time_overwrite.py MODE ARCHIVE SOURCE WORK RUNS LABEL=BINARY LABEL=BINARY > result.json

MODE full: the destination already holds every file, so each run replaces all of them.
MODE empty: the destination is an existing empty folder before each run.
The source must be the archive's top folder; the last run of each binary is compared
byte for byte with it.
"""
import filecmp
import json
import os
import resource
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

mode, archive, source, work, runs = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]), int(sys.argv[5])
readers = dict(arg.split("=", 1) for arg in sys.argv[6:])
work.mkdir(parents=True, exist_ok=True)


def destination(label):
    return work / f"out-{label}"


def clear(path):
    if path.exists():
        for root, dirs, _ in os.walk(path):
            for name in dirs:
                os.chmod(os.path.join(root, name), 0o755)
        shutil.rmtree(path)
    path.mkdir()


def run(label):
    out = destination(label)
    if mode == "empty":
        clear(out)
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    done = subprocess.run([readers[label], "extract", "--overwrite", str(archive), "-o", str(out)],
                          capture_output=True, text=True)
    seconds = time.perf_counter() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if done.returncode != 0:
        sys.exit(f"{label} failed: {done.stderr.strip()}")
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    return {"seconds": seconds, "cpu_seconds": cpu, "peak_rss_bytes": after.ru_maxrss}


def same_tree(left, right):
    compared = filecmp.dircmp(left, right)
    if compared.left_only or compared.right_only or compared.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(left, right, compared.common_files, shallow=False)
    return not mismatch and not errors and all(same_tree(left / d, right / d) for d in compared.common_dirs)


for label in readers:
    clear(destination(label))
    run(label)
samples = {label: [] for label in readers}
order = []
labels = list(readers)
for round_number in range(runs):
    sequence = labels if round_number % 2 == 0 else labels[::-1]
    order.append(sequence)
    for label in sequence:
        samples[label].append(run(label))
verified = {label: same_tree(destination(label) / source.name, source) for label in readers}
result = {"mode": mode, "archive_bytes": archive.stat().st_size, "runs": runs, "order": order,
          "verified": verified, "readers": readers, "results": {}}
for label, rows in samples.items():
    wall = [row["seconds"] for row in rows]
    result["results"][label] = {"samples": rows, "median_seconds": statistics.median(wall),
                                "min_seconds": min(wall), "max_seconds": max(wall),
                                "median_cpu_seconds": statistics.median(row["cpu_seconds"] for row in rows)}
json.dump(result, sys.stdout, indent=1)
if not all(verified.values()):
    sys.exit(f"tree mismatch: {verified}")
