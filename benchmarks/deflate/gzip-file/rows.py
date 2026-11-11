"""Print one line a tool from result files of time_tools.py or scripts/compare_zip_readers.py.

Usage: rows.py RESULT.json...
"""
import json
import statistics
import sys
from pathlib import Path

for path in sys.argv[1:]:
    result = json.loads(Path(path).read_text())
    print(f"{Path(path).stem}: {result['runs']} runs, threads {result.get('threads')}")
    for name, row in result["results"].items():
        samples = row["samples"]
        cpu = statistics.median(sample["cpu_seconds"] for sample in samples)
        peak = max(sample["peak_rss_bytes"] for sample in samples)
        print(f"  {name:14} {row['median_seconds'] * 1000:7.1f} ms ({row['min_seconds'] * 1000:.1f} to "
              f"{row['max_seconds'] * 1000:.1f}), CPU {cpu * 1000:.1f} ms, peak {peak / 2**20:.1f} MiB")
