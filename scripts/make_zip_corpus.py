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
    # A nested archive from a streaming writer contains data descriptors for
    # its own entries. Stored outer entries must still extract.
    inner = io.BytesIO()
    with zipfile.ZipFile(Unseekable(inner), "w", zipfile.ZIP_STORED) as archive:
        archive.writestr("inner.txt", b"inner entry\n" * 8)
    files["nested/inner.zip"] = inner.getvalue()
    files["modes/run.sh"] = b"#!/bin/sh\necho run\n"
    files["modes/readonly.txt"] = b"read only\n"
    files["modes/locked/child.txt"] = b"inside a read-only directory\n"
    for name, data in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    (root / "empty-dir").mkdir()
    (root / "modes/run.sh").chmod(0o755)
    (root / "modes/readonly.txt").chmod(0o444)
    # Fixed times, odd and even seconds, set on files before their directories.
    paths = sorted(root.rglob("*"), key=lambda path: (path.is_dir(), -len(path.parts), str(path)))
    for index, path in enumerate([*paths, root]):
        seconds = 1_577_934_245 + index * 7_919
        os.utime(path, (seconds, seconds))
    (root / "modes/locked").chmod(0o555)


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
                info = zipfile.ZipInfo.from_file(path, path.relative_to(source.parent).as_posix())
                if path.is_dir():
                    archive.writestr(info, b"")
                    continue
                info.compress_type = method
                with archive.open(info, "w", force_zip64=zip64) as entry:
                    entry.write(path.read_bytes())


def run(command, cwd, stdout=None, zone=None):
    env = None if zone is None else {**os.environ, "TZ": zone}
    subprocess.run(command, cwd=cwd, check=True, stdout=stdout or subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, env=env)


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


def walk(root):
    """Every path under root, without following symlinks."""
    found = []
    for directory, subdirectories, files in os.walk(root):
        found += [Path(directory) / name for name in subdirectories + files]
    return sorted(found)


def key(root, path):
    return unicodedata.normalize("NFC", path.relative_to(root).as_posix())


def tree(root):
    """Map of NFC path to None for a directory, (size, crc32) for a file, or
    ("link", target) for a symlink."""
    result = {}
    for path in walk(root):
        if path.is_symlink():
            result[key(root, path)] = ["link", os.readlink(path)]
        elif path.is_dir():
            result[key(root, path)] = None
        else:
            data = path.read_bytes()
            result[key(root, path)] = [len(data), zlib.crc32(data)]
    return result


def modes(root):
    """Map of NFC path to its permission bits, for paths that are not links."""
    return {key(root, path): path.lstat().st_mode & 0o777 for path in walk(root) if not path.is_symlink()}


def unlock(root):
    """Restores owner access so read-only directories can be deleted."""
    for path in [root, *walk(root)]:
        if path.is_dir() and not path.is_symlink():
            path.chmod(path.stat().st_mode | 0o700)


def times(root):
    """Map of NFC path to its modification time in whole seconds, for paths
    that are not links."""
    return {key(root, path): int(path.lstat().st_mtime) for path in walk(root) if not path.is_symlink()}


def write_link_source(root):
    """A tree of relative links that stay inside it."""
    (root / "text").mkdir(parents=True)
    (root / "text/readme.txt").write_bytes(b"linked\n")
    (root / "links").mkdir()
    os.symlink("../text/readme.txt", root / "links/file-link")
    os.symlink("../text", root / "links/dir-link")
    os.symlink("file-link", root / "links/chain")


def link_writers(have):
    """Writers that store symlinks as links, from cwd containing `src`."""
    found = []
    if have("zip"):
        found.append(("infozip-links", lambda target, cwd: run(["zip", "-q", "-r", "-y", str(target), "src"], cwd)))
    if have("7zz"):
        found.append(("7zip-links", lambda target, cwd: run(
            ["7zz", "a", "-tzip", "-bd", "-snl", str(target), "src"], cwd)))
    if have("bsdtar"):
        found.append(("bsdtar-links", lambda target, cwd: run(["bsdtar", "--format", "zip", "-cf", str(target), "src"], cwd)))
    if have("ditto"):
        found.append(("ditto-links", lambda target, cwd: run(
            ["ditto", "-c", "-k", "--norsrc", "--keepParent", "src", str(target)], cwd)))
    return found


# Info-ZIP unzip and bsdtar restore permission bits, including directory
# modes, and modification times. Python restores neither, and 7-Zip skips
# directory modes.
MODE_READERS = ["unzip", "bsdtar"]
# A time that both readers restore identically in both zones is exact. Times
# from DOS fields depend on the zone and are left to tests/cli.rs.
ZONES = ["UTC", "Pacific/Auckland"]


