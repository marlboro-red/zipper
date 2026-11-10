#!/usr/bin/env python3
"""Cases for scripts/merge_coverage.py; the `hooks` gate step runs them."""
from pathlib import Path
import os
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from merge_coverage import merge  # noqa: E402

PRE = "# preamble\n\n"
SHARED = "item = src/lib.rs | pub(crate) fn field_check(\nitem = src/lib.rs | pub(crate) type Field<\n"


def section(name, fingerprint, *items):
    return f"[{name}]\nfingerprint = {fingerprint}\n" + "".join(f"item = {i}\n" for i in items) + SHARED


def document(*parts):
    return PRE + "\n".join(parts)


CLEAN_ENV = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}


def run(git, *args, check=True):
    return subprocess.run(["git", *args], cwd=git, capture_output=True, text=True, check=check, env=CLEAN_ENV)


def cases():
    failures = []
    a = section("a", "1", "src/a.rs | fn a(")
    apfs = section("apfs", "2", "src/apfs.rs | fn x(", "src/apfs.rs | mod proofs {")
    swf = section("swf", "3", "src/swf.rs | fn y(")
    base = document(a)

    merged, conflicts = merge(base, document(a, apfs), document(a, swf))
    if conflicts or merged != document(a, apfs, swf):
        failures.append(f"two new sections ending alike did not both survive whole:\n{merged}")

    resynced = section("a", "9", "src/a.rs | fn a(")
    merged, conflicts = merge(base, document(resynced), document(a, swf))
    if conflicts or merged != document(resynced, swf):
        failures.append(f"a fingerprint change on one side and a new section on the other:\n{merged}")

    merged, conflicts = merge(base, document(resynced), document(section("a", "8", "src/a.rs | fn a(")))
    if conflicts != ["a"] or "<<<<<<< ours [a]" not in merged:
        failures.append(f"both sides re-synced [a] differently and no conflict was reported: {conflicts}")

    merged, conflicts = merge(document(a, swf), document(a), document(a, swf))
    if conflicts or merged != document(a):
        failures.append(f"a section one side removed came back:\n{merged}")

    real = (HERE.parent / "verification/coverage.txt").read_text(encoding="utf-8")
    merged, conflicts = merge(real, real, real)
    if conflicts or merged != real:
        failures.append("merging the real coverage.txt with itself changed it")

    with tempfile.TemporaryDirectory() as folder:
        git = Path(folder)
        run(git, "init", "-q", "-b", "main")
        run(git, "config", "user.email", "test@example.invalid")
        run(git, "config", "user.name", "test")
        run(git, "config", "merge.coverage.driver",
            f"{sys.executable} {HERE / 'merge_coverage.py'} %O %A %B")
        (git / ".gitattributes").write_text("coverage.txt merge=coverage\n")
        (git / "coverage.txt").write_text(base)
        run(git, "add", "-A")
        run(git, "commit", "-q", "-m", "base")
        run(git, "switch", "-q", "-c", "swf")
        (git / "coverage.txt").write_text(document(a, swf))
        run(git, "commit", "-q", "-am", "swf")
        run(git, "switch", "-q", "main")
        (git / "coverage.txt").write_text(document(a, apfs))
        run(git, "commit", "-q", "-am", "apfs")
        run(git, "switch", "-q", "swf")
        rebase = run(git, "rebase", "main", check=False)
        landed = (git / "coverage.txt").read_text()
        if rebase.returncode != 0 or landed != document(a, apfs, swf):
            failures.append(f"a real rebase through the driver: {rebase.stderr.strip()}\n{landed}")
    return failures


def main():
    failures = cases()
    for failure in failures:
        print(f"FAILED: {failure}")
    if failures:
        sys.exit(1)
    print("merge_coverage: every case passed")


if __name__ == "__main__":
    main()
