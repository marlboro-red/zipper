#!/usr/bin/env python3
"""Timing and verification helpers shared by the benchmark scripts."""
import hashlib
import os
import platform
import statistics
import subprocess
import tempfile
import time

def minimal_extract_flags(tar):
    """Flags that stop tar restoring metadata Zipper does not restore.

    Zipper restores file bytes and directories only. Comparisons pass these
    flags so external readers skip the same metadata work."""
    version = subprocess.run([tar, "--version"], capture_output=True, text=True).stdout
    flags = ["-m", "--no-same-owner", "--no-same-permissions", "--no-acls", "--no-xattrs"]
    if "bsdtar" in version:
        flags.append("--no-fflags")
        if platform.system() == "Darwin":
            flags.append("--no-mac-metadata")
    return flags


def cpu_cores(rows):
    """Median CPU seconds and CPU seconds per wall second of timed samples."""
    cpu = statistics.median(row["cpu_seconds"] for row in rows)
    return {"median_cpu_seconds": cpu,
            "cpu_per_wall": cpu / statistics.median(row["seconds"] for row in rows)}


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
            child = subprocess.run(["/usr/bin/time", "-f", "%M %U %S", "-o", report.name, "--"] + command,
                                   cwd=cwd, stdout=log, stderr=log)
            elapsed = time.perf_counter() - start
            if child.returncode:
                log.seek(0)
                raise RuntimeError(log.read().decode(errors="replace"))
            report.seek(0)
            rss, user, system = report.read().split()[-3:]
        return {"seconds": elapsed, "peak_rss_bytes": int(rss) * 1024,
                "cpu_seconds": float(user) + float(system)}
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
    return {"seconds": elapsed, "peak_rss_bytes": rss, "cpu_seconds": usage.ru_utime + usage.ru_stime}


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
