#!/usr/bin/env python3
"""Print one row per archive and tool from prof.py's JSON: the round with the median wall time."""
import json
import sys

GROUPS = {
    "create": ["open_create", "openat_create"],
    "write": ["write", "pwrite", "writev"],
    "close": ["close"],
    "times+modes": ["futimens", "utimensat", "utimes", "futimes", "lutimes", "fsetattrlist", "setattrlist",
                    "setattrlistat", "fchmod", "chmod", "fchmodat", "fchown", "chown", "lchown", "fchownat",
                    "fchflags", "chflags", "lchflags", "setxattr", "fsetxattr"],
    "lookups": ["stat", "lstat", "fstat", "fstatat", "getattrlist", "fgetattrlist", "getattrlistat", "access",
                "faccessat", "open_other", "openat_other", "getxattr", "fgetxattr", "readlink", "statfs", "fstatfs",
                "pathconf", "fpathconf", "getattrlistbulk"],
    "folders": ["mkdir", "mkdirat"],
    "publish": ["rename", "renameat", "renamex_np", "renameatx_np"],
    "archive reads": ["read", "pread", "lseek", "mmap", "munmap"],
    "waits": ["cond_wait", "cond_timedwait"],
}
rows = json.load(open(sys.argv[1]))
by_key = {}
for row in rows:
    if "trace" in row and row["exit"] == 0:
        by_key.setdefault((row["archive"], row["tool"]), []).append(row)
print("archive | tool | files | wall ms | user ms | system ms | " + " | ".join(f"{g} ms (calls)" for g in GROUPS)
      + " | other calls")
for (archive, tool), rounds in by_key.items():
    rounds.sort(key=lambda row: sum(t["wall_us"] for t in row["trace"]))
    row = rounds[len(rounds) // 2]
    calls = {}
    for trace in row["trace"]:
        for name, value in trace["calls"].items():
            total = calls.setdefault(name, {"count": 0, "us": 0})
            total["count"] += value["count"]
            total["us"] += value["us"]
    cells = []
    seen = set()
    for names in GROUPS.values():
        seen.update(names)
        cells.append(f"{sum(calls.get(n, {}).get('us', 0) for n in names) / 1000:.1f} "
                     f"({sum(calls.get(n, {}).get('count', 0) for n in names)})")
    other = {n: v["count"] for n, v in calls.items() if n not in seen and v["count"] > 20}
    wall = max(t["wall_us"] for t in row["trace"]) / 1000
    user = sum(t["user_us"] for t in row["trace"]) / 1000
    system = sum(t["system_us"] for t in row["trace"]) / 1000
    print(f"{archive} | {tool} | {row['files']} | {wall:.0f} | {user:.0f} | {system:.0f} | " + " | ".join(cells)
          + f" | {other}")
