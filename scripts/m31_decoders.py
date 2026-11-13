#!/usr/bin/env python3
"""Measure M31: each decoder alone in one process, and extraction against references.

`corpus` writes a deterministic Silesia tar and one stream per codec with the decode bench in
benchmarks/decoders/decode-bench, plus an XPRESS WIM when wimlib-imagex is installed. `decode`
times every decoder the bench lists, one process per sample, over interleaved rounds after one
warmup. `extract` runs compare_zip_readers.py on each tar stream and the WIM. `all` runs both and
writes one JSON.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import shutil
import statistics
import subprocess
import sys
import tarfile
import tempfile
from bench_common import refuse_when_busy
from process_metrics import timed

ROOT = Path(__file__).resolve().parent.parent
SCHEMA = "m31-decoders/1"
MIN_ROUNDS = 7
ORDER_SEED = 20261114
SOURCE_NAME = "silesia"
TAR_NAME = "silesia.tar"
TAR_STREAMS = {"gzip": "gz", "zstd": "zst", "xz": "xz", "bzip2": "bz2"}
WIM_NAME = "silesia.wim"
WIM_CHUNK = 32768
STDERR_KEPT = 20000
SKIPPED_ROWS = {"lzma": "no extraction row: Zipper writes silesia.tar.lzma out as one file, silesia.tar, where "
                        "bsdtar reads the tar inside, so the trees differ; the decode rows still measure LZMA"}


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def command_output(command, cwd=None):
    try:
        return subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def normalised(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    info.mode = 0o755 if info.isdir() else 0o644
    return info


def deterministic_tar(source, archive):
    """A USTAR tar of `source` whose bytes depend only on the names and contents of its files."""
    with tarfile.open(archive, "w", format=tarfile.USTAR_FORMAT) as tar:
        tar.add(source, arcname=source.name, recursive=False, filter=normalised)
        for path in sorted(source.rglob("*")):
            if path.is_symlink():
                raise ValueError(f"symlink not supported in the corpus: {path}")
            name = f"{source.name}/{path.relative_to(source).as_posix()}"
            tar.add(path, arcname=name, recursive=False, filter=normalised)


def keep_only_the_source(tree, source, out):
    """wimlib-imagex captures all of `tree`, and compare_zip_readers.py expects it to hold `source`
    alone. fetch_silesia.py writes its manifest beside the folder it fetches, so it moves to `out`."""
    manifest = tree / f"{SOURCE_NAME}.manifest.json"
    if manifest.exists():
        manifest.replace(out / manifest.name)
    stray = sorted(path.name for path in tree.iterdir() if path != source)
    if stray:
        raise SystemExit(f"{tree} must hold only {source.name}, so the WIM holds the same tree; "
                         f"it also holds {', '.join(stray)}")


def corpus(args):
    out = args.out.resolve()
    tree = out / "tree"
    source = tree / SOURCE_NAME
    if not source.exists():
        tree.mkdir(parents=True, exist_ok=True)
        if args.silesia:
            shutil.copytree(args.silesia.resolve(strict=True), source)
        else:
            subprocess.run([sys.executable, str(ROOT / "scripts/fetch_silesia.py"), str(source)], check=True)
    keep_only_the_source(tree, source, out)
    archive = out / TAR_NAME
    deterministic_tar(source, archive)
    streams = out / "streams"
    subprocess.run([str(args.harness), "corpus", str(archive), str(streams)], check=True)
    wimlib = shutil.which("wimlib-imagex")
    wim = streams / WIM_NAME
    if wimlib:
        wim.unlink(missing_ok=True)
        subprocess.run([wimlib, "capture", str(tree), str(wim), "--compress=xpress",
                        f"--chunk-size={WIM_CHUNK}"], check=True, stdout=subprocess.DEVNULL)
    else:
        print("wimlib-imagex is not installed; the XPRESS extraction row is skipped", file=sys.stderr)
    print(f"{archive}: {archive.stat().st_size} bytes, sha256 {sha256_file(archive)}", file=sys.stderr)


def locked_versions():
    versions = {}
    for block in (ROOT / "Cargo.lock").read_text().split("[[package]]")[1:]:
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        version = re.search(r'^version = "([^"]+)"', block, re.M)
        if name and version:
            versions.setdefault(name.group(1), []).append(version.group(1))
    return {name: ", ".join(sorted(found)) for name, found in versions.items()}


def cpu_model():
    if platform.system() == "Darwin":
        return command_output(["sysctl", "-n", "machdep.cpu.brand_string"])
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or None


def memory_bytes():
    if platform.system() == "Darwin":
        value = command_output(["sysctl", "-n", "hw.memsize"])
        return int(value) if value else None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except OSError:
        pass
    return None


def header(trial):
    status = command_output(["git", "status", "--porcelain"], cwd=ROOT)
    return {
        "schema": SCHEMA,
        "recorded": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "trial": trial,
        "machine": {"platform": platform.platform(), "machine": platform.machine(), "cpu": cpu_model(),
                    "cpu_count": os.cpu_count(), "memory_bytes": memory_bytes(),
                    "runner": os.environ.get("RUNNER_NAME"), "runner_environment": os.environ.get("RUNNER_ENVIRONMENT")},
        "revision": {"commit": command_output(["git", "rev-parse", "HEAD"], cwd=ROOT),
                     "tree_clean": None if status is None else status == ""},
        "toolchain": {"rustc": command_output(["rustc", "-V"], cwd=ROOT), "rustflags": os.environ.get("RUSTFLAGS"),
                      "python": platform.python_version()},
        "load_average_start": os.getloadavg() if hasattr(os, "getloadavg") else None,
    }


def checked_corpus(streams, original):
    manifest = json.loads((streams / "corpus.json").read_text())
    if sha256_file(original) != manifest["original"]["sha256"]:
        raise RuntimeError(f"{original} is not the original corpus.json names")
    for codec, stream in manifest["streams"].items():
        if sha256_file(streams / stream["file"]) != stream["sha256"]:
            raise RuntimeError(f"the {codec} stream differs from corpus.json")
    return manifest


def summary(rows, size):
    seconds = [row["seconds"] for row in rows]
    cpu = [row["cpu_seconds"] for row in rows]
    median = statistics.median(seconds)
    median_cpu = None if None in cpu else statistics.median(cpu)
    return {"median_seconds": median, "min_seconds": min(seconds), "max_seconds": max(seconds),
            "median_cpu_seconds": median_cpu, "min_cpu_seconds": None if median_cpu is None else min(cpu),
            "max_cpu_seconds": None if median_cpu is None else max(cpu),
            "cpu_per_wall": None if median_cpu is None or not median else median_cpu / median,
            "mib_s": size / 2**20 / median if median else None,
            "peak_rss_bytes": max(row["peak_rss_bytes"] for row in rows)}


def decode(harness, streams, original, rounds, only):
    """Each decoder decodes its stream once per round in a fresh process, which times the decode
    alone; the process's peak RSS comes from the operating system."""
    harness = harness.resolve(strict=True)
    streams = streams.resolve(strict=True)
    original = original.resolve(strict=True)
    manifest = checked_corpus(streams, original)
    size = manifest["original"]["bytes"]
    listed = json.loads(subprocess.run([str(harness), "decoders"], capture_output=True, text=True,
                                       check=True).stdout)
    listed = [row for row in listed if not only or row["codec"] in only]
    if not listed:
        raise SystemExit("no decoder matches --only")
    samples = {(row["codec"], row["decoder"]): [] for row in listed}
    order = []
    rng = random.Random(ORDER_SEED)
    with tempfile.TemporaryDirectory(prefix="m31-decode-") as work:
        report = Path(work) / "report.json"
        for iteration in range(rounds + 1):
            pairs = list(samples)
            rng.shuffle(pairs)
            order.append([f"{codec} {decoder}" for codec, decoder in pairs])
            for codec, decoder in pairs:
                print(f"Round {iteration}: {codec} {decoder}", file=sys.stderr, flush=True)
                if report.exists():
                    report.unlink()
                stream = streams / manifest["streams"][codec]["file"]
                process = timed([str(harness), "decode", codec, decoder, str(stream), str(original), str(report)],
                                work)
                row = json.loads(report.read_text())
                if row["verified_bytes"] != size:
                    raise RuntimeError(f"{codec} {decoder} verified {row['verified_bytes']} of {size} bytes")
                if iteration:
                    samples[(codec, decoder)].append({
                        "seconds": row["seconds"], "cpu_seconds": row["cpu_seconds"],
                        "process_seconds": process["seconds"], "process_cpu_seconds": process["cpu_seconds"],
                        "peak_rss_bytes": process["peak_rss_bytes"], "verified_bytes": row["verified_bytes"],
                        "rayon_threads": row["rayon_threads"]})
    versions = locked_versions()
    results = {}
    for row in listed:
        rows = samples[(row["codec"], row["decoder"])]
        result = {"crates": {name: versions.get(name) for name in row["crates"]}, "api": row["api"],
                  "samples": rows}
        result.update(summary(rows, size))
        results.setdefault(row["codec"], {})[row["decoder"]] = result
    timing = {"rounds": rounds, "warmup_rounds": 1, "order_seed": ORDER_SEED, "order_including_warmup": order,
              "timed": "the decode call alone, inside its process, into an output buffer written before timing",
              "verification": "every decoded byte against the original, after timing", "results": results}
    corpus_record = {"original": manifest["original"], "streams": manifest["streams"],
                     "harness": {"path": str(harness), "sha256": sha256_file(harness)}}
    return timing, corpus_record


