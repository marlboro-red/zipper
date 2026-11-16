#!/usr/bin/env python3
"""Cases for the table scripts/m35_ab.py writes; the `hooks` gate step runs them."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from m35_ab import ranges, table  # noqa: E402


def row(*seconds):
    return {"median_seconds": sorted(seconds)[len(seconds) // 2], "min_seconds": min(seconds),
            "max_seconds": max(seconds), "mib_s": 100.0, "samples": [{"seconds": s} for s in seconds]}


def record(results):
    return {"trial": False, "machine": {"cpu": "AMD EPYC 7763 64-Core Processor"},
            "revision": {"commit": "a" * 40},
            "ab": {"head_commit": "a" * 40, "base_commit": "b" * 40, "base_ref": "main", "base_label": "base"},
            "decode": {"rounds": 11, "order_seed": 7, "results": results}}


def check_ranges():
    assert ranges(row(1.0, 1.1, 1.2), row(1.3, 1.4, 1.5)) == "apart, head faster"
    assert ranges(row(1.6, 1.7, 1.8), row(1.3, 1.4, 1.5)) == "apart, head slower"
    assert ranges(row(1.0, 1.3, 1.4), row(1.3, 1.4, 1.5)) == "overlap"
    assert ranges(row(1.4, 1.45, 1.6), row(1.3, 1.4, 1.5)) == "overlap"
    assert ranges(row(1.2, 1.3, 2.0), row(1.4, 1.5, 1.6)) == "overlap"


def check_table():
    text = table(record({
        "zstd": {"zipper": row(1.0, 1.1, 1.2), "zipper@base": row(1.3, 1.4, 1.5), "libzstd": row(0.8, 0.9, 1.15)},
        "xz": {"zipper": row(2.0, 2.2, 2.4), "zipper@base": row(2.0, 2.2, 2.4), "liblzma": row(2.5, 2.6, 2.7)},
    }))
    lines = text.splitlines()
    assert "AMD EPYC 7763 64-Core Processor" in text, text
    assert "aaaaaaaaaaaa" in text and "bbbbbbbbbbbb" in text, text
    assert "| zstd | head over base | 0.786 | apart, head faster |" in lines, text
    assert "| zstd | head over libzstd | 1.222 | overlap |" in lines, text
    assert "| xz | head over base | 1.000 | overlap |" in lines, text
    assert "| xz | head over liblzma | 0.846 | apart, head faster |" in lines, text
    assert "| zstd | Zipper at base | 1.400 | 1.300 to 1.500 | 100.0 |" in lines, text
    assert "| zstd | libzstd | 0.900 | 0.800 to 1.150 | 100.0 |" in lines, text
    assert "trial" not in text.lower(), text
    trial = record({"lzma": {"zipper": row(1.0), "zipper@base": row(1.0), "liblzma": row(1.0)}})
    trial["trial"] = True
    assert "trial" in table(trial).lower()


if __name__ == "__main__":
    check_ranges()
    check_table()
    print("test_m35_ab: all cases passed")