def readers(have, zone=None):
    found = [("python", lambda archive, out: zipfile.ZipFile(archive).extractall(out))]
    if have("unzip"):
        found.append(("unzip", lambda archive, out: run(
            ["unzip", "-q", str(archive), "-d", str(out)], out.parent, zone=zone)))
    if have("bsdtar"):
        def bsdtar(archive, out):
            out.mkdir()
            run(["bsdtar", "-xf", str(archive), "-C", str(out)], out.parent, zone=zone)
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
    manifests, all_modes, all_times = {}, {}, {}
    with tempfile.TemporaryDirectory(prefix="zipper-corpus-") as work:
        work = Path(work)
        write_source(work / "src")
        write_link_source(work / "links" / "src")
        try:
            build(args, have, work, summary, manifests, all_modes, all_times)
        finally:
            unlock(work)
    write_manifest(args, summary, manifests, all_modes, all_times)


def build(args, have, work, summary, manifests, all_modes, all_times):
    """Writes each archive and records what the reference readers agree on."""
    jobs = [(name, write, work) for name, write in writers(have)]
    jobs += [(name, write, work / "links") for name, write in link_writers(have)]
    for name, write, source in jobs:
        source_times = times(source)
        archive = args.output / f"{name}.zip"
        write(archive.resolve(), source)
        trees, reader_modes = {}, {}
        for reader, extract in readers(have):
            out = work / f"out-{name}-{reader}"
            try:
                extract(archive.resolve(), out)
                trees[reader] = tree(out)
                reader_modes[reader] = modes(out)
            except Exception as error:
                trees[reader] = f"failed: {type(error).__name__}"
            finally:
                if out.exists():
                    unlock(out)
                shutil.rmtree(out, ignore_errors=True)
        opened = {r: t for r, t in trees.items() if isinstance(t, dict)}
        # Only unzip and bsdtar create links. Other readers write files in
        # their place, so link paths vote separately, like modes.
        link_trees = [opened.get(reader, {}) for reader in MODE_READERS]
        links = {path: value for path, value in link_trees[0].items()
                 if isinstance(value, list) and value[0] == "link"
                 and all(other.get(path) == value for other in link_trees[1:])}
        opened = {r: {path: value for path, value in t.items() if path not in links} for r, t in opened.items()}
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
        manifests[name] = {**json.loads(winner), **links}
        zone_times = []
        for zone in ZONES:
            for reader, extract in readers(have, zone):
                if reader not in MODE_READERS:
                    continue
                out = work / f"time-{name}-{reader}"
                try:
                    extract(archive.resolve(), out)
                    zone_times.append(times(out))
                finally:
                    if out.exists():
                        unlock(out)
                    shutil.rmtree(out, ignore_errors=True)
        # Keep a time only when every run restored the source's own time, so
        # an extraction-time value can never be recorded by coincidence.
        all_times[name] = {path: value for path, value in source_times.items()
                           if all(run.get(path) == value for run in zone_times)}
        mode_votes = [reader_modes.get(reader) for reader in MODE_READERS]
        agreed_modes = mode_votes[0] if None not in mode_votes and all(
            vote == mode_votes[0] for vote in mode_votes) else {}
        summary["writers"][name]["modes_agreed_by"] = MODE_READERS if agreed_modes else []
        all_modes[name] = agreed_modes


def write_manifest(args, summary, manifests, all_modes, all_times):
    # One line per expected path: archive, path, size or "dir", CRC-32 or "-",
    # the octal mode where the mode readers agree, and the exact modification
    # time in seconds, each "-" when unknown.
    lines = []
    for archive, expected in sorted(manifests.items()):
        for path, value in sorted(expected.items()):
            if value is None:
                fields = ["dir", "-"]
            elif value[0] == "link":
                fields = ["link", value[1]]
            else:
                fields = [str(value[0]), f"{value[1]:08x}"]
            mode = all_modes[archive].get(path)
            fields.append("-" if mode is None else f"{mode:o}")
            time = all_times[archive].get(path)
            fields.append("-" if time is None else str(time))
            lines.append("\t".join([archive, path, *fields]))
    (args.output / "manifest.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (args.output / "corpus.json").write_text(json.dumps(summary, indent=1, sort_keys=True) + "\n")
    print(f"{len(manifests)} archives agreed by {len(summary['readers'])} readers", file=sys.stderr)


if __name__ == "__main__":
    main()
