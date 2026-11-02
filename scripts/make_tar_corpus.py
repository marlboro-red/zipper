#!/usr/bin/env python3
"""Build the tar reference corpus in tests/data/tar-corpus.

Each archive comes from a real tar writer in one header format, over one of
three source trees. The short tree fits every format. The links tree holds
symlinks and hard links. The long tree holds names and link targets past 100
bytes, which ustar and v7 writers refuse or split.
Compressed variants wrap one tar from each writer in gzip, zstd and xz.

Every reference reader extracts every archive. The manifest records the tree
that at least three readers produce identically, in the format of
tests/data/zip-corpus/manifest.tsv, and corpus.json names refusals and
dissent. Run on macOS with GNU tar as gtar, bsdtar, 7zz, gzip, zstd and xz,
under Python 3.14 or later for zstd and extraction filters."""
import argparse
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tarfile
import tempfile

from make_zip_corpus import modes, times, tree, unlock

GNU_TAR = ["gtar", "--owner=0", "--group=0", "--numeric-owner"]
BSDTAR = ["bsdtar", "--uid", "0", "--gid", "0", "--uname", "", "--gname", ""]
TAR_ENV = {**os.environ, "COPYFILE_DISABLE": "1"}


def write_short(root):
    rng = random.Random(20261102)
    files = {
        "text/readme.txt": b"Zipper tar corpus.\n" * 40,
        "text/empty.txt": b"",
        "binary/random-4k.bin": bytes(rng.getrandbits(8) for _ in range(4096)),
        "binary/block-512.bin": bytes(range(256)) * 2,
        "binary/block-513.bin": bytes(range(256)) * 2 + b"!",
        "binary/zeros-64k.bin": bytes(65536),
        "nested/deep/deeper/leaf.txt": b"leaf\n",
        "names/café.txt": "café\n".encode(),
        "names/日本語.txt": "日本語\n".encode(),
        "names/space and (parens).txt": b"spaces\n",
        "modes/run.sh": b"#!/bin/sh\necho run\n",
        "modes/readonly.txt": b"read only\n",
        "modes/locked/child.txt": b"inside a read-only directory\n",
    }
    for name, data in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    (root / "empty-dir").mkdir()
    (root / "modes/run.sh").chmod(0o755)
    (root / "modes/readonly.txt").chmod(0o444)
    finish(root, locked=root / "modes/locked")


def write_links(root):
    """Symlinks and hard links that stay inside the tree. 7-Zip refuses
    symlinks that climb with `..`, so they live apart from the short tree."""
    (root / "text").mkdir(parents=True)
    (root / "text/readme.txt").write_bytes(b"linked\n")
    (root / "hard").mkdir()
    (root / "hard/original.txt").write_bytes(b"one inode, two names\n")
    os.link(root / "hard/original.txt", root / "hard/second.txt")
    (root / "links").mkdir()
    os.symlink("../text/readme.txt", root / "links/file-link")
    os.symlink("../text", root / "links/dir-link")
    os.symlink("file-link", root / "links/chain")
    finish(root)


def write_long(root):
    split = root / ("d" * 40) / ("e" * 40) / ("f" * 40)
    split.mkdir(parents=True)
    (split / "fits-a-ustar-prefix.txt").write_bytes(b"split between prefix and name\n")
    long_name = "n" * 120 + ".txt"
    (root / long_name).write_bytes(b"a name longer than 100 bytes\n")
    long_directory = root / ("l" * 110)
    long_directory.mkdir()
    (long_directory / "child.txt").write_bytes(b"in a directory named past 100 bytes\n")
    os.symlink("d" * 40 + "/" + "e" * 40 + "/" + "f" * 40 + "/fits-a-ustar-prefix.txt", root / "long-target-link")
    finish(root)


def finish(root, locked=None):
    """Sets fixed whole-second times, children before their directories."""
    paths = sorted(root.rglob("*"), key=lambda path: (path.is_dir(), -len(path.parts), str(path)))
    for index, path in enumerate([*paths, root]):
        seconds = 1_577_934_245 + index * 7_919
        os.utime(path, (seconds, seconds), follow_symlinks=False)
    if locked is not None:
        locked.chmod(0o555)


