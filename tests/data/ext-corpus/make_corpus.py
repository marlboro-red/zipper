#!/usr/bin/env python3
"""Write the ext corpus: small ext2, ext3 and ext4 images made with `mke2fs -d`
from one fixed source tree, compressed with `xz -9`, and `trees.json`, the tree
`7zz x` extracts from each image with a umask of 0, as the entries all images
share and each image's own. 7-Zip cannot open an image with an inline folder
or one sparse block-mapped file here, so those trees come from the source tree
under 7-Zip's mode rules and cover only the source paths. Run it from any folder with e2fsprogs,
xz and 7-Zip 26 or later installed; it rewrites this folder's images."""
import hashlib
import json
import os
import random
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SEARCH = ["/opt/homebrew/opt/e2fsprogs/sbin", "/usr/local/opt/e2fsprogs/sbin", "/usr/sbin", "/sbin"]

IMAGES = [
    ("ext2-1k.img", ["-t", "ext2", "-b", "1024", "-I", "128"], "8M", False),
    ("ext2-triple-indirect.img", ["-t", "ext2", "-b", "1024", "-I", "128"], "8M", True),
    ("ext3-2k.img", ["-t", "ext3", "-b", "2048"], "12M", False),
    ("ext4.img", ["-t", "ext4", "-b", "1024"], "12M", False),
    ("ext4-4k.img", ["-t", "ext4", "-b", "4096"], "16M", False),
    ("ext4-block-maps.img", ["-t", "ext4", "-b", "1024", "-O", "^extent,^64bit"], "12M", False),
    ("ext4-32bit.img", ["-t", "ext4", "-b", "1024", "-O", "^64bit"], "12M", False),
    ("ext4-inline.img", ["-t", "ext4", "-b", "1024", "-O", "inline_data"], "12M", False),
    ("ext4-uninit-bg.img", ["-t", "ext4", "-b", "1024", "-O", "^metadata_csum,uninit_bg"], "12M", False),
    ("ext4-meta-bg.img", ["-t", "ext4", "-b", "1024", "-O", "meta_bg,^resize_inode"], "12M", False),
    ("ext4-quota.img", ["-t", "ext4", "-b", "1024", "-O", "quota,project"], "12M", False),
    ("ext4-64k.img", ["-t", "ext4", "-b", "65536"], "96M", False),
]

FIXED_TIME = 981_173_106


def tool(name):
    found = shutil.which(name)
    if found:
        return found
    for folder in SEARCH:
        candidate = Path(folder) / name
        if candidate.is_file():
            return str(candidate)
    sys.exit(f"{name} is not installed; install e2fsprogs")


def write(path, data, mode=0o644, mtime=FIXED_TIME):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    os.chmod(path, mode)
    os.utime(path, (mtime, mtime))


def source(root, far):
    rng = random.Random(1234)
    write(root / "hello.txt", b"hello from ext\n")
    write(root / "empty.txt", b"")
    for size in (59, 60, 61, 120, 200):
        write(root / "inline" / f"s{size}.txt", bytes([97 + size % 26]) * size)
    write(root / "noise.bin", bytes(rng.getrandbits(8) for _ in range(5000)))
    pattern = bytes(rng.getrandbits(8) for _ in range(1024))
    write(root / "medium.bin", b"".join(pattern[index % 1024:] + pattern[:index % 1024]
                                        for index in range(300)))
    islands = bytearray(399 * 8192 + 1024)
    for index in range(400):
        islands[index * 8192:index * 8192 + 1024] = bytes([index % 251 + 1]) * 1024
    write(root / "islands.bin", bytes(islands))
    if far:
        with open(root / "far.bin", "wb") as handle:
            handle.seek(70 * 1024 * 1024)
            handle.write(b"far past the triple indirect block\n" * 100)
        os.utime(root / "far.bin", (FIXED_TIME, FIXED_TIME))
    write(root / "exec.sh", b"#!/bin/sh\necho hi\n", 0o755)
    write(root / "readonly.txt", b"read only\n", 0o444)
    write(root / "private.txt", b"private\n", 0o600)
    write(root / "setuid", b"setuid\n", 0o4755)
    write(root / "old.txt", b"before 1970\n", mtime=-315_619_200)
    write(root / "late.txt", b"late\n", mtime=2_000_000_000)
    os.link(root / "hello.txt", root / "hello-again.txt")
    write(root / "folder" / "inner.txt", b"inner\n")
    os.link(root / "noise.bin", root / "folder" / "noise-link.bin")
    os.link(root / "noise.bin", root / "noise-third.bin")
    os.symlink("hello.txt", root / "link")
    os.symlink("folder/" + "x" * 90, root / "long-link")
    os.symlink("folder", root / "folder-link")
    os.mkfifo(root / "pipe")
    os.chmod(root / "pipe", 0o640)
    os.utime(root / "pipe", (FIXED_TIME, FIXED_TIME))
    for name in ("café.txt", "日本語.txt", "emoji 😀.txt"):
        write(root / "names" / name, name.encode())
    for index in range(300):
        write(root / "many" / f"file-{index:04}.txt", f"file {index}\n".encode())
    deep = root / "deep"
    for level in range(30):
        deep = deep / f"d{level}"
    write(deep / "bottom.txt", b"bottom\n")
    (root / "empty-folder").mkdir()
    write(root / "locked" / "inside.txt", b"inside\n")
    write(root / "tight" / "inside.txt", b"inside\n")
    folders = sorted((p for p in root.rglob("*") if p.is_dir() and not p.is_symlink()),
                     key=lambda p: (-len(p.parts), str(p)))
    for number, folder in enumerate(folders):
        os.utime(folder, (FIXED_TIME + number, FIXED_TIME + number))
    os.chmod(root / "locked", 0o555)
    os.chmod(root / "tight", 0o750)


