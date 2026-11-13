#!/usr/bin/env python3
"""Run each extractor under syscount.dylib and record its file system calls.

Not a timing: the tracer adds two clock reads per call. It shows which calls a
tool makes per file and where its threads spend their time."""
import json
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

work, arch = Path(sys.argv[1]), Path(sys.argv[2])
rounds = int(sys.argv[3])
only = sys.argv[4:]
binaries, out = work / "bin", work / "out"
zipper = str(binaries / "zipper-base")
brew_tar = "/opt/homebrew/opt/libarchive/bin/bsdtar"


def remove(tree):
    for folder, _, _ in os.walk(tree):
        os.chmod(folder, stat.S_IRWXU)
    shutil.rmtree(tree)


def tools(archive, target):
    kind = archive.name.split(".", 1)[1]
    rows = {"zipper-auto": ([zipper, "extract", str(archive), "-o", str(target)], False),
            "zipper-1": ([zipper, "--threads", "1", "extract", str(archive), "-o", str(target)], False),
            "zipper-4": ([zipper, "--threads", "4", "extract", str(archive), "-o", str(target)], False),
            "zipper-content-only": ([zipper, "extract", str(archive), "-o", str(target), "--content-only"], False)}
    if kind in ("zip", "7z", "nonsolid.7z", "tar"):
        rows["7zz"] = (["7zz", "x", "-y", "-bd", "-bso0", "-snl", f"-o{target}", str(archive)], False)
        rows["7zz-mmt1"] = (["7zz", "x", "-y", "-bd", "-bso0", "-snl", "-mmt=1", f"-o{target}", str(archive)], False)
    if kind in ("zip", "tar", "tar.gz"):
        rows["bsdtar-3.8.6"] = ([brew_tar, "-xf", str(archive), "-C", str(target), "-b", "2048"], True)
        rows["bsdtar-system-copy"] = ([str(binaries / "bsdtar-system"), "-xf", str(archive), "-C", str(target),
                                       "-b", "2048"], True)
    if kind == "zip":
        rows["ditto-copy"] = ([str(binaries / "ditto-system"), "-x", "-k", str(archive), str(target)], True)
    return rows


results = []
for archive in sorted(arch.iterdir()):
    if only and archive.name not in only:
        continue
    for label, (command, make_target) in tools(archive, out / "tree").items():
        if not Path(command[0]).exists() and "/" in command[0]:
            continue
        for round_index in range(rounds):
            out.mkdir(parents=True, exist_ok=True)
            target = out / "tree"
            if make_target:
                target.mkdir()
            report = out / "report.json"
            environment = dict(os.environ, DYLD_INSERT_LIBRARIES=str(binaries / "syscount.dylib"),
                               SYSCOUNT_OUT=str(report))
            ran = subprocess.run(command, env=environment, capture_output=True, text=True)
            row = {"archive": archive.name, "tool": label, "round": round_index, "exit": ran.returncode}
            if report.exists():
                row["trace"] = [json.loads(line) for line in report.read_text().splitlines()]
            else:
                row["stderr"] = ran.stderr[-300:]
            row["files"] = sum(len(files) for _, _, files in os.walk(target)) if target.exists() else 0
            results.append(row)
            print(archive.name, label, round_index, ran.returncode, row["files"], "traced" if "trace" in row else "no trace",
                  file=sys.stderr, flush=True)
            remove(out)
print(json.dumps(results))
