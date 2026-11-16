#!/usr/bin/env python3
"""Time M35's A/B: Zipper's decoder at two revisions and the C library, in one run on one machine.

A hosted runner can change CPU between runs and counts no hardware events, so a change to a decoder
counts only when both builds run in one job. This runs `m31_decoders.decode` with the head harness
and the base harness: for each codec, Zipper at the base, Zipper at the head, and libzstd or
liblzma, one fresh process per sample, shuffled into each round by a fixed seed after one warmup,
with every decoded byte checked. `ab.json` keeps every raw sample, both commits and the machine,
and `ab.md` gives each row's median and range, the head over the base and over the C library,
and whether the ranges are apart.
"""
import argparse
from pathlib import Path
from bench_common import refuse_when_busy
from m31_decoders import ZIPPER_DECODER, base_row, built_source, decode, header, write
from m35_profile import CODECS

MIN_ROUNDS = 11
BASE_LABEL = "base"


def ranges(head, other):
    if head["max_seconds"] < other["min_seconds"]:
        return "apart, head faster"
    if head["min_seconds"] > other["max_seconds"]:
        return "apart, head slower"
    return "overlap"


def short(commit):
    return f"`{commit[:12]}`" if commit else "unknown"


def table(record):
    ab = record["ab"]
    decode_record = record["decode"]
    base = base_row(ab["base_label"])
    lines = ["# M35 A/B", "",
             f"Head {short(ab['head_commit'])} against base {short(ab['base_commit'])} "
             f"(`{ab.get('base_ref') or ab['base_commit']}`) on {record['machine'].get('cpu') or 'an unnamed CPU'}.",
             f"{decode_record['rounds']} rounds after one warmup, in an order shuffled by seed "
             f"{decode_record['order_seed']}. Each sample decodes in a fresh process and checks every byte.",
             "Seconds time the decode call alone. A ratio is the head's median time over the other row's, "
             "so below 1 means the head takes less time.", ""]
    if record.get("trial"):
        lines += ["This is a trial run with fewer rounds than a measurement needs.", ""]
    lines += ["| Codec | Row | Median s | Range s | MiB/s |", "| --- | --- | --- | --- | --- |"]
    for codec, rows in decode_record["results"].items():
        for name, title in ((base, "Zipper at base"), (ZIPPER_DECODER, "Zipper at head"),
                            (CODECS[codec], CODECS[codec])):
            row = rows[name]
            lines.append(f"| {codec} | {title} | {row['median_seconds']:.3f} | "
                         f"{row['min_seconds']:.3f} to {row['max_seconds']:.3f} | {row['mib_s']:.1f} |")
    lines += ["", "| Codec | Comparison | Median time ratio | Ranges |", "| --- | --- | --- | --- |"]
    for codec, rows in decode_record["results"].items():
        head = rows[ZIPPER_DECODER]
        for name, title in ((base, "head over base"), (CODECS[codec], f"head over {CODECS[codec]}")):
            other = rows[name]
            lines.append(f"| {codec} | {title} | {head['median_seconds'] / other['median_seconds']:.3f} | "
                         f"{ranges(head, other)} |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--harness", type=Path, required=True, help="the head's zipper-decode-bench binary")
    parser.add_argument("--base-harness", type=Path, required=True, help="the base's zipper-decode-bench binary")
    parser.add_argument("--base-commit", required=True, help="the commit the base harness was built from")
    parser.add_argument("--base-ref", help="the ref that named the base commit")
    parser.add_argument("--corpus", type=Path, required=True, help="the folder `m31_decoders.py corpus` wrote")
    parser.add_argument("--codec", choices=(*CODECS, "all"), default="all")
    parser.add_argument("--rounds", type=int, default=MIN_ROUNDS, help="rounds after one warmup")
    parser.add_argument("--trial", action="store_true", help="allow fewer rounds; the JSON and table say trial")
    parser.add_argument("--allow-busy", action="store_true", help="for trials; the JSON lists the busy processes")
    parser.add_argument("--out", type=Path, required=True, help="the folder for ab.json and ab.md")
    args = parser.parse_args()
    if args.rounds < 1 or (args.rounds < MIN_ROUNDS and not args.trial):
        parser.error(f"--rounds needs {MIN_ROUNDS} or more, or --trial for a smoke run")
    corpus = args.corpus.resolve(strict=True)
    codecs = list(CODECS) if args.codec == "all" else [args.codec]
    record = header(args.trial)
    record["busy_processes"] = refuse_when_busy(args.allow_busy)
    record["ab"] = {"head_commit": record["revision"]["commit"], "base_commit": args.base_commit,
                    "base_ref": args.base_ref, "base_label": BASE_LABEL}
    record["decode"], record["corpus"] = decode(
        args.harness, corpus / "streams", corpus / built_source(corpus)["tar"], args.rounds, codecs,
        decoders={ZIPPER_DECODER, *(CODECS[codec] for codec in codecs)}, base=(BASE_LABEL, args.base_harness))
    write(record, args.out / "ab.json")
    (args.out / "ab.md").write_text(table(record))


if __name__ == "__main__":
    main()
