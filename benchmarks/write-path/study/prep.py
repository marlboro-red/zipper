#!/usr/bin/env python3
"""Pack each workload as ZIP, tar, tar.gz and 7z for the write-path study."""
import subprocess
import sys
from pathlib import Path

scripts, corpora, out = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
sys.path.insert(0, str(scripts))
import extraction_matrix  # noqa: E402

out.mkdir(parents=True, exist_ok=True)
for name, folder in [("dependencies", "dependencies/dependencies"), ("flat-tiny", "flat-tiny/flat-tiny"),
                     ("silesia", "silesia/silesia")]:
    source = corpora / folder
    assert source.is_dir(), source
    if not (out / f"{name}.zip").exists():
        extraction_matrix.write_zip(source, out / f"{name}.zip")
    if not (out / f"{name}.tar").exists():
        extraction_matrix.write_tar(source, out / f"{name}.tar")
    if not (out / f"{name}.tar.gz").exists():
        extraction_matrix.write_gzip(out / f"{name}.tar", out / f"{name}.tar.gz")
    for label, flags in [("7z", []), ("nonsolid.7z", ["-ms=off"])]:
        archive = out / f"{name}.{label}"
        if not archive.exists():
            subprocess.run(["7zz", "a", "-bd", "-bso0", *flags, str(archive), source.name], cwd=source.parent,
                           check=True)
for archive in sorted(out.iterdir()):
    print(archive.stat().st_size, archive.name)