def run(command, cwd, stdout=None, stdin=None):
    subprocess.run(command, cwd=cwd, check=True, stdout=stdout or subprocess.DEVNULL,
                   stdin=stdin, stderr=subprocess.DEVNULL, env=TAR_ENV)


def python_tar(target, cwd, format, mode="w"):
    with tarfile.open(target, mode, format=format) as archive:
        archive.add(cwd / "src", arcname="src", filter=anonymous)


def anonymous(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info


def writers(have):
    """(name, extension, function writing target from cwd containing `src`)."""
    found = []
    if have("gtar"):
        for format in ["gnu", "oldgnu", "pax", "ustar", "v7"]:
            found.append((f"gnutar-{format}", "tar", lambda target, cwd, format=format: run(
                [*GNU_TAR, f"--format={format}", "-cf", str(target), "src"], cwd)))
    if have("bsdtar"):
        for format in ["ustar", "pax", "gnutar", "v7tar"]:
            found.append((f"bsdtar-{format}", "tar", lambda target, cwd, format=format: run(
                [*BSDTAR, "--format", format, "-cf", str(target), "src"], cwd)))
    for format, value in [("ustar", tarfile.USTAR_FORMAT), ("gnu", tarfile.GNU_FORMAT),
                          ("pax", tarfile.PAX_FORMAT)]:
        found.append((f"python-{format}", "tar", lambda target, cwd, value=value: python_tar(target, cwd, value)))
    return found


def compressed_writers(have):
    """Each wraps one tar format in a compressor the way users meet it."""
    found = []
    if have("gtar") and have("gzip"):
        found.append(("gnutar-gnu-gzip", "tar.gz", lambda target, cwd: run(
            [*GNU_TAR, "--format=gnu", "-czf", str(target), "src"], cwd)))
    if have("gtar") and have("zstd"):
        found.append(("gnutar-pax-zstd", "tar.zst", lambda target, cwd: run(
            [*GNU_TAR, "--format=pax", "--zstd", "-cf", str(target), "src"], cwd)))
    if have("gtar") and have("xz"):
        found.append(("gnutar-pax-xz", "tar.xz", lambda target, cwd: run(
            [*GNU_TAR, "--format=pax", "-cJf", str(target), "src"], cwd)))
    if have("bsdtar"):
        found.append(("bsdtar-pax-gzip", "tgz", lambda target, cwd: run(
            [*BSDTAR, "--format", "pax", "-czf", str(target), "src"], cwd)))
        found.append(("bsdtar-pax-xz", "txz", lambda target, cwd: run(
            [*BSDTAR, "--format", "pax", "-cJf", str(target), "src"], cwd)))
    found.append(("python-pax-gzip", "tar.gz", lambda target, cwd: python_tar(
        target, cwd, tarfile.PAX_FORMAT, "w:gz")))
    found.append(("python-pax-xz", "tar.xz", lambda target, cwd: python_tar(
        target, cwd, tarfile.PAX_FORMAT, "w:xz")))
    if sys.version_info >= (3, 14):
        found.append(("python-gnu-zstd", "tar.zst", lambda target, cwd: python_tar(
            target, cwd, tarfile.GNU_FORMAT, "w:zst")))
    if have("gzip"):
        found.append(("python-gnu-gzip-members", "tar.gz", lambda target, cwd: gzip_members(target, cwd)))
    return found


def gzip_members(target, cwd):
    """A tar split at a block boundary into two gzip members, as `cat a.gz b.gz` makes."""
    with tempfile.NamedTemporaryFile(suffix=".tar") as plain:
        python_tar(plain.name, cwd, tarfile.GNU_FORMAT)
        data = Path(plain.name).read_bytes()
    middle = len(data) // 2 // 512 * 512
    with open(target, "wb") as out:
        for part in [data[:middle], data[middle:]]:
            out.write(subprocess.run(["gzip", "-n", "-c"], input=part, check=True,
                                     capture_output=True).stdout)


# GNU tar, bsdtar and Python restore permission bits and modification times.
# 7-Zip skips directory modes.
MODE_READERS = ["gnutar", "bsdtar", "python"]


def readers(have):
    found = [("python", lambda archive, out: python_extract(archive, out))]
    if have("gtar"):
        found.append(("gnutar", lambda archive, out: tar_extract("gtar", archive, out)))
    if have("bsdtar"):
        found.append(("bsdtar", lambda archive, out: tar_extract("bsdtar", archive, out)))
    if have("7zz"):
        found.append(("7zip", seven_zip_extract))
    return found


def python_extract(archive, out):
    with tarfile.open(archive) as source:
        source.extractall(out, filter="fully_trusted")


def tar_extract(program, archive, out):
    out.mkdir()
    run([program, "-xpf", str(archive), "-C", str(out)], out.parent)


def seven_zip_extract(archive, out):
    """7-Zip opens a compressed tar as the compressed file, so a second 7-Zip
    reads the decompressed stream as tar."""
    if archive.suffix == ".tar":
        run(["7zz", "x", "-y", "-bd", "-snl", f"-o{out}", str(archive)], out.parent)
        return
    unpack = subprocess.Popen(["7zz", "x", "-so", str(archive)], stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL)
    try:
        run(["7zz", "x", "-y", "-bd", "-snl", "-si", "-ttar", f"-o{out}"], out.parent, stdin=unpack.stdout)
    finally:
        unpack.stdout.close()
        if unpack.wait() != 0:
            raise RuntimeError("7zz could not decompress")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("tests/data/tar-corpus"))
    args = parser.parse_args()
    have = lambda tool: shutil.which(tool) is not None
    args.output.mkdir(parents=True, exist_ok=True)
    for old in args.output.iterdir():
        if old.name not in ("manifest.tsv", "corpus.json"):
            old.unlink()
    summary = {"writers": {}, "readers": [name for name, _ in readers(have)],
               "python": sys.version.split()[0],
               "missing_tools": [t for t in ["gtar", "bsdtar", "7zz", "gzip", "zstd", "xz"] if not have(t)]}
    manifests, all_modes, all_times = {}, {}, {}
    with tempfile.TemporaryDirectory(prefix="zipper-tar-corpus-") as work:
        work = Path(work)
        write_short(work / "short" / "src")
        write_links(work / "links" / "src")
        write_long(work / "long" / "src")
        try:
            jobs = [(f"{name}-{shape}", extension, write, work / shape)
                    for name, extension, write in writers(have) for shape in ["short", "links", "long"]]
            jobs += [(name, extension, write, work / "short") for name, extension, write in compressed_writers(have)]
            for name, extension, write, source in jobs:
                build(args, have, work, summary, manifests, all_modes, all_times, name, extension, write, source)
        finally:
            unlock(work)
    write_manifest(args, summary, manifests, all_modes, all_times)


