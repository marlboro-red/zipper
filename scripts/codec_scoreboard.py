#!/usr/bin/env python3
"""Measure one Silesia archive per codec: Zipper at one thread against 7-Zip or bsdtar at one thread.

`build` writes the archives into a folder. `run` extracts each with Zipper and with its reference
through `compare_zip_readers.py`, byte-checked, and writes one JSON per codec.
"""
import argparse
import datetime
import json
import lzma
import struct
import subprocess
import sys
import tarfile
import zlib
from pathlib import Path

SILESIA = Path("target/bench-corpora/silesia/silesia")
LZIP_DICTIONARY_BITS = 23

# Name, container, how to build it, reference reader mode, reference tool.
CODECS = [
    ("zip-deflate64", "zip", ("7zip-zip", "Deflate64"), "7zip", "7zz"),
    ("zip-bzip2", "zip", ("7zip-zip", "BZip2"), "7zip", "7zz"),
    ("zip-lzma", "zip", ("7zip-zip", "LZMA"), "7zip", "7zz"),
    ("zip-ppmd", "zip", ("7zip-zip", "PPMd"), "7zip", "7zz"),
    ("zip-xz", "zip", ("7zip-zip", "XZ"), "7zip", "7zz"),
    ("zip-zstd", "zip", ("zstd-zip", None), None, None),
    ("7z-bzip2", "7z", ("7zip-7z", "-m0=BZip2"), "7zip", "7zz"),
    ("7z-deflate64", "7z", ("7zip-7z", "-m0=Deflate64"), "7zip", "7zz"),
    ("7z-ppmd", "7z", ("7zip-7z", "-m0=PPMd"), "7zip", "7zz"),
    ("7z-bcj2", "7z", ("7zip-7z", "-mf=BCJ2"), "7zip", "7zz"),
    ("7z-swap4", "7z", ("7zip-7z", "-mf=Swap4"), "7zip", "7zz"),
    ("tar-bzip2", "tar", ("tar", ["bzip2", "-9", "-c"]), "7zip-tar", "7zz"),
    ("tar-compress", "tar", ("tar", ["compress", "-c"]), "7zip-tar", "7zz"),
    ("tar-lzip", "tar", ("tar", "lzip"), "bsdtar", "bsdtar"),
    ("tar-lz4", "tar", ("tar", ["lz4", "-9", "-c"]), "bsdtar", "bsdtar"),
]
EXTENSIONS = {"zip": "zip", "7z": "7z", "tar-bzip2": "tar.bz2", "tar-compress": "tar.Z",
              "tar-lzip": "tar.lz", "tar-lz4": "tar.lz4"}


def extension(name, container):
    return EXTENSIONS.get(name, EXTENSIONS.get(container))


def zstd_zip(source, archive):
    """ZIP method 93, one Zstandard frame per file, written by the `zstd` tool."""
    out, central = bytearray(), bytearray()
    files = sorted(path for path in source.rglob("*") if path.is_file())
    for path in files:
        name = f"{source.name}/{path.relative_to(source).as_posix()}".encode()
        data = path.read_bytes()
        packed = subprocess.run(["zstd", "-q", "-c", "-3"], input=data, capture_output=True, check=True).stdout
        fields = struct.pack("<HHHHHIIIHH", 20, 0, 93, 0, 0x21, zlib.crc32(data), len(packed), len(data), len(name), 0)
        offset = len(out)
        out += b"PK\x03\x04" + fields + name + packed
        central += b"PK\x01\x02" + struct.pack("<H", 20) + fields + struct.pack("<HHHII", 0, 0, 0, 0, offset) + name
    start = len(out)
    out += central + b"PK\x05\x06" + struct.pack("<HHHHIIH", 0, 0, len(files), len(files), len(central), start, 0)
    archive.write_bytes(out)


