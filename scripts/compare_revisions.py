#!/usr/bin/env python3
"""Interleave verified archive round trips across named Zipper executables."""
import argparse
import json
import os
from pathlib import Path
import platform
import random
import statistics
import tempfile
import shutil
from benchmark import timed


def inventory(root):
    result = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError(f"symlinks unsupported: {path}")
        result[str(path.relative_to(root))] = None if path.is_dir() else path.stat().st_size
    return result


def verify(source, restored, expected):
    if inventory(restored) != expected:
        raise RuntimeError("restored tree differs")
    verified = 0
    for name, size in expected.items():
        if size is None:
            continue
        with (source / name).open("rb") as a, (restored / name).open("rb") as b:
            while True:
                original = a.read(1024 * 1024)
                recovered = b.read(1024 * 1024)
                if original != recovered:
                    raise RuntimeError(f"restored bytes differ: {name}")
                verified += len(original)
                if not original:
                    break
    return verified


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus_root", type=Path)
    parser.add_argument("--binary", action="append", required=True, help="LABEL=EXECUTABLE")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--corpus", action="append")
    args = parser.parse_args()
    if args.runs < 1 or args.threads < 1:
        parser.error("runs and threads must be positive")
    binaries = {}
    for spec in args.binary:
        label, path = spec.split("=", 1)
        if label in binaries:
            parser.error("duplicate binary label")
        binaries[label] = str(Path(path).resolve(strict=True))
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(),
              "load_average_start": os.getloadavg(), "threads": args.threads,
              "binaries": binaries, "runs": args.runs, "warmup_rounds": 1,
              "verification": "direct byte comparison, tree membership and file sizes",
              "errors": 0, "corpora": {}}
    rng = random.Random(20261029)
    with tempfile.TemporaryDirectory(prefix="zipper-revisions-") as work:
        for name in args.corpus or ["logs", "binaries", "random", "small-files"]:
            source = (args.corpus_root / name).resolve(strict=True)
            expected = inventory(source)
            total = sum(size for size in expected.values() if size is not None)
            if not total:
                raise ValueError("corpus has no payload")
            samples = {label: [] for label in binaries}
            order = []
            for iteration in range(args.runs + 1):
                labels = list(binaries)
                rng.shuffle(labels)
                order.append(labels)
                for label in labels:
                    archive = Path(work) / "archive.zipper"
                    out = Path(work) / "out"
                    binary = binaries[label]
                    compression = timed([binary, "--threads", str(args.threads), "create", str(archive), str(source), "--level", "1"], source.parent)
                    extraction = timed([binary, "--threads", str(args.threads), "extract", str(archive), "-o", str(out)], source.parent)
                    verified = verify(source, out / source.name, expected)
                    if verified != total:
                        raise RuntimeError("verified byte count mismatch")
                    if iteration:
                        samples[label].append({"compression": compression, "extraction": extraction,
                                               "archive_bytes": archive.stat().st_size,
                                               "verified_bytes": verified})
                    archive.unlink()
                    shutil.rmtree(out)
            engines = {}
            for label, rows in samples.items():
                summary = {"archive_bytes": rows[0]["archive_bytes"], "samples": rows}
                for phase in ["compression", "extraction"]:
                    times = [row[phase]["seconds"] for row in rows]
                    summary[phase] = {"median_seconds": statistics.median(times),
                                      "min_seconds": min(times), "max_seconds": max(times),
                                      "mib_s": total / 2**20 / statistics.median(times),
                                      "peak_rss_bytes": max(row[phase]["peak_rss_bytes"] for row in rows)}
                engines[label] = summary
            result["corpora"][name] = {"input_bytes": total,
                                       "files": sum(size is not None for size in expected.values()),
                                       "order_including_warmup": order, "engines": engines}
    result["load_average_end"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
