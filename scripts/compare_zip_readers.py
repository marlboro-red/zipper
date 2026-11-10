#!/usr/bin/env python3
"""Interleave native ZIP reader revisions on one immutable archive."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from process_metrics import timed
from bench_common import inventory, refuse_when_busy, verify


READER_MODES = ["native", "native-content-only", "7zip", "7zip-tar", "bsdtar", "sevenz-rust2", "pzip", "fastzip"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("source", type=Path, help="source directory stored as a top-level archive directory")
    parser.add_argument("--reader", action="append", nargs=3, metavar=("LABEL", "MODE", "BINARY"), required=True)
    parser.add_argument("--archive-format", choices=["zip", "native", "tar", "7z"], default="zip",
                        help="native, tar and 7z compare extraction by the default engine, without ZIP engine "
                             "flags; zip and 7z also accept 7-Zip itself as a `7zip` reader, and tar accepts "
                             "`7zip-tar` (7-Zip decoding into a second 7-Zip that reads the tar) and `bsdtar`; 7z also "
                             "accepts `sevenz-rust2`, the harness in benchmarks/lzma/sevenz-rust2/harness; zip also "
                             "accepts `native-content-only` (Zipper with --content-only), and the Go tools `pzip` "
                             "(its punzip CLI) and `fastzip` (a driver over its extractor), whose thread count "
                             "also sets GOMAXPROCS")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--reader-threads", action="append", nargs=2, metavar=("LABEL", "THREADS"), default=[],
                        help="override the worker count for one reader (or auto to omit --threads)")
    parser.add_argument("--operation", choices=["extract", "list", "test"], default="extract")
    parser.add_argument("--cooldown", type=float, default=0, help="untimed seconds after cleanup, 0..10")
    parser.add_argument("--work-dir", type=Path, help="parent for restored trees; the system temp folder by default")
    parser.add_argument("--allow-busy", action="store_true", help="for trials; the JSON lists the busy processes")
    args = parser.parse_args()
    busy = refuse_when_busy(args.allow_busy)
    if args.runs < 1 or not 1 <= args.threads <= 64 or not 0 <= args.cooldown <= 10:
        parser.error("positive runs and threads 1..64 required")
    readers = {}
    for label, mode, binary in args.reader:
        if label in readers or mode not in READER_MODES:
            parser.error(f"unique labels and modes from {READER_MODES} required")
        path = Path(binary).resolve(strict=True)
        readers[label] = {"mode": mode, "binary": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                          "threads": args.threads}
    allowed = {"zip": {"native", "native-content-only", "7zip", "pzip", "fastzip"}, "7z": {"native", "7zip", "sevenz-rust2"},
               "tar": {"native", "7zip-tar", "bsdtar"}}.get(args.archive_format, {"native"})
    if any(reader["mode"] not in allowed for reader in readers.values()):
        parser.error(f"{args.archive_format} comparisons accept {sorted(allowed)} readers")
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
        "runs": args.runs, "warmup_rounds": 1, "archive_format": args.archive_format, "operation": args.operation, "cooldown_seconds": args.cooldown,
        "source": source.name, "input_bytes": total,
        "archive_bytes": archive.stat().st_size, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "readers": readers, "verification": "direct bytes, file sizes and tree membership", "errors": 0,
        "busy_processes": busy,
        "load_average_start": (os.getloadavg() if hasattr(os, "getloadavg") else None)}
    samples = {label: [] for label in readers}
    order = []
    rng = random.Random(20261029)
    with tempfile.TemporaryDirectory(prefix="zipper-readers-", dir=args.work_dir) as work:
        for iteration in range(args.runs + 1):
            labels = list(readers)
            rng.shuffle(labels)
            order.append(labels)
            for label in labels:
                reader = readers[label]
                output = Path(work) / "out"
                prefix = [reader["binary"]]
                if args.archive_format == "zip" and reader["mode"].startswith("native"):
                    prefix += ["--zip-engine", "native"]
                if reader["threads"] is not None:
                    prefix += ["--threads", str(reader["threads"])]
                extract = prefix + ["extract", str(archive), "-o", str(output)]
                threads = [f"-mmt={reader['threads']}"] if reader["threads"] is not None else []
                if reader["mode"] == "7zip":
                    extract = [reader["binary"], "x", "-y", "-bd", "-bso0", "-snl", *threads,
                               f"-o{output}", str(archive)]
                elif reader["mode"] == "7zip-tar":
                    seven = shlex.quote(reader["binary"])
                    extract = ["/bin/sh", "-c", f"{seven} x -so -bd {' '.join(threads)} {shlex.quote(str(archive))}"
                               f" | {seven} x -si -ttar -y -bd -bso0 -snl {' '.join(threads)}"
                               f" -o{shlex.quote(str(output))}"]
                elif reader["mode"] == "sevenz-rust2":
                    extract = [reader["binary"], str(archive), str(output), str(reader["threads"] or "auto")]
                elif reader["mode"] == "native-content-only":
                    extract.append("--content-only")
                elif reader["mode"] in ("pzip", "fastzip"):
                    count = reader["threads"]
                    pinned = ["/usr/bin/env", f"GOMAXPROCS={count}"] if count is not None else []
                    workers = ["-concurrency", str(count)] if count is not None else []
                    extract = [*pinned, reader["binary"], *workers, "-d", str(output), str(archive)]
                elif reader["mode"] == "bsdtar":
                    output.mkdir()
                    extract = [reader["binary"], "-xf", str(archive), "-C", str(output)]
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
