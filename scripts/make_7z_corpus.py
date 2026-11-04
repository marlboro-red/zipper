#!/usr/bin/env python3
"""Build the 7z reference corpus in tests/data/7z-corpus.

Each archive comes from a real 7z writer, 7-Zip or libarchive through bsdtar,
with one method or layout, over the short, links and long trees of the tar
corpus. 7-Zip and bsdtar extract every archive, and the manifest records the
tree both produce, in the format of tests/data/zip-corpus/manifest.tsv. Modes
and times come from the source where bsdtar restores them. Archives whose
methods the native reader does not decode go in refused.tsv with the error
the reader must name. Run on macOS with 7zz and bsdtar."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import os

from make_tar_corpus import finish, write_long, write_short
from make_zip_corpus import modes, times, tree, unlock

SEVEN_ZIP = ["7zz", "a", "-bd", "-snl", "-mtm=on", "-mtc=off", "-mta=off"]
BSDTAR = ["bsdtar", "--uid", "0", "--gid", "0", "--uname", "", "--gname", "", "--format", "7zip"]

# (name, extra 7zz switches)
SEVEN_ZIP_WRITERS = [
    ("lzma2", []),
    ("lzma2-ultra", ["-mx9"]),
    ("lzma2-blocks", ["-mx5", "-mmt=4", "-md=64k", "-ms=on"]),
    ("lzma", ["-m0=LZMA"]),
    ("copy", ["-mx0"]),
    ("deflate", ["-m0=Deflate"]),
    ("bzip2", ["-m0=BZip2"]),
    ("ppmd", ["-m0=PPMd"]),
    ("nonsolid", ["-ms=off"]),
    ("solid-per-2-files", ["-ms=2f"]),
    ("plain-header", ["-mhc=off"]),
    ("bcj", ["-mf=BCJ"]),
    ("arm64", ["-mf=ARM64"]),
    ("delta", ["-mf=Delta:4"]),
    ("bcj2", ["-mf=BCJ2"]),
    ("bcj2-bound", ["-m0=BCJ2", "-m1=LZMA", "-m2=LZMA", "-m3=LZMA", "-mb0:1", "-mb0s1:2", "-mb0s2:3"]),
]

# Branch filters also run over x86-like code, so their call and jump streams are not empty.
CODE_WRITERS = ["bcj", "bcj2", "bcj2-bound"]

# Archives the native reader refuses, with the words its error must contain.
REFUSED_WRITERS = [
    ("encrypted", ["-pzipper", "-mhe=off"], "encrypted"),
    ("lzma-lc8", ["-m0=LZMA:lc=8"], "liblzma does not accept"),
]

BSDTAR_WRITERS = [
    ("lzma1", []),
    ("store", ["--options", "7zip:compression=store"]),
    ("deflate", ["--options", "7zip:compression=deflate"]),
    ("lzma2", ["--options", "7zip:compression=lzma2"]),
]


def write_links(root):
    """Symlinks and a hard link. 7-Zip ignores symlinks that climb with `..`,
    so these point sideways and down."""
    (root / "text").mkdir(parents=True)
    (root / "text/readme.txt").write_bytes(b"linked\n")
    (root / "text/original.txt").write_bytes(b"one inode, two names\n")
    os.link(root / "text/original.txt", root / "text/second.txt")
    os.symlink("text/readme.txt", root / "file-link")
    os.symlink("text", root / "dir-link")
    os.symlink("file-link", root / "chain")
    os.symlink("readme.txt", root / "text/sibling-link")
    finish(root)


def write_code(root):
    """Bytes shaped like x86 code: calls, jumps and conditional jumps with
    near targets among filler, so BCJ and BCJ2 convert many of them."""
    root.mkdir(parents=True)
    state, code = 1, bytearray()
    while len(code) < 300_000:
        state = (state * 1103515245 + 12345) % 2**31
        pick, target = state % 7, (state >> 8) % 4096
        if pick == 0:
            code += b"\xe8" + target.to_bytes(4, "little")
        elif pick == 1:
            code += b"\xe9" + (-target % 2**32).to_bytes(4, "little")
        elif pick == 2:
            code += bytes([0x0F, 0x80 | state % 16]) + target.to_bytes(4, "little")
        else:
            code += bytes([0x48, 0x89, state % 256, 0xC3][: 1 + pick % 4])
    (root / "code.bin").write_bytes(bytes(code))
    (root / "tail-e8.bin").write_bytes(b"\x90\x90\xe8\x01\x02")
    finish(root)


def run(command, cwd):
    subprocess.run(command, cwd=cwd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   env={"COPYFILE_DISABLE": "1", "PATH": "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin"})


def seven_zip_writer(switches):
    return lambda target, cwd: run([*SEVEN_ZIP, *switches, str(target), "src"], cwd)


def bsdtar_writer(switches):
    return lambda target, cwd: run([*BSDTAR, *switches, "-cf", str(target), "src"], cwd)


def readers():
    return [
        ("7zip", lambda archive, out: run(["7zz", "x", "-y", "-bd", "-snl", f"-o{out}", str(archive)], out.parent)),
        ("bsdtar", lambda archive, out: (out.mkdir(), run(["bsdtar", "-xpf", str(archive), "-C", str(out)], out.parent))),
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("tests/data/7z-corpus"))
    args = parser.parse_args()
    for tool in ["7zz", "bsdtar"]:
        if shutil.which(tool) is None:
            sys.exit(f"{tool} is required")
    args.output.mkdir(parents=True, exist_ok=True)
    for old in args.output.iterdir():
        old.unlink()
    summary = {"writers": {}, "readers": [name for name, _ in readers()]}
    manifests, all_modes, all_times, refused = {}, {}, {}, []
    with tempfile.TemporaryDirectory(prefix="zipper-7z-corpus-") as work:
        work = Path(work)
        write_short(work / "short" / "src")
        write_links(work / "links" / "src")
        write_long(work / "long" / "src")
        write_code(work / "code" / "src")
        try:
            jobs = [(f"7zip-{name}-{shape}", seven_zip_writer(switches), work / shape)
                    for name, switches in SEVEN_ZIP_WRITERS for shape in ["short", "links", "long"]]
            jobs += [(f"7zip-{name}-code", seven_zip_writer(dict(SEVEN_ZIP_WRITERS)[name]), work / "code")
                     for name in CODE_WRITERS]
            jobs += [(f"bsdtar-{name}-{shape}", bsdtar_writer(switches), work / shape)
                     for name, switches in BSDTAR_WRITERS for shape in ["short", "long"]]
            for name, write, source in jobs:
                build(args, work, summary, manifests, all_modes, all_times, name, write, source)
            for name, switches, error in REFUSED_WRITERS:
                archive = args.output / f"refused-{name}.7z"
                seven_zip_writer(switches)(archive.resolve(), work / "short")
                refused.append((archive.name, error))
        finally:
            unlock(work)
    write_manifest(args, summary, manifests, all_modes, all_times, refused)


def build(args, work, summary, manifests, all_modes, all_times, name, write, source):
    archive = args.output / f"{name}.7z"
    try:
        write(archive.resolve(), source)
    except Exception as error:
        summary["writers"][name] = {"refused": type(error).__name__}
        archive.unlink(missing_ok=True)
        return
    trees, reader_modes, reader_times = {}, {}, {}
    for reader, extract in readers():
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
    agreed = len({json.dumps(t, sort_keys=True) for t in trees.values() if isinstance(t, dict)}) == 1 \
        and all(isinstance(t, dict) for t in trees.values())
    summary["writers"][name] = {"readers": {r: ("ok" if isinstance(t, dict) else t) for r, t in trees.items()},
                                "agreed": agreed}
    if not agreed:
        archive.unlink()
        print(f"{name}: 7-Zip and bsdtar disagree; excluded", file=sys.stderr)
        return
    manifests[name] = trees["7zip"]
    source_modes, source_times = modes(source), times(source)
    all_modes[name] = {path: mode for path, mode in source_modes.items()
                       if reader_modes["bsdtar"].get(path) == mode}
    all_times[name] = {path: value for path, value in source_times.items()
                       if reader_times["bsdtar"].get(path) == value}


def write_manifest(args, summary, manifests, all_modes, all_times, refused):
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
    (args.output / "refused.tsv").write_text("".join(f"{name}\t{error}\n" for name, error in refused))
    (args.output / "corpus.json").write_text(json.dumps(summary, indent=1, sort_keys=True) + "\n")
    print(f"{len(manifests)} archives agreed by 7-Zip and bsdtar; {len(refused)} refused", file=sys.stderr)


if __name__ == "__main__":
    main()
