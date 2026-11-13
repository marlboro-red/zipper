#!/usr/bin/env python3
"""Interleave labelled extraction commands on one archive, verifying every byte.

For tools the repository's comparison scripts do not drive, such as `ditto`.
Usage: time_commands.py SCRIPTS SOURCE WORK RUNS LABEL=MKDIR:COMMAND...
`{out}` in a command is the output folder; MKDIR is 1 when the tool needs it made first."""
import json
import os
import platform
import random
import shlex
import shutil
import statistics
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, sys.argv[1])
from bench_common import inventory, refuse_when_busy, timed, verify  # noqa: E402

source, work, runs = Path(sys.argv[2]).resolve(strict=True), Path(sys.argv[3]), int(sys.argv[4])
commands = {}
for argument in sys.argv[5:]:
    label, _, rest = argument.partition("=")
    make, _, command = rest.partition(":")
    commands[label] = (make == "1", command)
busy = refuse_when_busy(os.environ.get("ALLOW_BUSY") == "1")
expected = inventory(source)
total = sum(size for size in expected.values() if size is not None)
wanted = {source.name: None}
wanted.update({str(Path(source.name) / key): value for key, value in expected.items()})
samples = {label: [] for label in commands}
orders = []
rng = random.Random(20261113)
with tempfile.TemporaryDirectory(prefix="zipper-commands-", dir=work) as folder:
    output = Path(folder) / "out"
    for round_index in range(runs + 1):
        order = list(commands)
        rng.shuffle(order)
        orders.append(order)
        for label in order:
            make, command = commands[label]
            if make:
                output.mkdir()
            row = timed(shlex.split(command.replace("{out}", str(output))), source.parent)
            if inventory(output) != wanted:
                raise RuntimeError(f"{label}: full output tree mismatch")
            row["verified_bytes"] = verify(source, output / source.name, expected)
            if round_index:
                samples[label].append(row)
            for path, _, _ in os.walk(output):
                os.chmod(path, 0o700)
            shutil.rmtree(output)
result = {"platform": platform.platform(), "cpu_count": os.cpu_count(), "source": source.name, "input_bytes": total,
          "runs": runs, "warmup_rounds": 1, "commands": {label: command for label, (_, command) in commands.items()},
          "busy_processes": busy, "order_including_warmup": orders, "load_average_end": os.getloadavg(),
          "verification": "direct bytes, file sizes and tree membership", "results": {}}
for label, rows in samples.items():
    seconds = [row["seconds"] for row in rows]
    result["results"][label] = {"samples": rows, "median_seconds": statistics.median(seconds),
                                "min_seconds": min(seconds), "max_seconds": max(seconds),
                                "median_cpu_seconds": statistics.median(row["cpu_seconds"] for row in rows),
                                "peak_rss_bytes": max(row["peak_rss_bytes"] for row in rows)}
print(json.dumps(result, indent=2))
