#!/usr/bin/env python3
"""Compare complete Zipper and tar+Zstandard round trips (Unix tools required)."""
import argparse
import json
import os
import platform
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import tempfile
import time
from benchmark import manifest


def pipeline(commands, cwd, output=None):
    start = time.perf_counter()
    with tempfile.TemporaryFile() as errors:
        children = []
        previous = None
        for i, command in enumerate(commands):
            last = i == len(commands) - 1
            child = subprocess.Popen(command, cwd=cwd, stdin=previous,
                                     stdout=(output if output is not None else subprocess.DEVNULL) if last else subprocess.PIPE,
                                     stderr=errors)
            if previous is not None:
                previous.close()
            previous = child.stdout
            children.append(child)
        codes = [child.wait() for child in children]
        elapsed = time.perf_counter() - start
        if any(codes):
            errors.seek(0)
            raise RuntimeError(errors.read().decode(errors="replace"))
    return elapsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus_root", type=Path)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--threads", type=int, default=4)
    args = parser.parse_args()
    if args.runs < 1 or args.threads < 1:
        parser.error("runs and threads must be positive")
    zipper = str(Path("target/release/zipper").resolve(strict=True))
    engines = ["zipper", "tar+zstd", "tar+pzstd"]
    rng = random.Random(20261029)
    result = {"platform": platform.platform(), "threads": args.threads,
              "runs": args.runs, "level": 1, "warmup_rounds": 1, "corpora": {}}
    with tempfile.TemporaryDirectory(prefix="zipper-zstd-compare-") as work:
        for name in ["logs", "binaries", "random", "small-files"]:
            source = (args.corpus_root / name).resolve(strict=True)
            expected = manifest(source)
            total = sum(x[0] for x in expected.values() if x is not None)
            samples = {engine: [] for engine in engines}
            order = []
            for run in range(args.runs + 1):
                shuffled = engines.copy()
                rng.shuffle(shuffled)
                order.append(shuffled)
                for engine in shuffled:
                    archive = Path(work) / "archive"
                    out = Path(work) / "out"
                    if engine == "zipper":
                        create = [[zipper, "--threads", str(args.threads), "create", str(archive), str(source), "--level", "1"]]
                        extract = [[zipper, "--threads", str(args.threads), "extract", str(archive), "-o", str(out)]]
                        ct = pipeline(create, source.parent)
                    else:
                        tool = engine.split("+")[1]
                        threads = ["-T" + str(args.threads)] if tool == "zstd" else ["-p", str(args.threads)]
                        create = [["tar", "-cf", "-", source.name], [tool, "-q", "-1", *threads, "-c"]]
                        extract = [[tool, "-q", "-d", *threads, "-c", str(archive)], ["tar", "-xf", "-", "-C", str(out)]]
                        with archive.open("wb") as stream:
                            start = time.perf_counter()
                            ct = pipeline(create, source.parent, stream)
                            stream.flush()
                            os.fsync(stream.fileno())
                            ct = time.perf_counter() - start
                        out.mkdir()
                    dt = pipeline(extract, source.parent)
                    if manifest(out / source.name) != expected:
                        raise RuntimeError(f"round trip failed: {name} {engine}")
                    if run:
                        samples[engine].append({"compression_seconds": ct, "extraction_seconds": dt,
                                                "archive_bytes": archive.stat().st_size})
                    shutil.rmtree(out)
                    archive.unlink()
            summary = {}
            for engine in engines:
                rows = samples[engine]
                summary[engine] = {"compression_mib_s": total / 2**20 / statistics.median(s["compression_seconds"] for s in rows),
                                   "extraction_mib_s": total / 2**20 / statistics.median(s["extraction_seconds"] for s in rows),
                                   "archive_bytes": rows[0]["archive_bytes"], "samples": rows}
            result["corpora"][name] = {"input_bytes": total, "order_including_warmup": order, "engines": summary}
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
