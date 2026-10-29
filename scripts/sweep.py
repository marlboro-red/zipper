#!/usr/bin/env python3
"""Interleaved native level/block-size sweep with direct byte verification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import tempfile
from benchmark import timed
from compare_revisions import inventory, verify


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/release/zipper"))
    parser.add_argument("--levels", type=int, nargs="+", default=[-3, 1, 3, 6])
    parser.add_argument("--blocks-kib", type=int, nargs="+", default=[64, 256, 1024, 4096, 16384])
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--runs", type=int, default=7)
    args = parser.parse_args()
    if args.runs < 1 or not 1 <= args.threads <= 64:
        parser.error("runs must be positive; threads must be 1..64")
    if any(not -7 <= level <= 22 for level in args.levels):
        parser.error("levels must be -7..22")
    if any(not 64 <= size <= 16384 or size & (size - 1) for size in args.blocks_kib):
        parser.error("block sizes must be powers of two in 64..16384 KiB")
    source = args.corpus.resolve(strict=True)
    binary = args.binary.resolve(strict=True)
    expected = inventory(source)
    total = sum(size for size in expected.values() if size is not None)
    if not total:
        parser.error("corpus must contain nonempty files")
    configurations = sorted(set((level, block) for level in args.levels for block in args.blocks_kib))
    samples = {f"level{level}-block{block}": [] for level, block in configurations}
    order = []
    rng = random.Random(20261029)
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "threads": args.threads, "runs": args.runs, "warmup_rounds": 1,
              "corpus": source.name, "input_bytes": total,
              "verification": "direct byte comparison, file sizes and tree membership",
              "errors": 0, "load_average_start": os.getloadavg()}
    with tempfile.TemporaryDirectory(prefix="zipper-sweep-") as work:
        for iteration in range(args.runs + 1):
            rng.shuffle(configurations)
            order.append([f"level{level}-block{block}" for level, block in configurations])
            for level, block in configurations:
                archive = Path(work) / "archive.zipper"
                out = Path(work) / "out"
                prefix = [str(binary), "--threads", str(args.threads)]
                compression = timed(prefix + ["create", str(archive), str(source), "--level", str(level),
                                               "--block-size-kib", str(block)], source.parent)
                extraction = timed(prefix + ["extract", str(archive), "-o", str(out)], source.parent)
                verified = verify(source, out / source.name, expected)
                if verified != total:
                    raise RuntimeError("verified byte count mismatch")
                if iteration:
                    samples[f"level{level}-block{block}"].append({"compression": compression,
                        "extraction": extraction, "archive_bytes": archive.stat().st_size,
                        "verified_bytes": verified})
                archive.unlink()
                shutil.rmtree(out)
    result["order_including_warmup"] = order
    result["configurations"] = {}
    for label, rows in samples.items():
        sizes = {row["archive_bytes"] for row in rows}
        if len(sizes) != 1:
            raise RuntimeError("archive size changed between runs")
        summary = {"archive_bytes": sizes.pop(), "samples": rows}
        for phase in ["compression", "extraction"]:
            seconds = [row[phase]["seconds"] for row in rows]
            summary[phase] = {"median_seconds": statistics.median(seconds),
                "min_seconds": min(seconds), "max_seconds": max(seconds),
                "mib_s": total / 2**20 / statistics.median(seconds),
                "peak_rss_bytes": max(row[phase]["peak_rss_bytes"] for row in rows)}
        result["configurations"][label] = summary
    result["load_average_end"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
