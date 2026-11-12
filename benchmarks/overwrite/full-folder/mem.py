"""Peak memory of `zipper extract --overwrite` into a folder that already holds every file.

Usage: mem.py ARCHIVE WORK THREADS LABEL=BINARY...
Each binary runs once into its own prefilled copy, under /usr/bin/time -l.
"""
import os
import re
import shutil
import subprocess
import sys
import zipfile

archive, work, threads = sys.argv[1:4]
for pair in sys.argv[4:]:
    label, binary = pair.split("=", 1)
    out = os.path.join(work, "mem-" + label)
    if os.path.exists(out):
        subprocess.run(["chmod", "-R", "u+rwx", out])
        shutil.rmtree(out)
    os.makedirs(out)
    with zipfile.ZipFile(archive) as source:
        for name in source.namelist():
            path = os.path.join(out, name)
            if name.endswith("/"):
                os.makedirs(path, exist_ok=True)
                continue
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as old:
                old.write(b"old")
    done = subprocess.run(["/usr/bin/time", "-l", binary, "--threads", threads, "extract", "--zip-engine", "native",
                           "--overwrite", "-o", out, archive], capture_output=True, text=True)
    rss = int(re.search(r"(\d+)\s+maximum resident", done.stderr).group(1))
    left = [name for name in os.listdir(out) if name.startswith(".zipper-")]
    count = sum(len(files) for _, _, files in os.walk(out))
    print(f"{os.path.basename(archive)} threads {threads} {label}: exit {done.returncode} peak rss {rss / 1e6:.1f} MB "
          f"files {count} staging left {left}", flush=True)
    subprocess.run(["chmod", "-R", "u+rwx", out])
    shutil.rmtree(out)
