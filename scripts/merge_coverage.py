#!/usr/bin/env python3
"""Git merge driver for verification/coverage.txt, one section at a time.

Git calls it as `merge_coverage.py BASE OURS THEIRS` and reads the result
from OURS. A section changed on one side only takes that side, a section
added on either side is kept, and a section both sides changed differently
is a conflict. A line-based merge cannot do this, because every section
that lists the shared field check ends with the same three lines.
"""
import re
import sys


def sections(text):
    parts = re.split(r"(?m)^(?=\[[^\]\n]+\]$)", text)
    preamble = parts[0] if parts and not parts[0].startswith("[") else ""
    named = {}
    order = []
    for part in parts:
        if part.startswith("["):
            name = part[1:part.index("]")]
            named[name] = part.rstrip("\n") + "\n"
            order.append(name)
    return preamble.rstrip("\n") + "\n" if preamble.strip() else "", named, order


def three_way(base, ours, theirs):
    if ours == theirs:
        return ours, False
    if ours == base:
        return theirs, False
    if theirs == base:
        return ours, False
    return None, True


def conflict_block(name, ours, theirs):
    return (
        f"<<<<<<< ours [{name}]\n{ours or ''}=======\n{theirs or ''}>>>>>>> theirs [{name}]\n"
    )


def merge(base_text, ours_text, theirs_text):
    base_pre, base, _ = sections(base_text)
    ours_pre, ours, ours_order = sections(ours_text)
    theirs_pre, theirs, theirs_order = sections(theirs_text)
    conflicts = []
    preamble, clash = three_way(base_pre, ours_pre, theirs_pre)
    if clash:
        conflicts.append("preamble")
        preamble = conflict_block("preamble", ours_pre, theirs_pre)
    names = ours_order + [name for name in theirs_order if name not in ours]
    merged = []
    for name in names:
        chosen, clash = three_way(base.get(name), ours.get(name), theirs.get(name))
        if clash:
            conflicts.append(name)
            merged.append(conflict_block(name, ours.get(name), theirs.get(name)))
        elif chosen is not None:
            merged.append(chosen)
    head = preamble + "\n" if preamble else ""
    return head + "\n".join(merged), conflicts


def main():
    if len(sys.argv) != 4:
        sys.exit("usage: merge_coverage.py BASE OURS THEIRS")
    texts = []
    for path in sys.argv[1:]:
        with open(path, encoding="utf-8") as file:
            texts.append(file.read())
    result, conflicts = merge(*texts)
    with open(sys.argv[2], "w", encoding="utf-8") as file:
        file.write(result)
    if conflicts:
        print(f"merge_coverage: both sides changed {', '.join(conflicts)}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