def compare(archive, source, archive_format, runs, readers, allow_busy):
    """The comparison's JSON, or an error entry that keeps the end of its stderr."""
    command = [sys.executable, str(ROOT / "scripts/compare_zip_readers.py"), str(archive), str(source),
               "--archive-format", archive_format, "--runs", str(runs), "--threads", "1", *readers]
    if allow_busy:
        command.append("--allow-busy")
    result = subprocess.run(command, capture_output=True, text=True)
    if not result.returncode:
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError as error:
            failure = f"compare_zip_readers.py printed no JSON: {error}"
    else:
        failure = f"compare_zip_readers.py exited with {result.returncode}"
    stderr = result.stderr[-STDERR_KEPT:]
    print(f"{archive.name}: {failure}\n{stderr}", file=sys.stderr, flush=True)
    return {"error": failure, "returncode": result.returncode, "stderr": stderr}


def extract(zipper, root, runs, allow_busy, only, section, save):
    """Fills `section` one row at a time and calls `save` after each, so a failed row is kept as an
    error entry and the rows before it stay on disk."""
    zipper = str(zipper.resolve(strict=True))
    root = root.resolve(strict=True)
    source = root / "tree" / SOURCE_NAME
    streams = root / "streams"
    wim = streams / WIM_NAME
    seven, bsdtar = shutil.which("7zz"), shutil.which("bsdtar")
    skipped = dict(SKIPPED_ROWS)
    if not wim.exists():
        skipped["xpress"] = "no silesia.wim, since wimlib-imagex was not installed for corpus"
    section.update({"runs": runs, "warmup_rounds": 1, "threads": 1,
                    "references_missing": [name for name, path in (("7zz", seven), ("bsdtar", bsdtar)) if not path],
                    "wim": "present" if wim.exists() else "absent, since wimlib-imagex was not installed for corpus",
                    "deflate": "gzip extraction covers Deflate; a raw Deflate stream has no container to extract",
                    "skipped": skipped, "failed": [], "results": {}})
    zipper_rows = ["--reader", "zipper", "native", zipper, "--reader", "zipper-all-threads", "native", zipper,
                   "--reader-threads", "zipper-all-threads", "auto"]
    rows = []
    for codec, extension in TAR_STREAMS.items():
        readers = list(zipper_rows)
        if bsdtar:
            readers += ["--reader", "bsdtar", "bsdtar", bsdtar]
        if seven:
            readers += ["--reader", "7zip", "7zip-tar", seven]
        rows.append((codec, streams / f"{TAR_NAME}.{extension}", "tar", readers))
    if wim.exists():
        seven_wim = ["--reader", "7zip", "7zip", seven] if seven else []
        rows.append(("xpress", wim, "native", list(zipper_rows) + seven_wim))
    for codec, archive, archive_format, readers in rows:
        if only and codec not in only:
            continue
        print(f"Extracting {archive.name}", file=sys.stderr, flush=True)
        row = compare(archive, source, archive_format, runs, readers, allow_busy)
        section["results"][codec] = row
        if "error" in row:
            section["failed"].append(codec)
        save()


