#!/usr/bin/env python3
"""Zipper's coverage of everything 7-Zip extracts.

    python3 scripts/parity.py capture   # record `7zz i` from the 7-Zip on PATH
    python3 scripts/parity.py corpus    # build one small archive per row this machine can make
    python3 scripts/parity.py corpus NAME...   # rebuild only the named archives, keeping the rest
    python3 scripts/parity.py render    # write docs/reference/parity.md
    python3 scripts/parity.py check     # the gate step

The rows are every format and codec `7zz i` lists, the ZIP methods 7-Zip's
ZIP handler decodes, and the `7z x` switches that change what extraction
writes. tests/data/parity/status.json classifies each row. `check` fails when
a row is unclassified, a classification has no proof, or the page differs
from what `render` writes. tests/parity.rs extracts every corpus archive."""
import hashlib
import json
import lzma
import re
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "tests/data/parity"
CAPTURE = DATA / "7zip-i.txt"
STATUS = DATA / "status.json"
ARCHIVES = DATA / "archives.json"
CORPUS = DATA / "archives"
PAGE = ROOT / "docs/reference/parity.md"
PASSWORD = "zipper"
COMPRESS_ABOVE = 256 * 1024
STORED_SUFFIX = ".stored.xz"
STATUSES = {
    "native": "Zipper reads it in process.",
    "partial": "Zipper reads some of it in process, as the note says.",
    "backend": "Zipper hands it to 7-Zip.",
    "missing": "Zipper cannot do it, even through 7-Zip.",
    "dropped": "Zipper leaves it out on purpose, as the note says.",
    "refused": "Zipper refuses it in every engine for licence reasons, as the note says, and never hands it to 7-Zip.",
}
ZIP_METHODS = ["Store", "Shrink", "Implode", "Deflate", "Deflate64", "BZip2", "LZMA",
               "Zstandard", "XZ", "PPMd", "ZipCrypto", "WinZip AES"]
SWITCHES = ["x", "e", "-o", "-aoa", "-aos", "-aot", "-aou", "-i", "-x", "-p", "-si", "-so",
            "-snl", "-snh", "-sni", "-sns", "-snz", "-spf"]


def sevenzip():
    return os.environ.get("ZIPPER_7Z") or shutil.which("7zz") or shutil.which("7z")


def captured_rows():
    lines = CAPTURE.read_text().splitlines()
    formats = lines[lines.index("Formats:") + 1:lines.index("Codecs:")]
    codecs = lines[lines.index("Codecs:") + 1:]
    codecs = codecs[:codecs.index("Hashers:")] if "Hashers:" in codecs else codecs
    return {
        "formats": [line.split()[1] for line in formats if line.strip()],
        "codecs": [line.split()[-1] for line in codecs if line.strip() and line.strip() != "Codecs:"],
        "zip methods": ZIP_METHODS,
        "switches": SWITCHES,
    }


def capture():
    binary = sevenzip()
    if not binary:
        sys.exit("parity: no 7-Zip on PATH; set ZIPPER_7Z")
    output = subprocess.run([binary, "i"], capture_output=True, text=True, check=True).stdout
    version = next(line for line in output.splitlines() if line.startswith("7-Zip"))
    sections = output[output.index("Formats:"):output.index("Hashers:")]
    CAPTURE.write_text(version + "\n\n" + sections.rstrip() + "\n")
    print(f"parity: captured {version}")


