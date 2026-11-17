#!/usr/bin/env python3
"""Profile M35: Zipper's Zstandard, xz and LZMA decoders against libzstd and liblzma under perf.

For each codec, Zipper's decoder and the C library's decode the corpus stream `--repeats` times in
one process with `zipper-decode-bench repeat`. Each runs under `perf stat` twice, once with the
repeats and once with the setup alone, so `summary.md` divides the difference by the repeats to
give the counts of one decode. Each then runs under `perf record -g`, and the folder gets
`perf report` for both decoders and `perf annotate` of Zipper's top symbols. This is a profile,
not a timing: `scripts/m31_decoders.py` measures time.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
from m31_decoders import built_source, checked_corpus

CODECS = {"zstd": "libzstd", "xz": "liblzma", "lzma": "liblzma"}
EVENTS = ("task-clock", "cycles", "instructions", "branches", "branch-misses", "L1-dcache-load-misses")
ANNOTATED = 3
PMU_PREFIX = re.compile(r"^\w+/(.+)/$")
REPORT_LINE = re.compile(r"^\s*([0-9.]+)%\s+\[\.\]\s+(.+?)(?:\s{2,}.*)?$")


def run(command, stdout=None):
    print("+ " + " ".join(str(part) for part in command), file=sys.stderr, flush=True)
    subprocess.run([str(part) for part in command], check=True, stdout=stdout)


def bench(harness, codec, decoder, stream, original, repeats):
    return [harness, "repeat", codec, decoder, stream, original, str(repeats)]


def stat_counts(path):
    """Each event's count from `perf stat -x,` output, or None when perf could not count it."""
    counts = {}
    for line in path.read_text().splitlines():
        fields = line.split(",")
        if line.startswith("#") or len(fields) < 3:
            continue
        name = PMU_PREFIX.sub(r"\1", fields[2].split(":")[0])
        if name in EVENTS:
            try:
                counts[name] = float(fields[0])
            except ValueError:
                counts[name] = None
    return counts


def per_decode(full, setup, repeats):
    rows = {}
    for event in EVENTS:
        whole, alone = full.get(event), setup.get(event)
        rows[event] = None if whole is None or alone is None else (whole - alone) / repeats
    return rows


def top_symbols(perf, data, limit):
    listing = subprocess.run([perf, "report", "-i", str(data), "--no-children", "--sort", "symbol", "--stdio",
                              "-q", "-g", "none", "--percent-limit", "1"],
                             check=True, capture_output=True, text=True).stdout
    symbols = []
    for line in listing.splitlines():
        found = REPORT_LINE.match(line)
        if found:
            symbols.append((float(found.group(1)), found.group(2)))
    return symbols[:limit]


def profile(args, codec, decoder, manifest):
    out = args.out
    stream = args.corpus / "streams" / manifest["streams"][codec]["file"]
    original = args.corpus / built_source(args.corpus)["tar"]
    name = f"{codec}-{decoder}"
    counted = {}
    for label, repeats in (("setup", 0), ("decode", args.repeats)):
        csv = out / f"stat-{name}-{label}.csv"
        with (out / f"repeat-{name}-{label}.json").open("w") as row:
            run([args.perf, "stat", "-x", ",", "-e", ",".join(EVENTS), "-o", csv, "--",
                 *bench(args.harness, codec, decoder, stream, original, repeats)], stdout=row)
        counted[label] = stat_counts(csv)
    data = out / f"perf-{name}.data"
    with (out / f"repeat-{name}-record.json").open("w") as row:
        run([args.perf, "record", "-g", "-o", data, "--",
             *bench(args.harness, codec, decoder, stream, original, args.repeats)], stdout=row)
    with (out / f"report-{name}.txt").open("w") as report:
        run([args.perf, "report", "-i", data, "--no-children", "--percent-limit", "1", "--stdio"], stdout=report)
    annotated = []
    if decoder == "zipper":
        for rank, (share, symbol) in enumerate(top_symbols(args.perf, data, ANNOTATED), 1):
            path = out / f"annotate-{name}-{rank}.txt"
            with path.open("w") as annotation:
                try:
                    run([args.perf, "annotate", "-i", data, "--stdio", "--", symbol], stdout=annotation)
                except subprocess.CalledProcessError as error:
                    annotation.write(f"perf annotate failed with {error.returncode}\n")
            annotated.append({"rank": rank, "percent": share, "symbol": symbol, "file": path.name})
    return {"per_decode": per_decode(counted["decode"], counted["setup"], args.repeats), "annotated": annotated}


def ratio(zipper, other):
    return None if zipper is None or not other else zipper / other


def cell(value, digits=0):
    return "not counted" if value is None else f"{value:,.{digits}f}"


def summary(results, repeats):
    lines = ["# M35 profile", "",
             f"Counts of one decode, from `perf stat` with {repeats} repeats less `perf stat` of the setup alone.",
             "A hosted runner may expose no hardware counters, which then read \"not counted\".", ""]
    for codec, rows in results.items():
        zipper, other = rows["zipper"]["per_decode"], rows[CODECS[codec]]["per_decode"]
        lines += [f"## {codec}", "", f"| Event | zipper | {CODECS[codec]} | zipper over {CODECS[codec]} |",
                  "| --- | --- | --- | --- |"]
        for event in EVENTS:
            share = ratio(zipper[event], other[event])
            lines.append(f"| `{event}` | {cell(zipper[event])} | {cell(other[event])} | {cell(share, 3)} |")
        for name, counts in (("zipper", zipper), (CODECS[codec], other)):
            ipc = ratio(counts["instructions"], counts["cycles"])
            misses = ratio(counts["branch-misses"], counts["branches"])
            lines.append(f"\n- `{name}`: {cell(ipc, 2)} instructions per cycle, "
                         f"{cell(None if misses is None else misses * 100, 2)} % of branches missed")
        lines.append("")
        for row in rows["zipper"]["annotated"]:
            lines.append(f"- `{row['file']}` annotates {row['symbol']}, {row['percent']:.2f} % of Zipper's samples")
        lines.append("")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--harness", type=Path, required=True, help="the zipper-decode-bench binary")
    parser.add_argument("--corpus", type=Path, required=True, help="the folder `m31_decoders.py corpus` wrote")
    parser.add_argument("--codec", choices=(*CODECS, "all"), default="all")
    parser.add_argument("--repeats", type=int, default=10, help="decodes in each profiled process, 1 or more")
    parser.add_argument("--perf", default="perf")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats needs 1 or more")
    args.harness = args.harness.resolve(strict=True)
    args.corpus = args.corpus.resolve(strict=True)
    args.out.mkdir(parents=True, exist_ok=True)
    manifest = checked_corpus(args.corpus / "streams", args.corpus / built_source(args.corpus)["tar"])
    codecs = list(CODECS) if args.codec == "all" else [args.codec]
    results = {}
    for codec in codecs:
        results[codec] = {decoder: profile(args, codec, decoder, manifest) for decoder in ("zipper", CODECS[codec])}
        (args.out / "profile.json").write_text(json.dumps({"repeats": args.repeats, "results": results}, indent=2) + "\n")
        (args.out / "summary.md").write_text(summary(results, args.repeats) + "\n")


if __name__ == "__main__":
    main()
