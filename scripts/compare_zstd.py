#!/usr/bin/env python3
"""Compare complete Zipper and tar+Zstandard round trips (Unix tools required)."""
import argparse
import fcntl
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


def sync_archive(stream):
    """Match Rust File::sync_all: F_FULLFSYNC on macOS, fsync elsewhere."""
    stream.flush()
    if platform.system() == "Darwin":
        fcntl.fcntl(stream.fileno(), fcntl.F_FULLFSYNC)
    else:
        os.fsync(stream.fileno())


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
            raise RuntimeError(f"pipeline failed {codes}: {commands}\n" + errors.read().decode(errors="replace"))
    return elapsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus_root", type=Path)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--corpus", action="append", help="corpus subdirectory; repeatable")
    parser.add_argument("--include-plain-tar", action="store_true")
    parser.add_argument("--tuned-tar", action="store_true", help="also test 1 MiB tar record buffers")
    args = parser.parse_args()
    if args.runs < 1 or args.threads < 1:
        parser.error("runs and threads must be positive")
    zipper = str(Path("target/release/zipper").resolve(strict=True))
    engines = ["zipper", "tar+zstd", "tar+pzstd"]
    if args.include_plain_tar:
        engines.append("tar")
    if args.tuned_tar:
        engines += [engine + "-1MiB" for engine in engines if engine != "zipper"]
    rng = random.Random(20261029)
    result = {"platform": platform.platform(), "threads": args.threads,
              "runs": args.runs, "level": 1, "warmup_rounds": 1,
              "archive_sync": "F_FULLFSYNC" if platform.system() == "Darwin" else "fsync",
              "compressed_tar_extraction": "--ignore-zeros consumes padding through decoder EOF",
              "tar_version": subprocess.check_output(["tar", "--version"], text=True).strip(),
              "corpora": {}}
    with tempfile.TemporaryDirectory(prefix="zipper-zstd-compare-") as work:
        for name in args.corpus or ["logs", "binaries", "random", "small-files"]:
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
                    base_engine = engine.removesuffix("-1MiB")
                    tar = ["tar", "-b", "2048"] if engine.endswith("-1MiB") else ["tar"]
                    archive = Path(work) / "archive"
                    out = Path(work) / "out"
                    if engine == "zipper":
                        create = [[zipper, "--threads", str(args.threads), "create", str(archive), str(source), "--level", "1"]]
                        extract = [[zipper, "--threads", str(args.threads), "extract", str(archive), "-o", str(out)]]
                        ct = pipeline(create, source.parent)
                    elif base_engine == "tar":
                        create = [tar + ["-cf", "-", source.name]]
                        extract = [tar + ["-xf", str(archive), "-C", str(out)]]
                        with archive.open("wb") as stream:
                            start = time.perf_counter()
                            pipeline(create, source.parent, stream)
                            sync_archive(stream)
                            ct = time.perf_counter() - start
                        out.mkdir()
                    else:
                        tool = base_engine.split("+")[1]
                        threads = ["-T" + str(args.threads)] if tool == "zstd" else ["-p", str(args.threads)]
                        create = [tar + ["-cf", "-", source.name], [tool, "-q", "-1", *threads, "-c"]]
                        # Consume the complete decompressed stream, including
                        # padded records, so the decoder finishes checksumming
                        # instead of receiving SIGPIPE after tar's zero blocks.
                        extract = [[tool, "-q", "-d", *threads, "-c", str(archive)], tar + ["--ignore-zeros", "-xf", "-", "-C", str(out)]]
                        with archive.open("wb") as stream:
                            start = time.perf_counter()
                            ct = pipeline(create, source.parent, stream)
                            sync_archive(stream)
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
