#!/usr/bin/env python3
"""Create a deterministic flat directory of tiny mixed-size files for extraction tests."""
import argparse
import hashlib
import json
from pathlib import Path
import random


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
parser.add_argument("--files", type=int, default=10000)
parser.add_argument("--layout", choices=["flat", "grouped", "deep"], default="flat")
args = parser.parse_args()
if args.files < 1:
    parser.error("files must be positive")
args.output.mkdir(parents=True, exist_ok=False)
rng = random.Random(20261029)
rows = []
for index in range(args.files):
    size = [0, 32, 128, 512, 4096][index % 5]
    data = (rng.randbytes(size) if index % 7 == 0 else
            (f"record={index}; enabled=true; value=example\n".encode() * (size // 20 + 1))[:size])
    folder = args.output
    if args.layout == "grouped":
        folder /= f"group-{index // 100:04}"
    elif args.layout == "deep":
        folder = folder / f"group-{index // 100:04}" / "a" / "b" / "c" / "d"
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"record-{index:06}.dat"
    path.write_bytes(data)
    rows.append({"name": path.relative_to(args.output).as_posix(), "bytes": size, "sha256": hashlib.sha256(data).hexdigest()})
args.output.with_name(args.output.name + ".manifest.json").write_text(
    json.dumps({"seed": 20261029, "layout": args.layout, "files": rows}, indent=2) + "\n")
print(f"{len(rows)} files, {sum(row['bytes'] for row in rows)} bytes: {args.output}")