def tree(root):
    entries = {}
    for path in sorted(root.rglob("*")):
        name = path.relative_to(root).as_posix()
        if path.is_symlink():
            entries[name] = "link:" + os.readlink(path)
        elif path.is_dir():
            entries[name] = "dir"
        else:
            entries[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return entries


def write_source(root):
    (root / "folder").mkdir(parents=True)
    (root / "hello.txt").write_text("hello from the parity corpus\n")
    (root / "folder/empty").write_bytes(b"")
    lines = "".join(f"line {index} of a compressible file\n" for index in range(300))
    (root / "folder/data.txt").write_text(lines)
    if shutil.which("xattr"):
        subprocess.run(["xattr", "-cr", str(root)], check=True)


def write_ar(src, out):
    """A GNU ar archive, as Debian packages use, with one short member name."""
    data = (src / "hello.txt").read_bytes()
    header = f"{'hello.txt/':<16}{0:<12}{0:<6}{0:<6}{0o100644:<8o}{len(data):<10}`\n".encode()
    out.write_bytes(b"!<arch>\n" + header + data + (b"\n" if len(data) % 2 else b""))


RPM_SPEC = """Name: parity
Version: 1.0
Release: 1
Summary: Zipper parity package
License: MIT
BuildArch: noarch
%description
The parity corpus tree, with a symlink and a hard link.
%install
cp -R SOURCE/. %{buildroot}/
ln -s data.txt %{buildroot}/folder/link
ln %{buildroot}/folder/data.txt %{buildroot}/folder/hard
%files
/hello.txt
/folder
"""
RPM_PAYLOADS = {"gzip": "w9.gzdio", "xz": "w6.xzdio", "zstd": "w19.zstdio", "bzip2": "w9.bzdio", "lzma": "w6.lzdio"}


def write_rpm(compressor):
    """An RPM package in the version 4 format, the newest one 7-Zip unpacks, with one payload compressor."""
    def build(src, out):
        top = out.with_name(out.name + ".top")
        top.mkdir()
        spec = top / "parity.spec"
        spec.write_text(RPM_SPEC.replace("SOURCE", str(src)))
        run(["rpmbuild", "--define", f"_topdir {top}", "--define", "_rpmformat 4",
             "--define", f"_binary_payload {RPM_PAYLOADS[compressor]}", "--define", "_buildhost zipper",
             "-bb", str(spec)], src)
        [built] = (top / "RPMS").rglob("*.rpm")
        built.rename(out)
        shutil.rmtree(top)
    return build


CAB_SET = "cab-set-1.cab"


def write_cab_set(src, out):
    """A set of three cabinets whose one MSZIP folder continues from each into the next, written
    by Zipper's own test writer, since no CAB tool on macOS or Linux writes a cabinet set."""
    environment = dict(os.environ, ZIPPER_CAB_SET_SOURCE=str(src), ZIPPER_CAB_SET_OUT=str(out.parent))
    subprocess.run(["cargo", "test", "--release", "--test", "cab_sets", "write_the_parity_set",
                    "--", "--ignored", "--exact"], cwd=ROOT, env=environment, check=True, capture_output=True)


def run(command, cwd):
    subprocess.run(command, cwd=cwd, check=True, capture_output=True)


def builders(binary):
    """Each builder makes one archive from the source tree, or returns a reason it cannot."""
    seven = lambda kind, *switches: lambda src, out: run(
        [binary, "a", f"-t{kind}", *switches, str(out), "hello.txt", "folder"], src)
    single = lambda tool, *flags: lambda src, out: out.write_bytes(
        subprocess.run([tool, *flags, "-c", "hello.txt"], cwd=src, check=True, capture_output=True).stdout)
    def hdiutil(*flags):
        def build(src, out):
            run(["hdiutil", "create", "-quiet", "-srcfolder", str(src), "-volname", "parity", *flags, str(out)], src)
            written = out.with_name(out.name + ".cdr")
            if written.exists():
                written.rename(out)
        return build
    found = lambda tool: shutil.which(tool) is not None
    table = []
    for codec, switches in [("Copy", ["-m0=Copy"]), ("Deflate", ["-m0=Deflate"]), ("Deflate64", ["-m0=Deflate64"]),
                            ("LZMA", ["-m0=LZMA"]), ("LZMA2", ["-m0=LZMA2"]), ("PPMD", ["-m0=PPMd"]),
                            ("BZip2", ["-m0=BZip2"]), ("BCJ", ["-mf=BCJ"]), ("BCJ2", ["-mf=BCJ2"]),
                            ("PPC", ["-mf=PPC"]), ("IA64", ["-mf=IA64"]), ("ARM", ["-mf=ARM"]), ("ARMT", ["-mf=ARMT"]),
                            ("SPARC", ["-mf=SPARC"]), ("ARM64", ["-mf=ARM64"]), ("RISCV", ["-mf=RISCV"]),
                            ("Delta", ["-mf=Delta:4"]), ("Swap2", ["-mf=Swap2"]), ("Swap4", ["-mf=Swap4"]),
                            ("7zAES", [f"-p{PASSWORD}", "-mhe=on"])]:
        table.append((f"7z-{codec.lower()}.7z", ("codecs", codec), True, seven("7z", *switches)))
    table.append(("7z.7z", ("formats", "7z"), True, seven("7z")))
    for method, switches in [("Store", ["-mm=Copy"]), ("Deflate", ["-mm=Deflate"]), ("Deflate64", ["-mm=Deflate64"]),
                             ("BZip2", ["-mm=BZip2"]), ("LZMA", ["-mm=LZMA"]), ("PPMd", ["-mm=PPMd"]),
                             ("XZ", ["-mm=XZ"]), ("Zstandard", ["-mm=ZSTD"]),
                             ("ZipCrypto", [f"-p{PASSWORD}", "-mem=ZipCrypto"]),
                             ("WinZip AES", [f"-p{PASSWORD}", "-mem=AES256"])]:
        name = "zip-" + method.lower().replace(" ", "-") + ".zip"
        table.append((name, ("zip methods", method), True, seven("zip", *switches)))
    table.append(("zip.zip", ("formats", "zip"), True, seven("zip")))
    table.append(("tar.tar", ("formats", "tar"), True, seven("tar")))
    table.append(("wim.wim", ("formats", "wim"), True, seven("wim")))
    for name, tool, flags, row in [("hello.txt.gz", "gzip", ["-n"], "gzip"), ("hello.txt.bz2", "bzip2", [], "bzip2"),
                                   ("hello.txt.xz", "xz", [], "xz"), ("hello.txt.zst", "zstd", ["-q"], "zstd"),
                                   ("hello.txt.lzma", "xz", ["--format=lzma"], "lzma"), ("hello.txt.Z", "compress", [], "Z")]:
        table.append((name, ("formats", row), found(tool), single(tool, *flags)))
    for name, tool, row in [("tar.tar.gz", "gzip", "gzip"), ("tar.tar.xz", "xz", "xz"), ("tar.tar.zst", "zstd", "zstd"),
                            ("tar.tar.bz2", "bzip2", "bzip2"), ("tar.tar.Z", "compress", "Z")]:
        def tarred(src, out, tool=tool):
            run([binary, "a", "-ttar", str(out.with_suffix("")), "hello.txt", "folder"], src)
            plain = out.with_suffix("")
            out.write_bytes(subprocess.run([tool, "-c", str(plain)], check=True, capture_output=True).stdout)
            plain.unlink()
        table.append((name, ("formats", row), found(tool), tarred))
    for name, form in [("cpio-newc.cpio", "newc"), ("cpio-odc.cpio", "odc"), ("cpio-bin.cpio", "bin")]:
        table.append((name, ("formats", "Cpio"), found("cpio"), lambda src, out, form=form: out.write_bytes(
            subprocess.run(["cpio", "-o", "-H", form], cwd=src, input=b"hello.txt\nfolder\nfolder/data.txt\nfolder/empty\n",
                           check=True, capture_output=True).stdout)))
    table.append(("ar.a", ("formats", "Ar"), True, write_ar))
    for compressor in RPM_PAYLOADS:
        table.append((f"rpm-{compressor}.rpm", ("formats", "Rpm"), found("rpmbuild"), write_rpm(compressor)))
    table.append(("xar.xar", ("formats", "Xar"), found("xar"), lambda src, out: run(["xar", "-cf", str(out), "hello.txt", "folder"], src)))
    table.append(("cab.cab", ("formats", "Cab"), found("gcab"), lambda src, out: run(
        ["gcab", "-c", "-z", str(out), "hello.txt", "folder/data.txt", "folder/empty"], src)))
    table.append((CAB_SET, ("formats", "Cab"), found("cargo"), write_cab_set))
    table.append(("split.7z.001", ("formats", "Split"), True, lambda src, out: run(
        [binary, "a", "-t7z", "-mx0", "-v4k", str(out.with_suffix("")), "hello.txt", "folder"], src)))
    if found("hdiutil"):
        table += [
            ("iso.iso", ("formats", "Iso"), True, lambda src, out: run(
                ["hdiutil", "makehybrid", "-quiet", "-iso", "-joliet", "-o", str(out), str(src)], src)),
            ("udf.iso", ("formats", "Udf"), True, lambda src, out: run(
                ["hdiutil", "makehybrid", "-quiet", "-udf", "-o", str(out), str(src)], src)),
            ("dmg-zlib.dmg", ("formats", "Dmg"), True, hdiutil("-format", "UDZO")),
            ("dmg-bzip2.dmg", ("formats", "Dmg"), True, hdiutil("-format", "UDBZ")),
            ("dmg-lzfse.dmg", ("formats", "Dmg"), True, hdiutil("-format", "ULFO")),
            ("dmg-lzma.dmg", ("formats", "Dmg"), True, hdiutil("-format", "ULMO")),
            ("hfs.img", ("formats", "HFS"), True, hdiutil("-fs", "HFS+", "-layout", "NONE", "-format", "UDTO")),
            ("apfs.img", ("formats", "APFS"), True, hdiutil("-fs", "APFS", "-layout", "NONE", "-format", "UDTO")),
            ("fat.img", ("formats", "FAT"), True, hdiutil("-fs", "MS-DOS", "-layout", "NONE", "-format", "UDTO")),
            ("gpt.img", ("formats", "GPT"), True, hdiutil("-fs", "MS-DOS", "-layout", "GPTSPUD", "-format", "UDTO")),
            ("mbr.img", ("formats", "MBR"), True, hdiutil("-fs", "MS-DOS", "-layout", "MBRSPUD", "-format", "UDTO")),
            ("apm.img", ("formats", "APM"), True, hdiutil("-fs", "HFS+", "-layout", "SPUD", "-format", "UDTO")),
        ]
    return table


def extract_with_7zip(binary, archive, into):
    """7-Zip's tree, with a lone inner tar extracted too, as Zipper extracts a compressed tar,
    and the lone cpio 7-Zip writes from an RPM, as Zipper extracts the files a package installs."""
    command = [binary, "x", "-y", f"-p{PASSWORD}", f"-o{into}", str(archive)]
    subprocess.run(command, check=True, capture_output=True, stdin=subprocess.DEVNULL)
    inner = list(into.iterdir())
    nested = {".tar": archive.suffix != ".tar", ".cpio": archive.suffix == ".rpm"}
    if len(inner) == 1 and nested.get(inner[0].suffix):
        return extract_with_7zip(binary, inner[0], into.with_name(into.name + "-inner"))
    return tree(into)


def corpus(only):
    binary = sevenzip()
    if not binary:
        sys.exit("parity: no 7-Zip on PATH; set ZIPPER_7Z")
    records = {}
    if only:
        records = json.loads(ARCHIVES.read_text())["archives"]
        for name in only:
            for file in records.pop(name, {}).get("files", []):
                (CORPUS / file).unlink()
    elif CORPUS.exists():
        shutil.rmtree(CORPUS)
    CORPUS.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as work:
        work = Path(work)
        source = work / "source"
        write_source(source)
        for name, row, available, build in builders(binary):
            if only and name not in only:
                continue
            if not available:
                print(f"parity: skipped {name}, its tool is not installed")
                continue
            made = work / "made" / name
            made.parent.mkdir(parents=True, exist_ok=True)
            try:
                build(source, made)
            except subprocess.CalledProcessError as error:
                print(f"parity: could not build {name}: {(error.stderr or b'').decode(errors='replace').strip()[:200]}")
                continue
            if name.endswith(".001"):
                outputs = sorted(made.parent.glob(made.name[:-4] + ".*"))
            elif name == CAB_SET:
                outputs = sorted(made.parent.glob(CAB_SET.replace("-1.", "-*.")))
            else:
                outputs = [made]
            try:
                reference = extract_with_7zip(binary, outputs[0], work / "reference" / name)
            except subprocess.CalledProcessError as error:
                print(f"parity: 7-Zip cannot extract {name}: {error.stdout.decode(errors='replace').strip()[-200:]}")
                continue
            stored = []
            for output in outputs:
                data = output.read_bytes()
                if len(data) > COMPRESS_ABOVE:
                    (CORPUS / (output.name + STORED_SUFFIX)).write_bytes(lzma.compress(data, preset=9))
                    stored.append(output.name + STORED_SUFFIX)
                else:
                    (CORPUS / output.name).write_bytes(data)
                    stored.append(output.name)
            hashes = {file: hashlib.sha256((CORPUS / file).read_bytes()).hexdigest() for file in stored}
            records[name] = {"row": list(row), "files": stored, "sha256": hashes, "tree": reference}
            print(f"parity: built {name}, {sum((CORPUS / file).stat().st_size for file in stored)} bytes stored")
    version = subprocess.run([binary, "i"], capture_output=True, text=True).stdout.splitlines()
    version = next(line for line in version if line.startswith("7-Zip"))
    ARCHIVES.write_text(json.dumps({"reference": version, "password": PASSWORD, "archives": records},
                                   indent=2, ensure_ascii=False, sort_keys=True) + "\n")


def proof_problems(status, archives):
    problems = []
    rows = captured_rows()
    for group, names in rows.items():
        given = status.get(group, {})
        for name in names:
            if name not in given:
                problems.append(f"{group} row {name!r} has no status")
        for name in given:
            if name not in names:
                problems.append(f"{group} status {name!r} names no row")
        for name, entry in given.items():
            if entry.get("status") not in STATUSES:
                problems.append(f"{group} {name!r} has unknown status {entry.get('status')!r}")
            if entry.get("status") in ("native", "partial") and not (
                    entry.get("tests") or any(record["row"] == [group, name] for record in archives["archives"].values())):
                problems.append(f"{group} {name!r} is {entry['status']} without a proof")
            for key in ("native archives", "refused archives"):
                for archive in entry.get(key, []):
                    if archives["archives"].get(archive, {}).get("row") != [group, name]:
                        problems.append(f"{group} {name!r} names {archive}, which is not one of its archives")
            if entry.get("status") in ("partial", "dropped", "refused") and not entry.get("note"):
                problems.append(f"{group} {name!r} is {entry['status']} without a note")
            for test in entry.get("tests", []):
                file, _, function = test.partition("::")
                path = ROOT / file
                if not path.is_file() or f"fn {function}(" not in path.read_text():
                    problems.append(f"{group} {name!r} names a missing test {test}")
    for name, record in archives["archives"].items():
        group, row = record["row"]
        if row not in status.get(group, {}):
            problems.append(f"archive {name} names no classified row")
        for file in record["files"]:
            if not (CORPUS / file).is_file():
                problems.append(f"archive {name} is missing {file}")
            elif hashlib.sha256((CORPUS / file).read_bytes()).hexdigest() != record["sha256"][file]:
                problems.append(f"archive {name} has changed bytes in {file}")
    return problems


def proofs(group, name, entry, archives):
    found = [f"`tests/data/parity/archives/{record['files'][0]}`"
             for record in archives["archives"].values() if record["row"] == [group, name]]
    found += [f"`{test.split('::')[1]}` in `{test.split('::')[0]}`" for test in entry.get("tests", [])]
    return ", ".join(found) if found else "No proof yet."


def page(status, archives):
    version = re.search(r"\d+\.\d+", CAPTURE.read_text().splitlines()[0]).group()
    reference = f"7-Zip {version}"
    counts = {key: 0 for key in STATUSES}
    unproven = 0
    for group_name, group in status.items():
        for name, entry in group.items():
            counts[entry["status"]] += 1
            unproven += proofs(group_name, name, entry, archives) == "No proof yet."
    lines = [
        "# 7-Zip parity",
        "",
        "<!-- Generated by scripts/parity.py render from tests/data/parity. Do not edit. -->",
        "",
        f"This page lists everything {reference} extracts and how Zipper handles it. "
        "The [roadmap](../roadmap/m10-parity-matrix.md) moves every row to native, or records why it is dropped.",
        "",
        "Each row has one status.",
        "",
    ]
    lines += [f"- `{key}` means {text}" for key, text in STATUSES.items()]
    lines += [
        "",
        "A proof is a corpus archive or a test. `tests/parity.rs` extracts every corpus archive. "
        "A native row must match the tree 7-Zip extracted when the corpus was built. "
        "A backend row must match a direct 7-Zip extraction on the same machine. "
        "A missing row must be refused with nothing published. "
        "A refused row, and a refused archive of another row, must be refused by every engine "
        "with an error that says Zipper does not read it, with nothing published.",
        "",
        "| Status | Rows |",
        "| --- | ---: |",
    ]
    lines += [f"| `{key}` | {counts[key]} |" for key in STATUSES]
    lines += ["", f"{unproven} rows have no proof yet. "
              "The milestone that moves a row adds a corpus archive for it, "
              "or a test when the row is a switch."]
    for group in ["formats", "codecs", "zip methods", "switches"]:
        heading = {"formats": "Formats", "codecs": "7z codecs", "zip methods": "ZIP methods",
                   "switches": "Extraction switches"}[group]
        lines += ["", f"## {heading}", "", "| Row | Status | Note | Proof |", "| --- | --- | --- | --- |"]
        for name in captured_rows()[group]:
            entry = status[group][name]
            note = entry.get("note", "")
            lines.append(f"| `{name}` | `{entry['status']}` | {note} | {proofs(group, name, entry, archives)} |")
    return "\n".join(lines) + "\n"


def load():
    return json.loads(STATUS.read_text()), json.loads(ARCHIVES.read_text())


os.environ["COPYFILE_DISABLE"] = "1"


def main():
    action = sys.argv[1] if len(sys.argv) > 1 else ""
    if action == "capture":
        capture()
    elif action == "corpus":
        corpus(sys.argv[2:])
    elif action == "render":
        PAGE.write_text(page(*load()))
        print(f"parity: wrote {PAGE.relative_to(ROOT)}")
    elif action == "check":
        status, archives = load()
        problems = proof_problems(status, archives)
        if not problems and PAGE.read_text() != page(status, archives):
            problems.append("docs/reference/parity.md differs; run python3 scripts/parity.py render")
        for problem in problems:
            print(f"parity: {problem}", file=sys.stderr)
        sys.exit(1 if problems else 0)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