def lzip(data):
    """One lzip member: header, raw LZMA with an end marker, then CRC-32, data size and member size."""
    packed = lzma.compress(data, format=lzma.FORMAT_RAW, filters=[{
        "id": lzma.FILTER_LZMA1, "dict_size": 1 << LZIP_DICTIONARY_BITS, "lc": 3, "lp": 0, "pb": 2, "preset": 6}])
    member = 6 + len(packed) + 20
    return (b"LZIP\x01" + bytes([LZIP_DICTIONARY_BITS]) + packed
            + struct.pack("<IQQ", zlib.crc32(data), len(data), member))


def anonymous(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info


def build(out):
    source = SILESIA.resolve(strict=True)
    out = out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    plain = out / "silesia.tar"
    with tarfile.open(plain, "w", format=tarfile.USTAR_FORMAT) as tar:
        tar.add(source, arcname=source.name, filter=anonymous)
    for name, container, (kind, option), _, _ in CODECS:
        archive = out / f"{name}.{extension(name, container)}"
        archive.unlink(missing_ok=True)
        if kind == "7zip-zip":
            subprocess.run(["7zz", "a", "-tzip", f"-mm={option}", "-mmt1", "-bd", "-bso0", str(archive), source.name],
                           cwd=source.parent, check=True)
        elif kind == "7zip-7z":
            subprocess.run(["7zz", "a", "-t7z", option, "-mmt1", "-bd", "-bso0", str(archive), source.name],
                           cwd=source.parent, check=True)
        elif kind == "zstd-zip":
            zstd_zip(source, archive)
        elif option == "lzip":
            archive.write_bytes(lzip(plain.read_bytes()))
        else:
            with plain.open("rb") as tar_input, archive.open("wb") as output:
                subprocess.run(option, stdin=tar_input, stdout=output, check=True)
        print(f"{archive.name}: {archive.stat().st_size} bytes", file=sys.stderr)


def dirty_paths():
    status = subprocess.check_output(["git", "status", "--porcelain"], text=True)
    return [line for line in status.splitlines()
            if not line[3:].startswith(("benchmarks/scoreboard/", "docs/results.md"))]


def run(args):
    if dirty_paths() and not args.allow_dirty:
        sys.exit("commit first; each JSON names the measured revision")
    source = SILESIA.resolve(strict=True)
    args.out.mkdir(parents=True, exist_ok=True)
    names = args.only or [codec[0] for codec in CODECS]
    for name, container, _, mode, tool in CODECS:
        if name not in names:
            continue
        archive = args.archives / f"{name}.{extension(name, container)}"
        readers = ["--reader", "zipper", "native", str(args.zipper)]
        if mode:
            readers += ["--reader", tool, mode, str(Path(subprocess.check_output(["which", tool], text=True).strip()))]
        result = subprocess.run([sys.executable, "scripts/compare_zip_readers.py", str(archive), str(source),
                                 "--archive-format", container, "--runs", str(args.runs), "--threads", "1", *readers],
                                capture_output=True, text=True, check=True)
        data = json.loads(result.stdout)
        data["codec"] = name
        data["reference"] = tool
        data["revision"] = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
        data["recorded"] = datetime.date.today().isoformat()
        data["tree_clean"] = not dirty_paths()
        (args.out / f"{name}.json").write_text(json.dumps(data, indent=2) + "\n")
        rows = data["results"]
        summary = ", ".join(f"{label} {row['median_seconds'] * 1000:.0f} ms" for label, row in rows.items())
        print(f"{name}: {summary}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    built = commands.add_parser("build")
    built.add_argument("out", type=Path)
    ran = commands.add_parser("run")
    ran.add_argument("archives", type=Path)
    ran.add_argument("--zipper", type=Path, required=True)
    ran.add_argument("--runs", type=int, default=5)
    ran.add_argument("--out", type=Path, default=Path("benchmarks/scoreboard/codecs"))
    ran.add_argument("--only", nargs="*")
    ran.add_argument("--allow-dirty", action="store_true", help="for trials; the JSON records tree_clean false")
    args = parser.parse_args()
    if args.command == "build":
        build(args.out)
    else:
        run(args)


if __name__ == "__main__":
    main()
