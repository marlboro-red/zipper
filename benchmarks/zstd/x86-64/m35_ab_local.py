#!/usr/bin/env python3
"""Time Zipper's Zstandard decoder at a base build against a new build and libzstd on this machine.

The method is `scripts/m31_decoders.py decode`: one `zipper-decode-bench decode` process a sample,
which times the decode call alone and checks every byte, one warmup round, then rounds in a new
seeded order. It refuses to start while another process uses 25% of a core, and records each
process's peak RSS. Run it with PYTHONPATH set to the checkout's `scripts` folder.
"""
import argparse
import json
from pathlib import Path
import random
import statistics
import tempfile
from bench_common import refuse_when_busy, timed
from m31_decoders import built_source, checked_corpus


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--new", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=11)
    parser.add_argument("--allow-busy", action="store_true")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    busy = refuse_when_busy(args.allow_busy)
    corpus = args.corpus.resolve(strict=True)
    original = corpus / built_source(corpus)["tar"]
    manifest = checked_corpus(corpus / "streams", original)
    stream = corpus / "streams" / manifest["streams"]["zstd"]["file"]
    new = args.new.resolve(strict=True)
    rows = {"base": (args.base.resolve(strict=True), "zipper"), "new": (new, "zipper"), "libzstd": (new, "libzstd")}
    order = list(rows)
    shuffler = random.Random(35)
    samples = {name: [] for name in rows}
    peaks = {name: [] for name in rows}
    with tempfile.TemporaryDirectory(prefix="m35-ab-") as work:
        report = Path(work) / "row.json"
        for round_number in range(args.rounds + 1):
            shuffler.shuffle(order)
            for name in order:
                harness, decoder = rows[name]
                process = timed([str(harness), "decode", "zstd", decoder, str(stream), str(original), str(report)],
                                work)
                row = json.loads(report.read_text())
                if round_number > 0:
                    samples[name].append(row["seconds"])
                    peaks[name].append(process["peak_rss_bytes"])
    stats = {name: {"median": statistics.median(values), "min": min(values), "max": max(values), "samples": values,
                    "peak_rss_bytes": max(peaks[name])}
             for name, values in samples.items()}
    lines = ["| Row | Median ms | Lowest | Highest | Peak RSS |", "| --- | ---: | ---: | ---: | ---: |"]
    for name, row in stats.items():
        lines.append(f"| {name} | {row['median'] * 1000:.1f} | {row['min'] * 1000:.1f} | {row['max'] * 1000:.1f} "
                     f"| {row['peak_rss_bytes']:,} |")
    for first, second in (("new", "base"), ("new", "libzstd"), ("base", "libzstd")):
        ratio = stats[first]["median"] / stats[second]["median"]
        apart = stats[first]["max"] < stats[second]["min"] or stats[second]["max"] < stats[first]["min"]
        lines.append(f"\n{first} over {second}: {ratio:.3f}, ranges {'apart' if apart else 'overlap'}")
    args.out.write_text(json.dumps({"rounds": args.rounds, "busy_at_start": busy, "rows": stats}, indent=2) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
