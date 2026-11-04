#!/usr/bin/env python3
"""Store a build folder as one ZIP, its files sorted, Deflate level 6, under its folder name."""
import sys
import zipfile
from pathlib import Path


def main():
    source, archive = Path(sys.argv[1]), Path(sys.argv[2])
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as out:
        for path in sorted(source.rglob("*")):
            out.write(path, Path(source.name) / path.relative_to(source))
    print(archive.stat().st_size, "bytes")


if __name__ == "__main__":
    main()
