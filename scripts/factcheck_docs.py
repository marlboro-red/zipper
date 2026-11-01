#!/usr/bin/env python3
"""Report facts lost from Markdown pages since a git revision.

A fact is a number, a link target or an inline code span. The script reads
each named page at the revision (default HEAD) and reports tokens missing from
the union of those pages on disk plus any pages named after --also. Use it when
rewriting, merging or moving pages: a rewording must keep every fact."""
import argparse
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys


def tokens(text):
    text = text.replace("–", "-").replace("—", "-")
    found = Counter()
    for match in re.finditer(r"\]\(([^)\s]+)\)", text):
        found["link " + match.group(1).split("#")[0]] += 1
    for match in re.finditer(r"`([^`\n]+)`", text):
        found["code " + match.group(1)] += 1
    for match in re.finditer(r"(?<![\w.])-?\d[\d,]*(?:\.\d+)?", text):
        found["num " + match.group(0).replace(",", "")] += 1
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pages", nargs="+", help="pages by their path at the revision")
    parser.add_argument("--also", nargs="*", default=[], help="pages that received moved content")
    parser.add_argument("--revision", default="HEAD")
    args = parser.parse_args()
    old, new = Counter(), Counter()
    for page in args.pages:
        shown = subprocess.run(["git", "show", f"{args.revision}:{page}"], capture_output=True, text=True)
        if shown.returncode == 0:
            old += tokens(shown.stdout)
    for page in args.pages + args.also:
        if Path(page).exists():
            new += tokens(Path(page).read_text(encoding="utf-8"))
    missing = sorted(key for key in old if key not in new)
    for key in missing:
        print("missing", key)
    print(f"{len(missing)} missing of {len(old)} distinct tokens", file=sys.stderr)
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
