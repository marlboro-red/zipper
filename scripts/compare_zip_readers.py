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
import tempfile
from benchmark import timed
from compare_revisions import inventory, verify


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("source", type=Path, help="source directory stored as a top-level archive directory")
    parser.add_argument("--reader", action="append", nargs=3, metavar=("LABEL", "MODE", "BINARY"), required=True)
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--operation", choices=["extract", "list", "test"], default="extract")
    args = parser.parse_args()
    if args.runs < 1 or not 1 <= args.threads <= 64:
        parser.error("positive runs and threads 1..64 required")
    readers = {}
    for label, mode, binary in args.reader:
        if label in readers or mode not in ["native", "native-fast"]:
            parser.error("unique labels and native/native-fast modes required")
        path = Path(binary).resolve(strict=True)
        readers[label] = {"mode": mode, "binary": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
    source = args.source.resolve(strict=True)
    archive = args.archive.resolve(strict=True)
    expected = inventory(source)
    total = sum(size for size in expected.values() if size is not None)
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(), "threads": args.threads,
        "runs": args.runs, "warmup_rounds": 1, "operation": args.operation, "source": source.name, "input_bytes": total,
        "archive_bytes": archive.stat().st_size, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "readers": readers, "verification": "direct bytes, file sizes and tree membership", "errors": 0,
        "load_average_start": os.getloadavg()}
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
                prefix = [reader["binary"], "--threads", str(args.threads), "--zip-engine", reader["mode"]]
                extract = prefix + ["extract", str(archive), "-o", str(output)]
                sample = timed(extract if args.operation == "extract" else
                               prefix + [args.operation, str(archive)], source.parent)
                if args.operation != "extract":
                    subprocess.run(extract, check=True, cwd=source.parent, stdout=subprocess.DEVNULL)
                sample["verified_bytes"] = verify(source, output / source.name, expected)
                if sample["verified_bytes"] != total:
                    raise RuntimeError("verified byte count mismatch")
                if iteration:
                    samples[label].append(sample)
                shutil.rmtree(output)
    result["order_including_warmup"] = order
    result["results"] = {}
    for label, rows in samples.items():
        seconds = [row["seconds"] for row in rows]
        result["results"][label] = {"samples": rows, "median_seconds": statistics.median(seconds),
            "min_seconds": min(seconds), "max_seconds": max(seconds),
            "mib_s": total / 2**20 / statistics.median(seconds),
            "peak_rss_bytes": max(row["peak_rss_bytes"] for row in rows)}
    result["load_average_end"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