def capture(root, synthetic=("[SYS]", "[UNKNOWN]")):
    entries = {}
    metadata = {}
    for folder, folders, files in os.walk(root):
        for name in folders + files:
            path = Path(folder) / name
            metadata[path] = os.lstat(path)
        for name in folders:
            path = Path(folder) / name
            os.chmod(path, metadata[path].st_mode | 0o700)
    groups = {}
    for path, info in metadata.items():
        if stat.S_ISREG(info.st_mode) and info.st_nlink > 1:
            key = (info.st_dev, info.st_ino)
            groups[key] = min(groups.get(key, "\uffff"), str(path.relative_to(root)))
    for path, info in sorted(metadata.items()):
        name = str(path.relative_to(root))
        mode = stat.S_IMODE(info.st_mode)
        if stat.S_ISLNK(info.st_mode):
            entries[name] = {"kind": "link:" + os.readlink(path)}
        elif stat.S_ISDIR(info.st_mode):
            entries[name] = {"kind": "dir", "mode": mode}
            if name not in synthetic:
                entries[name]["mtime"] = info.st_mtime_ns
        else:
            entries[name] = {
                "kind": "file:" + hashlib.sha256(path.read_bytes()).hexdigest(),
                "mode": mode,
                "mtime": info.st_mtime_ns,
            }
            if (info.st_dev, info.st_ino) in groups:
                entries[name]["group"] = groups[(info.st_dev, info.st_ino)]
    for path in metadata:
        if stat.S_ISDIR(metadata[path].st_mode):
            os.chmod(path, metadata[path].st_mode)
    return entries


def from_source(root):
    entries = {}
    groups = {}
    paths = sorted(root.rglob("*"))
    for path in paths:
        info = os.lstat(path)
        if stat.S_ISREG(info.st_mode) and info.st_nlink > 1:
            key = (info.st_dev, info.st_ino)
            groups[key] = min(groups.get(key, "\uffff"), str(path.relative_to(root)))
    for path in paths:
        info = os.lstat(path)
        name = str(path.relative_to(root))
        mtime = int(info.st_mtime) * 1_000_000_000
        if stat.S_ISLNK(info.st_mode):
            entries[name] = {"kind": "link:" + os.readlink(path)}
        elif stat.S_ISDIR(info.st_mode):
            entries[name] = {"kind": "dir", "mode": (stat.S_IMODE(info.st_mode) | 0o700) & 0o777,
                             "mtime": mtime}
        elif stat.S_ISREG(info.st_mode):
            entries[name] = {"kind": "file:" + hashlib.sha256(path.read_bytes()).hexdigest(),
                             "mode": stat.S_IMODE(info.st_mode) & 0o777, "mtime": mtime}
            if (info.st_dev, info.st_ino) in groups:
                entries[name]["group"] = groups[(info.st_dev, info.st_ino)]
        else:
            entries[name] = {"kind": "file:" + hashlib.sha256(b"").hexdigest(), "mode": 0o666,
                             "mtime": mtime}
    return entries


def main():
    mke2fs = tool("mke2fs")
    sevenzip = shutil.which("7zz") or sys.exit("7zz is not installed")
    os.umask(0)
    trees = {}
    with tempfile.TemporaryDirectory() as work:
        work = Path(work)
        for name, options, size, far in IMAGES:
            src = work / f"{name}.src"
            source(src, far)
            image = work / name
            subprocess.run([mke2fs, "-q", "-F", "-U", "6c6f7265-6d2d-6970-7375-6d2d65787434",
                            "-E", "hash_seed=6c6f7265-6d2d-6970-7375-6d2d65787435"]
                           + options + ["-d", str(src), str(image), size],
                           check=True)
            out = work / f"{name}.7z"
            run = subprocess.run([sevenzip, "x", "-y", f"-o{out}", str(image)], capture_output=True)
            if run.returncode == 0:
                trees[name] = {"source": "7zz x", "tree": capture(out)}
            else:
                trees[name] = {"source": "source tree", "tree": from_source(src)}
            packed = HERE / f"{name}.xz"
            with open(packed, "wb") as handle:
                subprocess.run(["xz", "-9", "-c", str(image)], stdout=handle, check=True)
            print(name, trees[name]["source"], len(trees[name]["tree"]), packed.stat().st_size)
    shared = None
    for image in trees.values():
        items = set((path, json.dumps(node, sort_keys=True)) for path, node in image["tree"].items())
        shared = items if shared is None else shared & items
    common = {path: json.loads(node) for path, node in shared}
    for image in trees.values():
        image["missing"] = sorted(path for path in common if path not in image["tree"])
        image["tree"] = {path: node for path, node in image["tree"].items() if common.get(path) != node}
    record = {"common": common, "images": trees}
    (HERE / "trees.json").write_text(json.dumps(record, indent=1, sort_keys=True, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