def write(record, out):
    record["load_average_end"] = os.getloadavg() if hasattr(os, "getloadavg") else None
    text = json.dumps(record, indent=2) + "\n"
    if out:
        out.parent.mkdir(parents=True, exist_ok=True)
        partial = out.with_name(out.name + ".partial")
        partial.write_text(text)
        partial.replace(out)
    else:
        sys.stdout.write(text)


def check_rounds(parser, args, name):
    value = getattr(args, name)
    if value < 1 or (value < MIN_ROUNDS and not args.trial):
        parser.error(f"--{name} needs {MIN_ROUNDS} or more, or --trial for a smoke run")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    built = commands.add_parser("corpus", help="write the Silesia tar, its streams and the WIM")
    built.add_argument("--harness", type=Path, required=True, help="the zipper-decode-bench binary")
    built.add_argument("--out", type=Path, required=True)
    built.add_argument("--silesia", type=Path, help="an existing Silesia folder to copy instead of fetching it")
    for name in ("decode", "extract", "all"):
        command = commands.add_parser(name)
        if name != "extract":
            command.add_argument("--harness", type=Path, required=True, help="the zipper-decode-bench binary")
            command.add_argument("--rounds", type=int, default=MIN_ROUNDS, help="decode rounds after one warmup")
        if name != "decode":
            command.add_argument("--zipper", type=Path, required=True, help="the zipper release binary")
            command.add_argument("--runs", type=int, default=MIN_ROUNDS, help="extraction runs after one warmup")
        if name == "decode":
            command.add_argument("--corpus", type=Path, required=True, help="the folder `corpus` wrote streams into")
            command.add_argument("--original", type=Path, required=True)
        else:
            command.add_argument("--corpus", type=Path, required=True, help="the folder given to `corpus --out`")
        command.add_argument("--only", nargs="*", help="codecs to measure")
        command.add_argument("--trial", action="store_true", help="allow fewer rounds; the JSON says trial")
        command.add_argument("--allow-busy", action="store_true", help="for trials; the JSON lists the busy processes")
        command.add_argument("--out", type=Path)
    args = parser.parse_args()
    if args.command == "corpus":
        corpus(args)
        return
    for name in ("rounds", "runs"):
        if hasattr(args, name):
            check_rounds(parser, args, name)
    record = header(args.trial)
    record["busy_processes"] = refuse_when_busy(args.allow_busy)
    if args.command == "decode":
        record["decode"], record["corpus"] = decode(args.harness, args.corpus, args.original, args.rounds, args.only)
    if args.command == "all":
        record["decode"], record["corpus"] = decode(args.harness, args.corpus / "streams", args.corpus / TAR_NAME,
                                                    args.rounds, args.only)
    if args.command in ("extract", "all"):
        def save():
            if args.out:
                write(record, args.out)
        save()
        record["extract"] = {}
        extract(args.zipper, args.corpus, args.runs, args.allow_busy, args.only, record["extract"], save)
    write(record, args.out)
    failed = record.get("extract", {}).get("failed")
    if failed:
        raise SystemExit(f"extraction rows failed: {', '.join(failed)}; the JSON keeps each one's stderr")


if __name__ == "__main__":
    main()
