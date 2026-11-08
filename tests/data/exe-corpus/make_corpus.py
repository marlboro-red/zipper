#!/usr/bin/env python3
"""Build the executable corpus in this folder and record the tree 7-Zip extracts.

    python3 tests/data/exe-corpus/make_corpus.py

Needs LLVM 21 with lld (Homebrew `llvm@21` and `lld@21`, or set LLVM and LLD to
their bin folders), Apple clang and lipo for Mach-O, and 7zz on PATH. Every
input is written here from fixed text, so the files hold no third-party code.
"""
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
LLVM = Path(os.environ.get("LLVM", "/opt/homebrew/opt/llvm@21/bin"))
LLD = Path(os.environ.get("LLD", "/opt/homebrew/opt/lld@21/bin"))
SOURCE = """\
int counter = 7;
int zeros[64];
const char message[] = "zipper executable corpus";
int helper(int x) { return x * 3 + counter; }
"""
ELF_START = SOURCE + "void _start(void) { zeros[1] = helper(message[0]); for (;;) {} }\n"
PE_ENTRY = SOURCE + "int entry(void) { return helper(message[1]); }\n"
MACHO_MAIN = SOURCE + "int main(void) { return helper(message[2]); }\n"
RESOURCES = """\
1 ICON "icon.ico"
2 BITMAP "bitmap.bmp"
LOGO RCDATA { "corpus data\\0" }
300 RCDATA { 1, 2, 3 }
STRINGTABLE
BEGIN
  1 "first string"
  2 "second string"
  17 "string seventeen"
END
1 VERSIONINFO
FILEVERSION 1,2,3,4
PRODUCTVERSION 5,6,7,8
FILEFLAGSMASK 0x3f
FILEFLAGS 0
FILEOS 0x40004
FILETYPE 1
FILESUBTYPE 0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Zipper Test"
      VALUE "FileDescription", "Corpus binary"
      VALUE "FileVersion", "1.2.3.4"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
1 MENU
BEGIN
  POPUP "&File"
  BEGIN
    MENUITEM "E&xit", 100
  END
END
"""


def run(*args, cwd):
    subprocess.run([str(arg) for arg in args], cwd=cwd, check=True)


def icon():
    width = 16
    header = struct.pack("<IiiHHIIiiII", 40, width, 2 * width, 1, 32, 0, 0, 0, 0, 0, 0)
    image = header + bytes([0x10, 0x20, 0x30, 0xFF]) * width * width + bytes(4 * width)
    entry = struct.pack("<BBBBHHII", width, width, 0, 0, 1, 32, len(image), 22)
    return struct.pack("<HHH", 0, 1, 1) + entry + image


def bitmap():
    rows = (bytes([0, 0, 255, 0, 255, 0]) + b"\0\0") * 2
    info = struct.pack("<IiiHHIIiiII", 40, 2, 2, 1, 24, 0, len(rows), 2835, 2835, 0, 0)
    return b"BM" + struct.pack("<IHHI", 14 + 40 + len(rows), 0, 0, 54) + info + rows


def pe_to_te(pe):
    """Rewrite a PE image as a UEFI TE image, as EDK2's GenFw does."""
    pointer = struct.unpack_from("<I", pe, 0x3C)[0]
    machine, sections, _, _, _, optional_size, _ = struct.unpack_from("<HHIIIHH", pe, pointer + 4)
    optional = pointer + 24
    subsystem = struct.unpack_from("<H", pe, optional + 68)[0]
    entry, base_of_code = struct.unpack_from("<II", pe, optional + 16)
    image_base = struct.unpack_from("<Q", pe, optional + 24)[0]
    table = optional + optional_size
    stripped = table
    header = b"VZ" + struct.pack("<HBBH", machine, sections, subsystem, stripped)
    header += struct.pack("<IIQ", entry, base_of_code, image_base) + bytes(16)
    return header + pe[table:]


def tree(root):
    found = {}
    for path in sorted(root.rglob("*")):
        name = path.relative_to(root).as_posix()
        found[name] = "dir" if path.is_dir() else hashlib.sha256(path.read_bytes()).hexdigest()
    return found