def build(args, have, work, summary, manifests, all_modes, all_times, name, extension, write, source):
    """Writes one archive and records what the reference readers agree on."""
    archive = args.output / f"{name}.{extension}"
    try:
        write(archive.resolve(), source)
    except Exception as error:
        summary["writers"][name] = {"refused": type(error).__name__}
        archive.unlink(missing_ok=True)
        return
    source_times = times(source)
    trees, reader_modes, reader_times = {}, {}, {}
    for reader, extract in readers(have):
        out = work / f"out-{name}-{reader}"
        try:
            extract(archive.resolve(), out)
            trees[reader] = tree(out)
            reader_modes[reader] = modes(out)
            reader_times[reader] = times(out)
        except Exception as error:
            trees[reader] = f"failed: {type(error).__name__}"
        finally:
            if out.exists():
                unlock(out)
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
        return
    manifests[name] = json.loads(winner)
    # bsdtar lists a directory before its children. GNU tar then fails under a
    # read-only directory and restores a directory time before writing into
    # it, so modes and times need two agreeing readers, not three.
    voters = [reader for reader in MODE_READERS if reader in reader_modes]
    mode_votes = [reader_modes[reader] for reader in voters]
    agreed_modes = mode_votes[0] if len(voters) >= 2 and all(
        vote == mode_votes[0] for vote in mode_votes) else {}
    summary["writers"][name]["modes_agreed_by"] = voters if agreed_modes else []
    all_modes[name] = agreed_modes
    all_times[name] = {path: value for path, value in source_times.items()
                       if sum(reader_times[reader].get(path) == value for reader in voters) >= 2}


def write_manifest(args, summary, manifests, all_modes, all_times):
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
    print(f"{len(manifests)} archives agreed by at least three of {len(summary['readers'])} readers",
          file=sys.stderr)


if __name__ == "__main__":
    main()
