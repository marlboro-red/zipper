#!/usr/bin/env python3
"""Build the RAR writer corpus in tests/data/rar-written.

RARLAB's own `rar` writes every archive, run as a black box. RAR 7 writes
RAR 5 archives only, so a RAR 6 `rar` writes the RAR 4 ones with `-ma4`.
RARLAB's `unrar` extracts each archive, and its tree must equal the source
tree the script wrote, or the script stops. manifest.json then records, for
each archive, the command that wrote it, the features it covers, and either
the tree every reader must extract or the reason Zipper must give when it
refuses the archive. tests/rar_written.rs checks Zipper against the manifest
without any RAR tool, and against `unrar` and 7-Zip where they are installed.

unrar runs with `-ver`, so it extracts old versions too. RAR 4 keeps DOS times in local time, so its trees record no times. RAR 4
keeps only the low 16 bits of a character outside the Basic Multilingual
Plane, as unrar shows, and its trees spell names that way. A version
record names a file `name;N` in unrar and `[VER]/N/name` in 7-Zip and Zipper,
and the tree keeps 7-Zip's name.

Usage: make_rar_corpus.py --rar4 PATH/TO/rar-6.x [--rar rar] [--unrar unrar]

RARLAB's downloads rarmacos-arm-723.tar.gz and rarmacos-arm-624.tar.gz from
https://www.rarlab.com/rar/ hold the tools. Homebrew disabled its rar cask
on 1 September 2026.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import stat
import subprocess
import sys
import tempfile
import unicodedata

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "tests" / "data" / "rar-written"
PASSWORD = "zipper"
SEED = 20261114
FIRST_TIME = 1_577_934_245
LONG_NAME = "long-" + "n" * 195 + ".txt"
LONG_PATH = "/".join(f"level-{index:02}-" + "p" * 40 for index in range(12))


def words(rng, count):
    vocabulary = ("archive header block folder window filter delta table match "
                  "literal length offset stream solid volume record checksum "
                  "the of and to a in is that for it with as on").split()
    return " ".join(rng.choice(vocabulary) for _ in range(count))


def prose(rng, size):
    lines = []
    while sum(len(line) for line in lines) < size:
        lines.append(words(rng, rng.randrange(6, 14)).capitalize() + ".\n")
    return "".join(lines).encode()[:size]


def x86_code(rng, size):
    """Bytes shaped like x86 code: short instructions and many E8 and E9
    calls to a few fixed targets, which the x86 filter makes repeat."""
    targets = [rng.randrange(0, size) for _ in range(48)]
    plain = [bytes([0x55]), bytes([0x48, 0x89, 0xE5]), bytes([0x8B, 0x45, 0xFC]),
             bytes([0x5D, 0xC3]), bytes([0x31, 0xC0]), bytes([0x48, 0x83, 0xEC, 0x20])]
    data = bytearray()
    while len(data) < size:
        if rng.random() < 0.3:
            opcode = 0xE8 if rng.random() < 0.8 else 0xE9
            relative = (rng.choice(targets) - (len(data) + 5)) & 0xFFFFFFFF
            data += bytes([opcode]) + relative.to_bytes(4, "little")
        else:
            data += rng.choice(plain)
    return bytes(data[:size])


def audio(size, channels=2):
    """16-bit samples of slow sine waves, one per channel, which the delta
    and audio filters predict."""
    data = bytearray()
    step = 0
    while len(data) < size:
        for channel in range(channels):
            sample = int(12000 * math.sin(step / (40 + 13 * channel)))
            data += (sample & 0xFFFF).to_bytes(2, "little")
        step += 1
    return bytes(data[:size])


def image(width, height):
    """24-bit pixels of smooth gradients, which the RGB filter predicts."""
    return bytes(
        component
        for y in range(height)
        for x in range(width)
        for component in ((x * 255) // width, (y * 255) // height, ((x + y) * 127) // (width + height))
    )


def far_repeats(rng, size, distance):
    """A random block that repeats `distance` bytes later, with runs of one
    byte between, so a match must reach back about `distance` bytes."""
    block = bytes(rng.getrandbits(8) for _ in range(16384))
    runs = (distance - len(block)) // 4096
    filler = b"".join(bytes([index % 251]) * 4096 for index in range(runs))
    data = bytearray()
    while len(data) < size:
        data += block + filler
    return bytes(data[:size])


def set_times(root):
    paths = sorted(root.rglob("*"), key=lambda path: (path.is_dir(), -len(path.parts), str(path)))
    for index, path in enumerate(paths):
        seconds = FIRST_TIME + index * 7_919
        nanoseconds = seconds * 1_000_000_000 + (index * 123_456_789) % 1_000_000_000
        os.utime(path, ns=(nanoseconds, nanoseconds), follow_symlinks=False)


def write_files(root, files, folders=(), links=(), modes=()):
    for name, data in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    for name in folders:
        (root / name).mkdir(parents=True, exist_ok=True)
    for name, target in links:
        (root / name).parent.mkdir(parents=True, exist_ok=True)
        os.symlink(target, root / name)
    for name, mode in modes:
        (root / name).chmod(mode)
    set_times(root)


def mixed(root):
    rng = random.Random(SEED)
    write_files(
        root,
        {
            "text/prose.txt": prose(rng, 24 * 1024),
            "text/numbers.csv": "".join(f"{n},{n * n},{n % 7}\n" for n in range(3000)).encode(),
            "binary/random-8k.bin": bytes(rng.getrandbits(8) for _ in range(8192)),
            "binary/zeros-64k-plus-1.bin": bytes(65537),
            "empty.txt": b"",
            "nested/deep/deeper/leaf.txt": b"leaf\n",
            "names/café.txt": "café\n".encode(),
            "names/日本語.txt": "日本語\n".encode(),
            "names/space and (parens).txt": b"spaces\n",
            "names/emoji \U0001f389.txt": b"emoji\n",
            f"names/{LONG_NAME}": b"long name\n",
            f"names/{LONG_PATH}/end.txt": b"long path\n",
            "modes/run.sh": b"#!/bin/sh\necho run\n",
            "modes/readonly.txt": b"read only\n",
            "modes/private.txt": b"private\n",
        },
        folders=["empty-folder"],
        links=[("links/to-leaf", "../nested/deep/deeper/leaf.txt"), ("links/to-folder", "../nested")],
        modes=[("modes/run.sh", 0o755), ("modes/readonly.txt", 0o444), ("modes/private.txt", 0o600)],
    )


def executables(root):
    rng = random.Random(SEED + 1)
    write_files(root, {"program.exe": x86_code(rng, 96 * 1024), "library.dll": x86_code(rng, 32 * 1024)})


def tables(root):
    write_files(root, {"stereo.wav": audio(96 * 1024), "mono.raw": audio(32 * 1024, channels=1)})


def media(root):
    write_files(root, {"stereo.wav": audio(96 * 1024), "gradient.bmp": image(128, 96)})


def distances(size, distance):
    def build(root):
        write_files(root, {"far.bin": far_repeats(random.Random(SEED + distance), size, distance)})
    return build


def hard_links(root):
    write_files(root, {"original.txt": b"one inode, two names\n" * 50, "other.txt": b"other\n"})
    os.link(root / "original.txt", root / "second-name.txt")
    set_times(root)


def versions(root):
    write_files(root, {"changed.txt": b"first version\n", "same.txt": b"unchanged\n"})

    def rewrite():
        (root / "changed.txt").write_bytes(b"second version, a little longer\n")
        set_times(root)
    return rewrite


def copies(root):
    data = prose(random.Random(SEED + 2), 4096).decode()
    write_files(root, {"first.txt": data.encode(), "copy.txt": data.encode()})


def many_small_texts(root):
    rng = random.Random(SEED + 3)
    write_files(root, {f"notes/note-{index:03}.txt": prose(rng, rng.randrange(200, 900)) for index in range(120)})


def single(root):
    write_files(root, {"only.bin": bytes(random.Random(SEED + 4).getrandbits(8) for _ in range(40 * 1024))})


def comment(work):
    path = work / "comment.txt"
    path.write_text("A comment written by rar -z.\n")
    return path


RAR5 = "RAR 5"
RAR4 = "RAR 4"

REFUSED_ENCRYPTED_HEADERS_5 = "RAR 5 archive has encrypted headers, which Zipper does not decrypt"
REFUSED_ENCRYPTED_HEADERS_4 = "RAR 4 archive has encrypted headers, which Zipper does not decrypt"
REFUSED_ENCRYPTED_ENTRY_5 = "is encrypted, which Zipper does not decrypt"
REFUSED_ENCRYPTED_ENTRY_4 = "is encrypted, which Zipper does not decrypt"
REFUSED_VOLUME_5 = "Zipper does not read RAR 5 volumes"
REFUSED_VOLUME_4 = "Zipper does not read RAR 4 volumes"
REFUSED_SOLID_4 = "Zipper does not read RAR 4 solid archives"
REFUSED_COPY = "is a FileCopy, which Zipper does not create"
# A gap, not a feature refusal; docs/roadmap/m18-rar.md lists it.
REFUSED_PPMD_RESTARTS = "RAR 4 PPMd models restart more often than their output justifies"


def cases(work):
    """(archive, writer, source builder, rar switches, features, refusal).
    Each archive holds the source folder `src`."""
    note = comment(work)
    levels5 = [(f"rar5-m{level}.rar", "rar", mixed, [f"-m{level}"], [RAR5, f"-m{level}"], None)
               for level in range(6)]
    levels4 = [(f"rar4-m{level}.rar", "rar4", mixed, ["-ma4", f"-m{level}"], [RAR4, f"-m{level}"], None)
               for level in range(6)]
    return levels5 + levels4 + [
        ("rar5-solid-m3.rar", "rar", mixed, ["-m3", "-s"], [RAR5, "solid"], None),
        ("rar5-solid-m5.rar", "rar", mixed, ["-m5", "-s"], [RAR5, "solid"], None),
        ("rar5-solid-small-texts.rar", "rar", many_small_texts, ["-m5", "-s"], [RAR5, "solid", "many files"], None),
        ("rar5-md1m.rar", "rar", distances(1536 * 1024, 768 * 1024), ["-m3", "-md1m"], [RAR5, "dictionary 1 MiB"], None),
        ("rar5-md4m.rar", "rar", distances(6 * 1024 * 1024, 3 * 1024 * 1024), ["-m3", "-md4m"], [RAR5, "dictionary 4 MiB"], None),
        ("rar5-md16m.rar", "rar", distances(24 * 1024 * 1024, 12 * 1024 * 1024), ["-m5", "-md16m"], [RAR5, "dictionary 16 MiB"], None),
        ("rar5-x86-auto.rar", "rar", executables, ["-m3"], [RAR5, "x86 filter, chosen by rar"], None),
        ("rar5-x86-forced.rar", "rar", executables, ["-m3", "-mcE+"], [RAR5, "x86 filter"], None),
        ("rar5-delta-auto.rar", "rar", tables, ["-m3"], [RAR5, "delta filter, chosen by rar"], None),
        ("rar5-delta-forced.rar", "rar", tables, ["-m3", "-mc4D+"], [RAR5, "delta filter"], None),
        ("rar5-exhaustive.rar", "rar", mixed, ["-m5", "-mcx"], [RAR5, "long range and exhaustive search"], None),
        ("rar5-blake2.rar", "rar", mixed, ["-m3", "-htb"], [RAR5, "BLAKE2sp"], None),
        ("rar5-all-times.rar", "rar", mixed, ["-m3", "-tsm", "-tsc", "-tsa"], [RAR5, "file times"], None),
        ("rar5-owners.rar", "rar", mixed, ["-m3", "-ow"], [RAR5, "owners"], None),
        ("rar5-quick-open.rar", "rar", mixed, ["-m3", "-qo+"], [RAR5, "quick open record"], None),
        ("rar5-hard-links.rar", "rar", hard_links, ["-m3", "-oh"], [RAR5, "hard links"], None),
        ("rar5-comment.rar", "rar", mixed, ["-m3", f"-z{note}"], [RAR5, "archive comment"], None),
        ("rar5-recovery.rar", "rar", mixed, ["-m3", "-rr5%"], [RAR5, "recovery record"], None),
        ("rar5-by-rar6.rar", "rar4", mixed, ["-ma5", "-m5", "-s"], [RAR5, "solid", "RAR 6 writer"], None),
        ("rar5-versions.rar", "rar", versions, ["-m3", "-ver"], [RAR5, "file versions"], None),
        ("rar5-file-copies.rar", "rar", copies, ["-m3", "-oi1:1"], [RAR5, "file copies"], REFUSED_COPY),
        ("rar5-encrypted-headers.rar", "rar", single, ["-m3", f"-hp{PASSWORD}"], [RAR5, "encrypted headers"], REFUSED_ENCRYPTED_HEADERS_5),
        ("rar5-encrypted-data.rar", "rar", single, ["-m3", f"-p{PASSWORD}"], [RAR5, "encrypted data"], REFUSED_ENCRYPTED_ENTRY_5),
        ("rar5-volumes.part1.rar", "rar", single, ["-m0", "-v24k"], [RAR5, "volumes"], REFUSED_VOLUME_5),
        ("rar4-text-ppmd.rar", "rar4", mixed, ["-ma4", "-m5", "-mcT+"], [RAR4, "PPMd"], None),
        ("rar4-text-ppmd-small-files.rar", "rar4", many_small_texts, ["-ma4", "-m5", "-mcT+"], [RAR4, "PPMd", "many files"], REFUSED_PPMD_RESTARTS),
        ("rar4-x86-auto.rar", "rar4", executables, ["-ma4", "-m3"], [RAR4, "x86 filter, chosen by rar"], None),
        ("rar4-x86-forced.rar", "rar4", executables, ["-ma4", "-m3", "-mcE+"], [RAR4, "x86 filter"], None),
        ("rar4-delta-forced.rar", "rar4", tables, ["-ma4", "-m3", "-mc4D+"], [RAR4, "delta filter"], None),
        ("rar4-audio-forced.rar", "rar4", media, ["-ma4", "-m3", "-mc4A+"], [RAR4, "audio filter"], None),
        ("rar4-rgb-forced.rar", "rar4", media, ["-ma4", "-m3", "-mcC+"], [RAR4, "RGB filter"], None),
        ("rar4-media-auto.rar", "rar4", media, ["-ma4", "-m5"], [RAR4, "audio and RGB filters, chosen by rar"], None),
        ("rar4-md4m.rar", "rar4", distances(6 * 1024 * 1024, 3 * 1024 * 1024), ["-ma4", "-m3", "-md4m"], [RAR4, "dictionary 4 MiB"], None),
        ("rar4-md64k.rar", "rar4", distances(256 * 1024, 48 * 1024), ["-ma4", "-m3", "-md64k"], [RAR4, "dictionary 64 KiB"], None),
        ("rar4-comment.rar", "rar4", mixed, ["-ma4", "-m3", f"-z{note}"], [RAR4, "archive comment"], None),
        ("rar4-recovery.rar", "rar4", mixed, ["-ma4", "-m3", "-rr1%"], [RAR4, "recovery record"], None),
        ("rar4-solid.rar", "rar4", mixed, ["-ma4", "-m3", "-s"], [RAR4, "solid"], REFUSED_SOLID_4),
        ("rar4-encrypted-headers.rar", "rar4", single, ["-ma4", "-m3", f"-hp{PASSWORD}"], [RAR4, "encrypted headers"], REFUSED_ENCRYPTED_HEADERS_4),
        ("rar4-encrypted-data.rar", "rar4", single, ["-ma4", "-m3", f"-p{PASSWORD}"], [RAR4, "encrypted data"], REFUSED_ENCRYPTED_ENTRY_4),
        ("rar4-volumes.part1.rar", "rar4", single, ["-ma4", "-m0", "-v24k"], [RAR4, "volumes"], REFUSED_VOLUME_4),
    ]


def as_7zip_names_a_version(name):
    """unrar -ver names an old version `name;N`, and 7-Zip and Zipper name it
    `[VER]/N/name`."""
    base, sep, number = name.rpartition(";")
    if sep and number.isdigit():
        return f"[VER]/{number}/{base}"
    return name


def tree(root, rar4):
    found = {}
    for base, folders, files in os.walk(root):
        for name in folders + files:
            path = Path(base) / name
            key = unicodedata.normalize("NFC", as_7zip_names_a_version(path.relative_to(root).as_posix()))
            if key.startswith("[VER]/"):
                parts = key.split("/")
                for depth in range(1, len(parts)):
                    found["/".join(parts[:depth])] = {"dir": True}
            info = os.lstat(path)
            if stat.S_ISLNK(info.st_mode):
                found[key] = {"link": os.readlink(path)}
            elif stat.S_ISDIR(info.st_mode):
                found[key] = {"dir": True}
            else:
                node = {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                        "mode": oct(info.st_mode & 0o777)}
                if not rar4:
                    node["mtime_ns"] = info.st_mtime_ns
                found[key] = node
    return found


def as_rar4_spells_it(name):
    """RAR 4 keeps a Unicode name in 16-bit units, and keeps only the low
    16 bits of a character outside the Basic Multilingual Plane."""
    return "".join(chr(ord(c) & 0xFFFF) for c in name)


def unlock(root):
    for base, folders, _ in os.walk(root):
        for folder in folders:
            path = Path(base) / folder
            if not path.is_symlink():
                path.chmod(0o755)


def version(binary):
    banner = subprocess.run([binary], capture_output=True, text=True).stdout
    return next(line.split("Copyright")[0].strip() for line in banner.splitlines() if line.strip())


def volumes(archive):
    stem = archive.name.split(".part1.rar")[0]
    return sorted(archive.parent.glob(f"{stem}.part*.rar"))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rar", default="rar")
    parser.add_argument("--rar4", required=True, help="a RAR 6 rar, the last that writes RAR 4")
    parser.add_argument("--unrar", default="unrar")
    parser.add_argument("--output", type=Path, default=OUTPUT)
    args = parser.parse_args()
    writers = {"rar": args.rar, "rar4": args.rar4}
    tools = {"rar": version(args.rar), "rar4": version(args.rar4), "unrar": version(args.unrar)}
    if args.output.exists():
        shutil.rmtree(args.output)
    args.output.mkdir(parents=True)
    manifest = {"tools": tools, "archives": {}}
    with tempfile.TemporaryDirectory() as scratch:
        work = Path(scratch)
        for archive, writer, build, switches, features, refusal in cases(work):
            case = work / archive
            source = case / "source"
            rewrite = build(source / "src")
            target = args.output / archive
            written = target.with_name(archive.replace(".part1.rar", ".rar"))
            command = [writers[writer], "a", "-idq", "-r", "-ol", *switches, str(written), "src"]
            subprocess.run(command, cwd=source, check=True)
            rar4 = RAR4 in features
            expected = tree(source, rar4)
            if rewrite is not None:
                old = {f"[VER]/1/{path}": node for path, node in expected.items() if "sha256" in node}
                rewrite()
                subprocess.run(command, cwd=source, check=True)
                expected = tree(source, rar4)
                for path, node in old.items():
                    parts = path.split("/")
                    for depth in range(1, len(parts)):
                        expected["/".join(parts[:depth])] = {"dir": True}
                    expected[path] = node
            out = case / "unrar"
            out.mkdir()
            subprocess.run([args.unrar, "x", "-idq", "-y", "-ver", f"-p{PASSWORD}", str(target), f"{out}/"], check=True)
            if rar4:
                expected = {as_rar4_spells_it(path): node for path, node in expected.items()}
            extracted = tree(out, rar4)
            if extracted != expected:
                unlock(source)
                unlock(out)
                for path in sorted(set(expected) | set(extracted)):
                    if expected.get(path) != extracted.get(path):
                        print(f"  {path}: source {expected.get(path)}, unrar {extracted.get(path)}", file=sys.stderr)
                sys.exit(f"{archive}: unrar extracted a tree that differs from the source")
            entry = {
                "writer": tools[writer],
                "command": ["rar", *command[1:-2], "<archive>", "src"],
                "features": features,
                "files": [path.name for path in volumes(target)] if ".part1." in archive else [archive],
            }
            if refusal is None:
                entry["tree"] = expected
            else:
                entry["refused"] = refusal
            manifest["archives"][archive] = entry
            unlock(source)
            unlock(out)
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")
    total = sum(path.stat().st_size for path in args.output.iterdir())
    print(f"{len(manifest['archives'])} archives, {total} bytes", file=sys.stderr)


if __name__ == "__main__":
    main()
