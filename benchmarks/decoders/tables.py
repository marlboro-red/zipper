#!/usr/bin/env python3
"""Print the M31 decode and extraction tables of one decoder family from the JSON in data/.

Every number in the M31 decoder records comes from this script. Run it with a family name, such as
`python3 benchmarks/decoders/tables.py zstandard`.
"""
import json
import statistics
import sys
from pathlib import Path

DATA = Path(__file__).resolve().parent / "data"
RUNS = [
    ("Silesia on the Apple M2 Max", ["m31-decoders-m2-max.json", "m31-lzma-decode-m2-max.json"]),
    ("Silesia on hosted Linux", ["m31-decoders-linux-x86_64-silesia.json"]),
    ("Dependencies on the Apple M2 Max", ["m31-decoders-m2-max-dependencies.json"]),
    ("Dependencies on hosted Linux", ["m31-decoders-linux-x86_64-dependencies.json"]),
]
FIRST_LINUX_SILESIA = ("Silesia on hosted Linux, first run", ["m31-decoders-linux-x86_64.json"])
SUMMARY_RUNS = RUNS[:2] + [FIRST_LINUX_SILESIA] + RUNS[2:]
FAMILIES = {
    "zstandard": {"decode": ["zstd"], "extract": ["zstd"]},
    "deflate-and-gzip": {"decode": ["deflate", "gzip"], "extract": ["gzip"]},
    "lzma-and-xz": {"decode": ["xz", "lzma"], "extract": ["xz"]},
    "bzip2": {"decode": ["bzip2"], "extract": ["bzip2"]},
    "xpress": {"decode": ["xpress"], "extract": ["xpress"]},
}
OWN = {"zipper", "zipper-all-threads"}
MIB = 1024 * 1024


def load(names):
    decode, extract, meta = {}, {}, []
    for name in names:
        run = json.loads((DATA / name).read_text())
        meta.append((name, run))
        decode.update(run.get("decode", {}).get("results", {}))
        extract.update(run.get("extract", {}).get("results", {}))
    return decode, extract, meta


def ms(seconds):
    return f"{seconds * 1000:.1f}"


def wall(row):
    return f"{ms(row['median_seconds'])} ({ms(row['min_seconds'])} to {ms(row['max_seconds'])})"


def ratio(own, other):
    median = own["median_seconds"] / other["median_seconds"]
    low = own["min_seconds"] / other["max_seconds"]
    high = own["max_seconds"] / other["min_seconds"]
    if own["max_seconds"] < other["min_seconds"]:
        verdict = "Zipper ahead"
    elif own["min_seconds"] > other["max_seconds"]:
        verdict = "Zipper behind"
    else:
        verdict = "ranges overlap"
    return median, low, high, verdict


def ratio_text(own, other):
    median, low, high, verdict = ratio(own, other)
    return f"{median:.3f} ({low:.3f} to {high:.3f})", verdict


def peak(row):
    return max(sample["peak_rss_bytes"] for sample in row["samples"]) / MIB


def cpu(row):
    if "median_cpu_seconds" in row:
        return row["median_cpu_seconds"]
    return statistics.median(sample["cpu_seconds"] for sample in row["samples"])


def decode_table(codec, rows):
    print(f"Decode of `{codec}`, median wall ms of the decode call with its range, CPU s and peak RSS:")
    print()
    print("| Decoder | Version | Wall ms | CPU s | Peak MiB | Zipper's time over it | Ranges |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    own = rows["zipper"]
    for name, row in rows.items():
        versions = ", ".join(f"`{crate}` {version}" for crate, version in row["crates"].items())
        compared, verdict = ("", "") if name in OWN else ratio_text(own, row)
        print(
            f"| `{name}` | {versions} | {wall(row)} | {cpu(row):.3f} | {peak(row):.1f} "
            f"| {compared} | {verdict} |"
        )
    print()


def extract_table(codec, run):
    rows = run["results"]
    print(
        f"Extraction of the `{codec}` row, {run['archive_bytes']:,} bytes into "
        f"{run['input_bytes']:,}, median wall ms with its range, CPU s and peak RSS:"
    )
    print()
    print("| Reader | Threads | Wall ms | CPU s | Peak MiB | Zipper one thread over it | Ranges |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    own = rows["zipper"]
    for name, row in rows.items():
        threads = run["readers"][name]["threads"] or "all"
        compared, verdict = ("", "") if name in OWN else ratio_text(own, row)
        print(
            f"| `{name}` | {threads} | {wall(row)} | {cpu(row):.3f} | {peak(row):.1f} "
            f"| {compared} | {verdict} |"
        )
    print()


def describe(meta, codecs):
    for name, run in meta:
        held = set(run["decode"]["results"]) | set(run.get("extract", {}).get("results", {}))
        if not codecs & held:
            continue
        print(
            f"- `{name}`, revision `{run['revision']['commit'][:8]}`, {run['machine']['cpu']} with "
            f"{run['machine']['cpu_count']} cores, {run['decode']['rounds']} decode rounds, "
            f"one-minute load {run['load_average_start'][0]:.2f} at the start, busy processes at "
            f"the start {len(run['busy_processes'])}"
        )
    print()


def summary(family):
    print(
        "Every decode comparison, Zipper's median time over the other decoder's with its range, "
        "in each run:"
    )
    print()
    print("| Stream | Against | Run | Zipper's time over it | Ranges |")
    print("| --- | --- | --- | --- | --- |")
    for codec in family["decode"]:
        for label, names in SUMMARY_RUNS:
            rows = load(names)[0].get(codec)
            if not rows:
                continue
            for other, row in rows.items():
                if other in OWN:
                    continue
                text, verdict = ratio_text(rows["zipper"], row)
                print(f"| `{codec}` | `{other}` | {label} | {text} | {verdict} |")
    print()


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in FAMILIES:
        sys.exit(f"usage: tables.py {'|'.join(FAMILIES)}")
    family = FAMILIES[sys.argv[1]]
    codecs = set(family["decode"]) | set(family["extract"])
    summary(family)
    for label, names in RUNS:
        decode, extract, meta = load(names)
        print(f"### {label}")
        print()
        describe(meta, codecs)
        for codec in family["decode"]:
            if codec in decode:
                decode_table(codec, decode[codec])
        for codec in family["extract"]:
            if codec in extract:
                extract_table(codec, extract[codec])
            else:
                print(f"No extraction row for `{codec}` in this run.")
                print()


if __name__ == "__main__":
    main()
