#!/usr/bin/env python3
"""Build the ZIP reference corpus in tests/data/zip-corpus.

Each archive comes from a real ZIP writer in a specific mode. Every available
reference reader extracts it. The manifest records the tree that at least three
readers produce identically, and corpus.json names any reader that disagreed.
Python zipfile, for example, decodes names without the UTF-8 flag as CP437. tests/zip_corpus.rs then requires
both native Zipper engines to produce exactly that tree.

Run on macOS with Info-ZIP zip and unzip, 7zz, bsdtar and ditto installed.
Missing writers and readers are skipped and named in corpus.json."""
import argparse
import io
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import zipfile
import zlib


def write_source(root):
    """A small tree covering the shapes the native reader must handle."""
    rng = random.Random(20261101)
    files = {
        "text/readme.txt": b"Zipper reference corpus.\n" * 40,
        "text/empty.txt": b"",
        "binary/random-4k.bin": bytes(rng.getrandbits(8) for _ in range(4096)),
        "binary/zeros-64k.bin": bytes(65536),
        "binary/zeros-64k-plus-1.bin": bytes(65537),
        "binary/pattern-192k-plus-1.bin": (b"0123456789abcdef" * 12289)[:196609],
        "nested/deep/deeper/leaf.txt": b"leaf\n",
        "names/café.txt": "café\n".encode(),
        "names/日本語.txt": "日本語\n".encode(),
        "names/space and (parens).txt": b"spaces\n",
    }
    for name, data in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    (root / "empty-dir").mkdir()


class Unseekable(io.RawIOBase):
    def __init__(self, stream):
        self.stream = stream

    def writable(self):
        return True

    def write(self, data):
        return self.stream.write(data)


def python_zip(target, source, method, seekable, zip64):
    with open(target, "wb") as raw:
        stream = raw if seekable else Unseekable(raw)
        with zipfile.ZipFile(stream, "w", method) as archive:
            for path in sorted(source.rglob("*")):
                name = path.relative_to(source.parent).as_posix()
                if path.is_dir():
                    archive.writestr(name + "/", b"")
                    continue
                with archive.open(name, "w", force_zip64=zip64) as entry:
                    entry.write(path.read_bytes())


def run(command, cwd, stdout=None):
    subprocess.run(command, cwd=cwd, check=True, stdout=stdout or subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)


def writers(have):
    """(archive name, function writing it from cwd containing `src`)."""
    found = []
    if have("zip"):
        for name, flags in [("infozip-default", []), ("infozip-stored", ["-0"]),
                            ("infozip-zip64", ["-fz"]), ("infozip-descriptors", ["-fd"])]:
            found.append((name, lambda target, cwd, flags=flags: run(
                ["zip", "-q", "-r", *flags, str(target), "src"], cwd)))
        def piped(target, cwd):
            data = subprocess.run(["zip", "-q", "-r", "-", "src"], cwd=cwd, check=True,
                                  capture_output=True).stdout
            target.write_bytes(data)
        found.append(("infozip-piped", piped))
    if have("7zz"):
        for name, flags in [("7zip-deflate", ["-mm=Deflate"]), ("7zip-copy", ["-mm=Copy"]),
                            ("7zip-timestamps", ["-mtc=on"])]:
            found.append((name, lambda target, cwd, flags=flags: run(
                ["7zz", "a", "-tzip", "-bd", *flags, str(target), "src"], cwd)))
    if have("bsdtar"):
        for name, flags in [("bsdtar-deflate", []), ("bsdtar-stored", ["--options", "zip:compression=store"])]:
            found.append((name, lambda target, cwd, flags=flags: run(
                ["bsdtar", "--format", "zip", *flags, "-cf", str(target), "src"], cwd)))
    if have("ditto"):
        found.append(("ditto", lambda target, cwd: run(
            ["ditto", "-c", "-k", "--norsrc", "--keepParent", "src", str(target)], cwd)))
    for name, method, seekable, zip64 in [
            ("python-deflate", zipfile.ZIP_DEFLATED, True, False),
            ("python-stored", zipfile.ZIP_STORED, True, False),
            ("python-stream-deflate", zipfile.ZIP_DEFLATED, False, False),
            ("python-stream-stored", zipfile.ZIP_STORED, False, False),
            ("python-zip64", zipfile.ZIP_DEFLATED, True, True),
            ("python-stream-zip64", zipfile.ZIP_DEFLATED, False, True)]:
        found.append((name, lambda target, cwd, a=(method, seekable, zip64): python_zip(
            target, cwd / "src", *a)))
    return found


