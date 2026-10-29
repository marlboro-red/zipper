"""Byte-check the Windows release executable and create the portable package."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile


binary = Path(sys.argv[1]).resolve()
version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]


def run(*args):
    subprocess.run([str(binary), *map(str, args)], check=True)


with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    source = root / "input"
    source.mkdir()
    (source / "empty").mkdir()
    (source / "hello.txt").write_bytes(b"Zipper Windows round trip\r\n" * 10000)
    (source / "binary.dat").write_bytes(bytes(range(256)) * 8192)
    (source / "unicode-caf\u00e9.txt").write_bytes(b"UTF-8 filename")
    (source / "zero").write_bytes(b"")

    def verify(destination):
        expected = {p.relative_to(source) for p in source.rglob("*")}
        actual = {p.relative_to(destination) for p in destination.rglob("*")}
        assert actual == expected, (actual, expected)
        for relative in expected:
            original = source / relative
            restored = destination / relative
            assert original.is_dir() == restored.is_dir()
            if original.is_file():
                assert original.read_bytes() == restored.read_bytes(), relative

    run("--version")
    archive = root / "test.zipper"
    run("--threads", "4", "create", archive, source)
    run("test", archive)
    run("extract", archive, "--output", root / "native")
    verify(root / "native" / "input")
    archive = root / "test.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
        for path in source.rglob("*"):
            output.write(path, path.relative_to(source).as_posix())
    for engine in ("native", "native-fast"):
        run("--zip-engine", engine, "test", archive)
        destination = root / engine / "zip"
        destination.parent.mkdir(exist_ok=True)
        run("--zip-engine", engine, "--threads", "4", "extract", archive,
            "--output", destination)
        verify(destination)

dist = Path("dist")
dist.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory() as tmp:
    package = Path(tmp)
    shutil.copy2(binary, package / "zipper.exe")
    shutil.copy2("docs/windows.md", package / "README.md")
    shutil.copy2("Cargo.lock", package / "Cargo.lock")
    (package / "BUILD.json").write_text(json.dumps({
        "version": version,
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "target": "x86_64-pc-windows-msvc",
        "rust": "1.96.0",
        "static_crt": True,
        "verification": "release tests and direct byte comparison of native/ZIP CLI round trips",
    }, indent=2) + "\n")
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry" / "src"
    lock = tomllib.loads(Path("Cargo.lock").read_text())
    for dependency in lock["package"]:
        if not dependency.get("source", "").startswith("registry+"):
            continue
        name = f'{dependency["name"]}-{dependency["version"]}'
        for crate in registry.glob(f"*/{name}"):
            for path in crate.rglob("*"):
                if path.is_file() and path.name.lower().startswith(("license", "copying", "notice", "copyright")):
                    target = package / "third-party-notices" / name / path.relative_to(crate)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, target)
    archive = dist / f"zipper-{version}-windows-x86_64.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
        for path in sorted(package.rglob("*")):
            if path.is_file():
                output.write(path, path.relative_to(package).as_posix())
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (dist / "SHA256SUMS.txt").write_text(f"{digest}  {archive.name}\n")
    print(f"Packaged {archive}: {digest}")
