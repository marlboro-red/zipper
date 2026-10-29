#!/usr/bin/env python3
"""Interleave native ZIP reader revisions on one immutable archive."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from process_metrics import timed
from compare_revisions import inventory, verify


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("source", type=Path, help="source directory stored as a top-level archive directory")
    parser.add_argument("--reader", action="append", nargs=3, metavar=("LABEL", "MODE", "BINARY"), required=True)
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--reader-threads", action="append", nargs=2, metavar=("LABEL", "THREADS"), default=[],
                        help="override the worker count for one reader (or auto to omit --threads)")
    parser.add_argument("--operation", choices=["extract", "list", "test"], default="extract")
    parser.add_argument("--cooldown", type=float, default=0, help="untimed seconds after cleanup, 0..10")
    args = parser.parse_args()
    if args.runs < 1 or not 1 <= args.threads <= 64 or not 0 <= args.cooldown <= 10:
        parser.error("positive runs and threads 1..64 required")
    readers = {}
    for label, mode, binary in args.reader:
        if label in readers or mode not in ["native", "native-fast"]:
            parser.error("unique labels and native/native-fast modes required")
        path = Path(binary).resolve(strict=True)
        readers[label] = {"mode": mode, "binary": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                          "threads": args.threads}
    overridden = set()
    for label, value in args.reader_threads:
        if label not in readers or label in overridden or (value != "auto" and
                (not value.isdigit() or not 1 <= int(value) <= 64)):
            parser.error("reader thread overrides require a unique known label and count 1..64 or auto")
        readers[label]["threads"] = None if value == "auto" else int(value)
        overridden.add(label)
    source = args.source.resolve(strict=True)
    archive = args.archive.resolve(strict=True)
    expected = inventory(source)
    total = sum(size for size in expected.values() if size is not None)
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(), "threads": args.threads,
        "runs": args.runs, "warmup_rounds": 1, "operation": args.operation, "cooldown_seconds": args.cooldown,
        "source": source.name, "input_bytes": total,
        "archive_bytes": archive.stat().st_size, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "readers": readers, "verification": "direct bytes, file sizes and tree membership", "errors": 0,
        "load_average_start": (os.getloadavg() if hasattr(os, "getloadavg") else None)}
    samples = {label: [] for label in readers}
    order = []
    rng = random.Random(20261029)
    with tempfile.TemporaryDirectory(prefix="zipper-readers-") as work:
        for iteration in range(args.runs + 1):
            labels = list(readers)
            rng.shuffle(labels)
            order.append(labels)
            for label in labels:
                reader = readers[label]
                output = Path(work) / "out"
                prefix = [reader["binary"], "--zip-engine", reader["mode"]]
                if reader["threads"] is not None:
                    prefix += ["--threads", str(reader["threads"])]
                extract = prefix + ["extract", str(archive), "-o", str(output)]
                print(f"Round {iteration}: {label} ({args.operation})", file=sys.stderr, flush=True)
                sample = timed(extract if args.operation == "extract" else
                               prefix + [args.operation, str(archive)], source.parent)
                if args.operation != "extract":
                    subprocess.run(extract, check=True, cwd=source.parent, stdout=subprocess.DEVNULL)
                wanted = {source.name: None}
                wanted.update({str(Path(source.name) / key): value for key, value in expected.items()})
                if inventory(output) != wanted:
                    raise RuntimeError("full output tree mismatch")
                sample["verified_bytes"] = verify(source, output / source.name, expected)
                if sample["verified_bytes"] != total:
                    raise RuntimeError("verified byte count mismatch")
                if iteration:
                    samples[label].append(sample)
                shutil.rmtree(output)
                if args.cooldown:
                    time.sleep(args.cooldown)
    result["order_including_warmup"] = order
    result["results"] = {}
    for label, rows in samples.items():
        seconds = [row["seconds"] for row in rows]
        result["results"][label] = {"samples": rows, "median_seconds": statistics.median(seconds),
            "min_seconds": min(seconds), "max_seconds": max(seconds),
            "mib_s": total / 2**20 / statistics.median(seconds),
            "peak_rss_bytes": max(row["peak_rss_bytes"] for row in rows)}
    result["load_average_end"] = (os.getloadavg() if hasattr(os, "getloadavg") else None)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