def main():
    work = Path(tempfile.mkdtemp())
    (work / "elf.c").write_text(ELF_START)
    (work / "pe.c").write_text(PE_ENTRY)
    (work / "macho.c").write_text(MACHO_MAIN)
    (work / "resources.rc").write_text(RESOURCES)
    (work / "icon.ico").write_bytes(icon())
    (work / "bitmap.bmp").write_bytes(bitmap())
    clang = LLVM / "clang"
    built = {}
    for name, target, flags in [
        ("x86_64.elf", "x86_64-linux-gnu", []),
        ("i386.elf", "i386-linux-gnu", []),
        ("aarch64.elf", "aarch64-linux-gnu", []),
        ("ppc-be.elf", "powerpc-linux-gnu", []),
        ("s390x-be.elf", "s390x-linux-gnu", []),
        ("riscv64.so", "riscv64-linux-gnu", ["-shared", "-fPIC"]),
    ]:
        run(clang, f"--target={target}", "-O1", "-fno-pic" if "-fPIC" not in flags else "-fPIC",
            "-c", "elf.c", "-o", "elf.o", cwd=work)
        run(LLD / "ld.lld", *[f for f in flags if f == "-shared"], "elf.o", "-o", name, cwd=work)
        built[name] = work / name
    run(clang, "--target=x86_64-linux-gnu", "-O1", "-c", "elf.c", "-o", "x86_64.o", cwd=work)
    built["x86_64.o"] = work / "x86_64.o"
    run(LLVM / "llvm-objcopy", "--strip-sections", "x86_64.elf", "segments-only.elf", cwd=work)
    built["segments-only.elf"] = work / "segments-only.elf"
    run(LLVM / "llvm-rc", "-no-preprocess", "-fo", "resources.res", "resources.rc", cwd=work)
    for name, target, machine in [
        ("x64.exe", "x86_64-pc-windows-msvc", "x64"),
        ("x86.exe", "i686-pc-windows-msvc", "x86"),
        ("arm64.exe", "aarch64-pc-windows-msvc", "arm64"),
    ]:
        obj = name.replace(".exe", ".obj")
        run(clang, f"--target={target}", "-O1", "-c", "pe.c", "-o", obj, cwd=work)
        run(LLD / "lld-link", "/nodefaultlib", "/entry:entry", "/subsystem:console", f"/machine:{machine}",
            "/timestamp:1600000000", obj, "resources.res", f"/out:{name}", cwd=work)
        built[name] = work / name
        built[obj] = work / obj
    run(LLD / "lld-link", "/nodefaultlib", "/dll", "/noentry", "/machine:x64", "/timestamp:1600000000",
        "x64.obj", f"/out:library.dll", cwd=work)
    built["library.dll"] = work / "library.dll"
    pe = (work / "x64.exe").read_bytes()
    (work / "x64.te").write_bytes(pe_to_te(pe))
    built["x64.te"] = work / "x64.te"
    for name, arch in [("arm64.macho", "arm64"), ("x86_64.macho", "x86_64")]:
        run("clang", "-arch", arch, "-O1", "macho.c", "-o", name, cwd=work)
        built[name] = work / name
    run("clang", "-arch", "arm64", "-O1", "-c", "macho.c", "-o", "arm64-object.macho", cwd=work)
    built["arm64-object.macho"] = work / "arm64-object.macho"
    run("lipo", "-create", "arm64.macho", "x86_64.macho", "-output", "universal.mub", cwd=work)
    built["universal.mub"] = work / "universal.mub"
    for old in HERE.iterdir():
        if old.name not in ("make_corpus.py", "NOTICE.txt"):
            old.unlink()
    trees = {}
    for name, path in sorted(built.items()):
        shutil.copy(path, HERE / name)
        out = work / ("x-" + name)
        result = subprocess.run(["7zz", "x", "-y", f"-o{out}", str(HERE / name)], capture_output=True)
        if result.returncode != 0:
            sys.exit(f"7-Zip refuses {name}")
        trees[name] = tree(out)
    (HERE / "trees.json").write_text(json.dumps(trees, indent=2, sort_keys=True) + "\n")
    shutil.rmtree(work)


if __name__ == "__main__":
    main()
