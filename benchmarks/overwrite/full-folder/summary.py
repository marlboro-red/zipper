"""Print medians, ranges and CPU from time_overwrite.py results, and full against empty per binary.

Usage: summary.py FULL.json EMPTY.json
"""
import json
import sys

full, empty = (json.load(open(path)) for path in sys.argv[1:3])
for result in (full, empty):
    print(result["mode"], "runs", result["runs"], "verified", result["verified"])
    for label, row in result["results"].items():
        rss = max(sample["peak_rss_bytes"] for sample in row["samples"])
        print(f"  {label:12} median {row['median_seconds']:.3f} s  range {row['min_seconds']:.3f} to "
              f"{row['max_seconds']:.3f}  cpu {row['median_cpu_seconds']:.3f} s  peak rss of children {rss / 2**20:.0f} MiB")
for label in full["results"]:
    ratio = full["results"][label]["median_seconds"] / empty["results"][label]["median_seconds"]
    print(f"  {label:12} full / empty = {ratio:.2f}")