def tree(root):
    """Map of NFC path to None for directories or (size, crc32) for files."""
    result = {}
    for path in sorted(root.rglob("*")):
        name = unicodedata.normalize("NFC", path.relative_to(root).as_posix())
        if path.is_dir():
            result[name] = None
        else:
            data = path.read_bytes()
            result[name] = [len(data), zlib.crc32(data)]
    return result


def readers(have):
    found = [("python", lambda archive, out: zipfile.ZipFile(archive).extractall(out))]
    if have("unzip"):
        found.append(("unzip", lambda archive, out: run(["unzip", "-q", str(archive), "-d", str(out)], out.parent)))
    if have("bsdtar"):
        def bsdtar(archive, out):
            out.mkdir()
            run(["bsdtar", "-xf", str(archive), "-C", str(out)], out.parent)
        found.append(("bsdtar", bsdtar))
    if have("7zz"):
        found.append(("7zip", lambda archive, out: run(["7zz", "x", "-y", "-bd", f"-o{out}", str(archive)], out.parent)))
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("tests/data/zip-corpus"))
    args = parser.parse_args()
    have = lambda tool: shutil.which(tool) is not None
    args.output.mkdir(parents=True, exist_ok=True)
    for old in args.output.glob("*.zip"):
        old.unlink()
    summary = {"writers": {}, "readers": [name for name, _ in readers(have)],
               "missing_tools": [t for t in ["zip", "unzip", "7zz", "bsdtar", "ditto", "jar"] if not have(t)]}
    manifests = {}
    with tempfile.TemporaryDirectory(prefix="zipper-corpus-") as work:
        work = Path(work)
        write_source(work / "src")
        for name, write in writers(have):
            archive = args.output / f"{name}.zip"
            write(archive.resolve(), work)
            trees = {}
            for reader, extract in readers(have):
                out = work / f"out-{name}-{reader}"
                try:
                    extract(archive.resolve(), out)
                    trees[reader] = tree(out)
                except Exception as error:
                    trees[reader] = f"failed: {type(error).__name__}"
                finally:
                    shutil.rmtree(out, ignore_errors=True)
            opened = {r: t for r, t in trees.items() if isinstance(t, dict)}
            votes = {}
            for reader, result in opened.items():
                votes.setdefault(json.dumps(result, sort_keys=True), []).append(reader)
            winner, agreeing = max(votes.items(), key=lambda vote: len(vote[1]), default=(None, []))
            summary["writers"][name] = {
                "readers": {r: ("ok" if isinstance(t, dict) else t) for r, t in trees.items()},
                "agreeing": sorted(agreeing),
                "dissenting": sorted(r for r in opened if r not in agreeing),
            }
            if len(agreeing) < 3:
                archive.unlink()
                print(f"{name}: fewer than three readers agree; excluded", file=sys.stderr)
                continue
            manifests[name] = json.loads(winner)
    # One line per expected path: archive, path, then "dir" or size and CRC-32.
    lines = []
    for archive, expected in sorted(manifests.items()):
        for path, value in sorted(expected.items()):
            fields = ["dir"] if value is None else [str(value[0]), f"{value[1]:08x}"]
            lines.append("\t".join([archive, path, *fields]))
    (args.output / "manifest.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (args.output / "corpus.json").write_text(json.dumps(summary, indent=1, sort_keys=True) + "\n")
    print(f"{len(manifests)} archives agreed by {len(summary['readers'])} readers", file=sys.stderr)


if __name__ == "__main__":
    main()
