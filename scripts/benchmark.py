#!/usr/bin/env python3
"""Unix benchmark: verified round trips, wall time, child peak RSS, JSON output."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time


def manifest(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"symlink not supported in benchmark corpus: {path}")
        if path.is_file():
            with path.open("rb") as stream:
                hasher = hashlib.sha256()
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    hasher.update(block)
                digest = hasher.hexdigest()
            result[str(path.relative_to(root))] = (path.stat().st_size, digest)
        elif path.is_dir():
            result[str(path.relative_to(root))] = None
    return result


def timed(command, cwd):
    if platform.system() == "Linux":
        # A forked Python child can carry the harness's high RSS into wait4.
        # GNU time measures its own post-exec child, excluding that inherited peak.
        with tempfile.TemporaryFile() as log, tempfile.NamedTemporaryFile() as report:
            start = time.perf_counter()
            child = subprocess.run(["/usr/bin/time", "-f", "%M", "-o", report.name, "--"] + command,
                                   cwd=cwd, stdout=log, stderr=log)
            elapsed = time.perf_counter() - start
            if child.returncode:
                log.seek(0)
                raise RuntimeError(log.read().decode(errors="replace"))
            report.seek(0)
            rss = int(report.read().strip()) * 1024
        return {"seconds": elapsed, "peak_rss_bytes": rss}
    start = time.perf_counter()
    with tempfile.TemporaryFile() as log:
        child = subprocess.Popen(command, cwd=cwd, stdout=log, stderr=log)
        _, status, usage = os.wait4(child.pid, 0)
        child.returncode = os.waitstatus_to_exitcode(status)
        elapsed = time.perf_counter() - start
        if child.returncode:
            log.seek(0)
            raise RuntimeError(log.read().decode(errors="replace"))
    rss = usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
    return {"seconds": elapsed, "peak_rss_bytes": rss}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--zipper", type=Path, default=Path("target/release/zipper"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--level", type=int, default=1)
    parser.add_argument("--sevenzip", help="Optional 7zz/7z executable for LZMA2 comparison")
    args = parser.parse_args()
    if args.runs < 1 or args.threads < 1:
        parser.error("runs and threads must be positive")
    corpus = args.corpus.resolve(strict=True)
    if not corpus.is_dir():
        parser.error("corpus must be a directory")
    expected = manifest(corpus)
    total = sum(item[0] for item in expected.values() if item is not None)
    if not total:
        parser.error("corpus must contain nonempty files")
    zipper = str(args.zipper.resolve(strict=True))
    results = []
    engines = ["zipper"] + (["7zip-lzma2"] if args.sevenzip else [])
    sevenzip = shutil.which(args.sevenzip) if args.sevenzip else None
    if args.sevenzip and not sevenzip:
        parser.error("7-Zip executable was not found")
    with tempfile.TemporaryDirectory(prefix="zipper-bench-") as work:
        for engine in engines:
            samples = []
            for run in range(args.runs):
                archive = str(Path(work) / f"{engine}-{run}.archive")
                out = Path(work) / f"{engine}-{run}-out"
                if engine == "zipper":
                    create = [zipper, "--threads", str(args.threads), "create", archive, str(corpus), "--level", str(args.level)]
                    extract = [zipper, "--threads", str(args.threads), "extract", archive, "-o", str(out)]
                else:
                    create = [sevenzip, "a", "-t7z", "-m0=lzma2", "-mx=1", f"-mmt={args.threads}", archive, corpus.name]
                    extract = [sevenzip, "x", "-y", f"-mmt={args.threads}", f"-o{out}", archive]
                compression = timed(create, corpus.parent)
                extraction = timed(extract, corpus.parent)
                if manifest(out / corpus.name) != expected:
                    raise RuntimeError("round-trip verification failed")
                size = Path(archive).stat().st_size
                samples.append({"compression": compression, "extraction": extraction, "archive_bytes": size})
                shutil.rmtree(out)
                Path(archive).unlink()
            results.append({"engine": engine, "samples": samples,
                            "compression_mib_s": total / statistics.median(s["compression"]["seconds"] for s in samples) / 2**20,
                            "extraction_mib_s": total / statistics.median(s["extraction"]["seconds"] for s in samples) / 2**20,
                            "archive_to_input_ratio": samples[0]["archive_bytes"] / total})
    print(json.dumps({"platform": platform.platform(), "cpu_count": os.cpu_count(),
                      "threads": args.threads, "zipper_level": args.level,
                      "input_bytes": total, "corpus": str(corpus), "results": results}, indent=2))


if __name__ == "__main__":
    main()
