#!/usr/bin/env python3
"""Writes the SquashFS, CramFS and LZO corpus in this folder.

    python3 tests/data/squashfs-corpus/generate.py

It needs `mksquashfs` 4.x from squashfs-tools, `mkfs.cramfs` from util-linux
and `lzop`. On macOS, `brew install squashfs util-linux lzop` installs them;
`mkfs.cramfs` is then in /opt/homebrew/opt/util-linux/sbin. Every file it
writes is a pure function of the source tree below, with fixed times, so a
rerun writes the same bytes when the tools are the same versions."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
MKCRAMFS = shutil.which("mkfs.cramfs") or "/opt/homebrew/opt/util-linux/sbin/mkfs.cramfs"
TIME = 1_600_000_000
COMPRESSORS = ["gzip", "lzma", "lzo", "xz", "lz4", "zstd"]
PSEUDO = [
    "cdev C {t} 640 0 0 1 2",
    "bdev B {t} 600 0 0 8 1",
    "pipe I {t} 600 0 0 f",
    "sock I {t} 700 0 0 s",
    "sticky D {t} 1777 0 0",
    "locked D {t} 500 0 0",
    "locked/inside F {t} 400 0 0 echo inside",
    "setuid F {t} 4755 0 0 echo setuid",
]


def words(length, seed):
    state = seed | 1
    out = bytearray()
    vocabulary = [f"word{index}{'x' * (index % 7)} ".encode() for index in range(64)]
    while len(out) < length:
        state ^= (state << 13) & 0xFFFFFFFF
        state ^= state >> 17
        state ^= (state << 5) & 0xFFFFFFFF
        if state % 11 == 0:
            out += b"\n"
        out += vocabulary[(state >> 8) % 64]
    return bytes(out[:length])


def noise(length, seed):
    state = seed | 1
    out = bytearray()
    while len(out) < length:
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        if state >> 60 == 0:
            out += bytes([state >> 52 & 0xFF]) * (state >> 40 & 0x3FF)
        else:
            out += (state >> 32).to_bytes(4, "little")
    return bytes(out[:length])


def write_source(root):
    """A tree with every entry kind a writer keeps: files that fill whole blocks, end in a
    fragment, hold zeros or repeat each other, hard links, links, empty files and folders."""
    (root / "folder" / "deeper" / "deepest").mkdir(parents=True)
    (root / "empty-folder").mkdir()
    (root / "hello.txt").write_bytes(b"hello, squashfs\n")
    (root / "words.txt").write_bytes(words(100_000, 7))
    (root / "noise.bin").write_bytes(noise(30_000, 3))
    (root / "zeros.bin").write_bytes(bytes(70_000))
    (root / "empty.txt").write_bytes(b"")
    (root / "folder" / "copy.txt").write_bytes(words(100_000, 7))
    (root / "folder" / "tail.txt").write_bytes(words(5_000, 11))
    (root / "folder" / "deeper" / "deepest" / "leaf.txt").write_bytes(b"leaf\n")
    for index in range(40):
        (root / "folder" / f"small-{index:02}.txt").write_bytes(words(index * 37 + 1, index))
    os.link(root / "hello.txt", root / "folder" / "hard-link.txt")
    os.symlink("hello.txt", root / "link-to-hello")
    os.symlink("deeper/deepest/leaf.txt", root / "folder" / "link-to-leaf")
    os.symlink("missing-target", root / "dangling")
    os.mkfifo(root / "folder" / "fifo")
    (root / "words.txt").chmod(0o600)
    (root / "folder" / "deeper").chmod(0o750)
    (root / "noise.bin").chmod(0o755)
    for path in [root, *root.rglob("*")]:
        os.utime(path, (TIME, TIME + sum(path.name.encode()) * 97), follow_symlinks=False)


def run(command):
    subprocess.run(command, check=True, capture_output=True)


def squashfs(source, out, *options):
    pseudo = [part for line in PSEUDO for part in ("-p", line.format(t=TIME + 5))]
    run(["mksquashfs", str(source), str(out), "-noappend", "-no-xattrs", "-quiet",
         "-no-progress", "-mkfs-time", str(TIME), *pseudo, *options])


def main():
    lzo = HERE / "lzo"
    lzo.mkdir(exist_ok=True)
    (lzo / "words.txt").write_bytes(words(60_000, 5))
    (lzo / "noise.bin").write_bytes(noise(40_000, 9))
    for source in ["words.txt", "noise.bin"]:
        for level in [1, 3, 7, 9]:
            packed = subprocess.run(["lzop", f"-{level}", "-n", "-c", str(lzo / source)],
                                    check=True, capture_output=True).stdout
            stem, _, extension = source.partition(".")
            (lzo / f"{stem}.{extension}.{level}.lzo").write_bytes(packed)
    with tempfile.TemporaryDirectory() as work:
        source = Path(work) / "source"
        source.mkdir()
        write_source(source)
        for compressor in COMPRESSORS:
            squashfs(source, HERE / f"{compressor}.sqfs", "-comp", compressor, "-b", "4096")
        squashfs(source, HERE / "xz-x86-1m.sqfs", "-comp", "xz", "-Xbcj", "x86", "-b", "1M")
        squashfs(source, HERE / "uncompressed.sqfs", "-no-compression", "-b", "4096")
        squashfs(source, HERE / "no-fragments.sqfs", "-no-fragments", "-b", "8192")
        squashfs(source, HERE / "always-fragments.sqfs", "-always-use-fragments", "-b", "16384")
        squashfs(source, HERE / "no-duplicates-exports.sqfs", "-no-duplicates", "-no-exports")
        squashfs(source, HERE / "lz4-hc-zstd-options.sqfs", "-comp", "lz4", "-Xhc")
        for endian in ["little", "big"]:
            run([MKCRAMFS, "-b", "4096", "-N", endian, "-n", "zipper", str(source),
                 str(HERE / f"cramfs-{endian}.cramfs")])
        run([MKCRAMFS, "-b", "4096", "-z", str(source), str(HERE / "cramfs-holes.cramfs")])


if __name__ == "__main__":
    main()
