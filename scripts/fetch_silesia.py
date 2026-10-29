#!/usr/bin/env python3
"""Fetch a pinned Silesia mirror; record archive and extracted-file SHA-256."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import urllib.request
import zipfile

REVISION = "3f3fa2cdbbb3795c903b74e774acb309e1360337"
BASE = f"https://raw.githubusercontent.com/MiloszKrajewski/SilesiaCorpus/{REVISION}"
NAMES = "dickens mozilla mr nci ooffice osdb reymont samba sao webster x-ray xml".split()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    args.destination.mkdir(parents=True, exist_ok=False)
    records = {}
    for name in NAMES:
        url = f"{BASE}/{name}.zip"
        with urllib.request.urlopen(url, timeout=120) as response:
            archive = response.read()
        with zipfile.ZipFile(io.BytesIO(archive)) as zipped:
            members = zipped.infolist()
            if len(members) != 1 or members[0].filename != name:
                raise RuntimeError(f"unexpected archive members: {name}")
            data = zipped.read(members[0])
        (args.destination / name).write_bytes(data)
        records[name] = {"url": url, "zip_sha256": hashlib.sha256(archive).hexdigest(),
                         "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    # Keep provenance outside the timed corpus.
    args.destination.with_suffix(".manifest.json").write_text(
        json.dumps({"revision": REVISION, "files": records}, indent=2) + "\n")


if __name__ == "__main__":
    main()
