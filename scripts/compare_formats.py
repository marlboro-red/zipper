#!/usr/bin/env python3
"""Benchmark Zipper extracting classic formats against its actual 7-Zip backend."""
import argparse
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import tempfile
import time
from benchmark import timed
from compare_revisions import inventory, verify
from compare_zstd import sync_archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/release/zipper"))
    parser.add_argument("--sevenzip", default="7zz")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--threads", type=int, default=4, help="creation workers; backend extraction uses its defaults")
    parser.add_argument("--formats", nargs="+", choices=["zip", "7z", "tar"], default=["zip", "7z", "tar"])
    args = parser.parse_args()
    if args.runs < 1 or not 1 <= args.threads <= 64:
        parser.error("positive runs and threads in 1..64 required")
    backend = shutil.which(args.sevenzip)
    if backend is None:
        parser.error("7-Zip backend not found")
    binary = str(args.binary.resolve(strict=True))
    source = args.corpus.resolve(strict=True)
    expected = inventory(source)
    total = sum(size for size in expected.values() if size is not None)
    if not total:
        parser.error("corpus must contain nonempty files")
    os.environ["ZIPPER_7Z"] = backend
    samples = {kind: [] for kind in dict.fromkeys(args.formats)}
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(),
              "corpus": source.name, "input_bytes": total, "runs": args.runs, "warmup_rounds": 1,
              "creation_threads": args.threads, "extraction_threads": "7-Zip defaults for both paths",
              "backend": backend, "backend_version": subprocess.check_output([backend, "i"], text=True).splitlines()[:4],
              "creation": "7-Zip level 1 (tar stores); NOT native Zipper encoding",
              "archive_sync": "F_FULLFSYNC" if platform.system() == "Darwin" else "fsync",
              "verification": "direct byte comparison, sizes and tree membership after every extraction",
              "errors": 0, "load_average_start": os.getloadavg()}
    rng = random.Random(20261029)
    order = []
    with tempfile.TemporaryDirectory(prefix="zipper-formats-") as work:
        for iteration in range(args.runs + 1):
            formats = list(samples)
            rng.shuffle(formats)
            for kind in formats:
                archive = Path(work) / f"archive.{kind}"
                out = Path(work) / "out"
                options = [f"-t{kind}", f"-mmt{args.threads}"]
                if kind != "tar":
                    options += ["-mx=1"]
                if kind == "7z":
                    options += ["-m0=lzma2"]
                start = time.perf_counter()
                timed([backend, "a", *options, str(archive), source.name], source.parent)
                with archive.open("r+b") as stream:
                    sync_archive(stream)
                creation_seconds = time.perf_counter() - start
                decoders = ["zipper", "7zip-direct"]
                rng.shuffle(decoders)
                order.append({"round": iteration, "format": kind, "decoders": decoders})
                row = {"archive_bytes": archive.stat().st_size, "creation_seconds": creation_seconds}
                for decoder in decoders:
                    command = ([binary, "extract", str(archive), "-o", str(out)] if decoder == "zipper" else
                               [backend, "x", "-y", f"-o{out}", "--", str(archive)])
                    measurement = timed(command, source.parent)
                    verified = verify(source, out / source.name, expected)
                    if verified != total:
                        raise RuntimeError("verified byte count mismatch")
                    # Process-tree memory accounting differs; do not compare wrapper RSS.
                    row[decoder] = {"seconds": measurement["seconds"], "verified_bytes": verified}
                    shutil.rmtree(out)
                if iteration:
                    samples[kind].append(row)
                archive.unlink()
    result["order_including_warmup"] = order
    result["formats"] = {}
    for kind, rows in samples.items():
        if len({row["archive_bytes"] for row in rows}) != 1:
            raise RuntimeError("archive size changed across runs")
        summary = {"archive_bytes": rows[0]["archive_bytes"], "samples": rows}
        for decoder in ["zipper", "7zip-direct"]:
            seconds = [row[decoder]["seconds"] for row in rows]
            summary[decoder] = {"median_seconds": statistics.median(seconds),
                "min_seconds": min(seconds), "max_seconds": max(seconds),
                "mib_s": total / 2**20 / statistics.median(seconds)}
        result["formats"][kind] = summary
    result["load_average_end"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
