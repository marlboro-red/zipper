#!/usr/bin/env python3
"""Benchmark Zipper extracting classic formats against its actual 7-Zip backend."""
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
    parser.add_argument("--level", type=int, default=1, choices=range(1, 10))
    parser.add_argument("--threads", type=int, default=4, help="creation workers; backend extraction uses its defaults")
    parser.add_argument("--formats", nargs="+", choices=["zip", "7z", "tar"], default=["zip", "7z", "tar"])
    parser.add_argument("--native-zip", action="store_true", help="add Zipper's opt-in native ZIP reader")
    parser.add_argument("--native-fast", action="store_true", help="also measure the buffered native ZIP reader")
    parser.add_argument("--native-auto", action="store_true", help="omit --threads for native extraction")
    parser.add_argument("--reuse-archives", action="store_true", help="create once per format; benchmark extraction only")
    parser.add_argument("--tar-reader", help="bsdtar executable capable of extracting ZIP; also test 1 MiB input buffering")
    parser.add_argument("--tar-minimal", action="store_true", help="also test bsdtar without restoring timestamps or optional metadata")
    args = parser.parse_args()
    if args.runs < 1 or not 1 <= args.threads <= 64:
        parser.error("positive runs and threads in 1..64 required")
    backend = shutil.which(args.sevenzip)
    if backend is None:
        parser.error("7-Zip backend not found")
    binary = str(args.binary.resolve(strict=True))
    tar_reader = shutil.which(args.tar_reader) if args.tar_reader else None
    if args.tar_reader and tar_reader is None:
        parser.error("tar reader not found")
    if args.tar_minimal and tar_reader is None:
        parser.error("--tar-minimal requires --tar-reader")
    minimal_flags = ["-m", "--no-same-owner", "--no-same-permissions", "--no-acls", "--no-fflags", "--no-xattrs"]
    if platform.system() == "Darwin":
        minimal_flags.append("--no-mac-metadata")
    source = args.corpus.resolve(strict=True)
    expected = inventory(source)
    total = sum(size for size in expected.values() if size is not None)
    if not total:
        parser.error("corpus must contain nonempty files")
    os.environ["ZIPPER_7Z"] = backend
    samples = {kind: [] for kind in dict.fromkeys(args.formats)}
    result = {"platform": platform.platform(), "cpu_count": os.cpu_count(),
              "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
              "corpus": source.name, "input_bytes": total, "runs": args.runs, "warmup_rounds": 1,
              "creation_threads": args.threads, "extraction_threads": "7-Zip defaults for both paths",
              "backend": backend, "backend_version": subprocess.check_output([backend, "i"], text=True).splitlines()[:4],
              "creation": f"7-Zip level {args.level} (tar stores); NOT native Zipper encoding",
              "level": args.level,
              "archive_creation": "once per format before extraction samples" if args.reuse_archives else "once per format per round",
              "archive_sync": "F_FULLFSYNC" if platform.system() == "Darwin" else "fsync",
              "verification": "direct byte comparison, sizes and tree membership after every extraction",
              "errors": 0, "load_average_start": os.getloadavg()}
    result["native_zip_threads"] = args.threads if (args.native_zip or args.native_fast) and not args.native_auto else None
    result["native_zip_concurrency"] = "automatic" if args.native_auto else "explicit"
    if tar_reader:
        result["tar_reader"] = tar_reader
        result["tar_version"] = subprocess.check_output([tar_reader, "--version"], text=True).strip()
        result["tar_minimal_flags"] = minimal_flags if args.tar_minimal else None
    rng = random.Random(20261029)
    order = []
    preparations = {}
    with tempfile.TemporaryDirectory(prefix="zipper-formats-") as work:
        for iteration in range(args.runs + 1):
            formats = list(samples)
            rng.shuffle(formats)
            for kind in formats:
                archive = Path(work) / f"archive.{kind}"
                out = Path(work) / "out"
                options = [f"-t{kind}", f"-mmt{args.threads}"]
                if kind != "tar":
                    options += [f"-mx={args.level}"]
                if kind == "7z":
                    options += ["-m0=lzma2"]
                creation_seconds = None
                if not archive.exists():
                    start = time.perf_counter()
                    timed([backend, "a", *options, str(archive), source.name], source.parent)
                    with archive.open("r+b") as stream:
                        sync_archive(stream)
                    creation_seconds = time.perf_counter() - start
                    if args.reuse_archives:
                        preparations[kind] = {"creation_seconds": creation_seconds,
                            "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}
                decoders = ["zipper", "7zip-direct"]
                if kind == "zip" and (args.native_zip or args.native_fast):
                    decoders.append("zipper-native")
                if kind == "zip" and args.native_fast:
                    decoders.append("zipper-native-fast")
                if tar_reader:
                    decoders += ["bsdtar", "bsdtar-1MiB"]
                    if args.tar_minimal:
                        decoders.append("bsdtar-minimal")
                rng.shuffle(decoders)
                order.append({"round": iteration, "format": kind, "decoders": decoders})
                row = {"archive_bytes": archive.stat().st_size, "creation_seconds": creation_seconds}
                for decoder in decoders:
                    command = ([binary, "extract", str(archive), "-o", str(out)] if decoder == "zipper" else
                               [backend, "x", "-y", f"-o{out}", "--", str(archive)])
                    if decoder == "zipper-native":
                        command = [binary, *([] if args.native_auto else ["--threads", str(args.threads)]), "--zip-engine", "native",
                                   "extract", str(archive), "-o", str(out)]
                    if decoder == "zipper-native-fast":
                        command = [binary, *([] if args.native_auto else ["--threads", str(args.threads)]), "--zip-engine", "native-fast",
                                   "extract", str(archive), "-o", str(out)]
                    if decoder.startswith("bsdtar"):
                        out.mkdir()
                        command = [tar_reader, *(["-b", "2048"] if decoder in ["bsdtar-1MiB", "bsdtar-minimal"] else []),
                                   *(minimal_flags if decoder == "bsdtar-minimal" else []),
                                   "-xf", str(archive), "-C", str(out)]
                    measurement = timed(command, source.parent)
                    verified = verify(source, out / source.name, expected)
                    if verified != total:
                        raise RuntimeError("verified byte count mismatch")
                    # Process-tree memory accounting differs; do not compare wrapper RSS.
                    row[decoder] = {"seconds": measurement["seconds"], "verified_bytes": verified}
                    if decoder.startswith("zipper-native"):
                        row[decoder]["peak_rss_bytes"] = measurement["peak_rss_bytes"]
                    shutil.rmtree(out)
                if iteration:
                    samples[kind].append(row)
                if not args.reuse_archives:
                    archive.unlink()
    result["preparations"] = preparations
    result["order_including_warmup"] = order
    result["formats"] = {}
    for kind, rows in samples.items():
        if len({row["archive_bytes"] for row in rows}) != 1:
            raise RuntimeError("archive size changed across runs")
        summary = {"archive_bytes": rows[0]["archive_bytes"], "samples": rows}
        decoders = ["zipper", "7zip-direct"]
        if kind == "zip" and (args.native_zip or args.native_fast): decoders.append("zipper-native")
        if kind == "zip" and args.native_fast: decoders.append("zipper-native-fast")
        if tar_reader: decoders += ["bsdtar", "bsdtar-1MiB"]
        if args.tar_minimal: decoders.append("bsdtar-minimal")
        for decoder in decoders:
            seconds = [row[decoder]["seconds"] for row in rows]
            summary[decoder] = {"median_seconds": statistics.median(seconds),
                "min_seconds": min(seconds), "max_seconds": max(seconds),
                "mib_s": total / 2**20 / statistics.median(seconds)}
            if decoder.startswith("zipper-native"):
                summary[decoder]["peak_rss_bytes"] = max(row[decoder]["peak_rss_bytes"] for row in rows)
        result["formats"][kind] = summary
    result["load_average_end"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
