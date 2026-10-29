#!/usr/bin/env python3
"""Create deterministic synthetic corpora plus a local macOS executable sample."""
import argparse
import hashlib
import json
from pathlib import Path
import random

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
parser.add_argument("--mib", type=int, default=256)
args = parser.parse_args()
if args.mib < 1:
    parser.error("mib must be positive")
root = args.output
rng = random.Random(20261029)
for name in ("logs", "random", "small-files", "binaries"):
    (root / name).mkdir(parents=True, exist_ok=False)
target = args.mib * 1024 * 1024
with (root / "logs/events.log").open("wb") as f:
    i = 0
    while f.tell() < target:
        record = {"ts": 1793232000 + i, "request": f"{rng.getrandbits(64):016x}",
                  "user": rng.randrange(10000), "route": rng.choice(["/api/items", "/login", "/search", "/health"]),
                  "status": rng.choice([200, 200, 200, 404, 500]), "latency_ms": rng.randrange(2000)}
        f.write((json.dumps(record) + "\n").encode())
        i += 1
with (root / "random/noise.bin").open("wb") as f:
    for _ in range(args.mib):
        f.write(rng.getrandbits(8 * 1024 * 1024).to_bytes(1024 * 1024, "little"))
for i in range(4000):
    folder = root / "small-files" / f"group-{i // 100:02}"
    folder.mkdir(exist_ok=True)
    text = (f"module={i}\n" + "configuration=value; enabled=true; path=/api/items\n" * 80).encode()
    (folder / f"config-{i:04}.txt").write_bytes(text)
sources = []
size = 0
with (root / "binaries/executables.bin").open("wb") as f:
    for path in sorted(Path("/usr/bin").iterdir()):
        if path.is_symlink() or not path.is_file():
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        f.write(data)
        sources.append({"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
        size += len(data)
        if size >= target:
            break
(root / "binary-sources.json").write_text(json.dumps(sources, indent=2) + "\n")
print(root)
