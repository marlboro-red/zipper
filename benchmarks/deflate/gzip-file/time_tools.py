"""Interleaved timing of decoding one archive with Zipper binaries and with other tools.

Usage: time_tools.py --plain PATH --archive FILE --work DIR --runs N
           [--zipper NAME=BINARY]... [--stdout NAME='COMMAND']... [--folder NAME='COMMAND']...
           [--setting NAME=VARIABLE=VALUE]... [--threads N] [--allow-busy] > result.json

PATH is the file or folder the archive must decode to. Every run writes a fresh output under DIR,
and the SHA-256 of each file must match PATH. Deleting the last output is outside the timed span.

- A Zipper binary runs as `BINARY extract ARCHIVE -o DIR/run-NAME`.
- A stdout tool runs as `COMMAND ARCHIVE > DIR/run-NAME.out`, and PATH must be a file.
- A folder tool runs COMMAND with `{archive}` and `{out}` replaced, and `{out}` is DIR/run-NAME.

Zipper and folder tools must leave PATH's name inside their output folder. Other files there,
such as a xar table of contents, are listed in the result and not compared.
"""
import argparse
import hashlib
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
from bench_common import refuse_when_busy  # noqa: E402


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def digests(path):
    if path.is_file():
        return {"": digest(path)}
    return {str(inner.relative_to(path)): digest(inner) for inner in sorted(path.rglob("*")) if inner.is_file()}


def run(command, sink, settings):
    start = time.perf_counter()
    child = subprocess.Popen(command, stdout=sink, stderr=subprocess.PIPE, env={**os.environ, **settings})
    _, status, usage = os.wait4(child.pid, 0)
    seconds = time.perf_counter() - start
    child.returncode = os.waitstatus_to_exitcode(status)
    if child.returncode:
        raise SystemExit(f"{command} failed: {child.stderr.read().decode(errors='replace')}")
    child.stderr.close()
    return {"seconds": seconds, "cpu_seconds": usage.ru_utime + usage.ru_stime,
            "user_seconds": usage.ru_utime, "system_seconds": usage.ru_stime, "peak_rss_bytes": usage.ru_maxrss}


def named(pairs):
    return dict(pair.split("=", 1) for pair in pairs)


def remove(path):
    if path.is_dir():
        for folder, _, _ in os.walk(path):
            os.chmod(folder, 0o700)
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


parser = argparse.ArgumentParser()
parser.add_argument("--plain", type=Path, required=True)
parser.add_argument("--archive", type=Path, required=True)
parser.add_argument("--work", type=Path, required=True)
parser.add_argument("--runs", type=int, default=15)
parser.add_argument("--zipper", action="append", default=[])
parser.add_argument("--stdout", action="append", default=[])
parser.add_argument("--folder", action="append", default=[])
parser.add_argument("--setting", action="append", default=[], metavar="NAME=VARIABLE=VALUE",
                    help="an environment variable for one tool, for an exploration build that reads one")
parser.add_argument("--threads")
parser.add_argument("--allow-busy", action="store_true")
arguments = parser.parse_args()
if arguments.runs < 7 and not arguments.allow_busy:
    raise SystemExit("use at least seven measured rounds")
busy = refuse_when_busy(arguments.allow_busy)
work = arguments.work.resolve()
work.mkdir(parents=True, exist_ok=True)
archive = arguments.archive.resolve()
plain = arguments.plain.resolve()
expected = digests(plain)
tools, hashes = {}, {}
for name, binary in named(arguments.zipper).items():
    binary = str(Path(binary).resolve())
    hashes[name] = digest(Path(binary))
    threads = ["--threads", arguments.threads] if arguments.threads else []
    tools[name] = ("folder", [binary, "extract", str(archive), "-o", str(work / f"run-{name}")] + threads)
for name, command in named(arguments.folder).items():
    tools[name] = ("folder", [part.replace("{archive}", str(archive)).replace("{out}", str(work / f"run-{name}"))
                              for part in shlex.split(command)])
for name, command in named(arguments.stdout).items():
    tools[name] = ("stdout", shlex.split(command) + [str(archive)])

settings = {name: {} for name in tools}
for name, pair in named(arguments.setting).items():
    variable, value = pair.split("=", 1)
    settings[name][variable] = value
samples = {name: [] for name in tools}
extras = {}
order = []
for round_number in range(arguments.runs + 1):
    shift = round_number % len(tools)
    names = list(tools)[shift:] + list(tools)[:shift]
    order.append(names)
    for name in names:
        kind, command = tools[name]
        folder, flat = work / f"run-{name}", work / f"run-{name}.out"
        remove(folder)
        remove(flat)
        if kind == "folder":
            sample = run(command, subprocess.DEVNULL, settings[name])
            produced = folder / plain.name
            extras[name] = sorted(path.name for path in folder.iterdir() if path.name != plain.name)
        else:
            with flat.open("wb") as sink:
                sample = run(command, sink, settings[name])
            produced = flat
        if not produced.exists() or digests(produced) != expected:
            raise SystemExit(f"{name} wrote different bytes")
        if round_number:
            samples[name].append(sample)
for name in tools:
    remove(work / f"run-{name}")
    remove(work / f"run-{name}.out")

result = {"archive": archive.name, "archive_bytes": archive.stat().st_size,
          "decoded_files": len(expected),
          "decoded_bytes": sum(path.stat().st_size for path in ([plain] if plain.is_file() else plain.rglob("*"))
                               if path.is_file()),
          "decoded_sha256": hashlib.sha256(json.dumps(expected, sort_keys=True).encode()).hexdigest()
          if len(expected) > 1 else expected[""],
          "runs": arguments.runs, "warmup_rounds": 1, "threads": arguments.threads or "default",
          "busy_at_start": busy, "binary_sha256": hashes, "settings": settings,
          "files_not_compared": extras,
          "commands": {name: [Path(command[0]).name] + [Path(part).name if os.sep in part else part
                                                        for part in command[1:]]
                       for name, (_, command) in tools.items()},
          "order_including_warmup": order, "results": {}}
for name, rows in samples.items():
    wall = [row["seconds"] for row in rows]
    result["results"][name] = {
        "median_seconds": statistics.median(wall), "min_seconds": min(wall), "max_seconds": max(wall),
        "median_cpu_seconds": statistics.median(row["cpu_seconds"] for row in rows),
        "median_user_seconds": statistics.median(row["user_seconds"] for row in rows),
        "median_system_seconds": statistics.median(row["system_seconds"] for row in rows),
        "max_peak_rss_bytes": max(row["peak_rss_bytes"] for row in rows),
        "samples": rows}
json.dump(result, sys.stdout, indent=1)
print(file=sys.stderr)
for name, row in result["results"].items():
    print(f"{name:14} {row['median_seconds'] * 1000:7.1f} [{row['min_seconds'] * 1000:.1f} to "
          f"{row['max_seconds'] * 1000:.1f}] cpu {row['median_cpu_seconds'] * 1000:6.1f} "
          f"(user {row['median_user_seconds'] * 1000:.1f} sys {row['median_system_seconds'] * 1000:.1f}) "
          f"peak {row['max_peak_rss_bytes'] / 2**20:.1f} MiB", file=sys.stderr)
