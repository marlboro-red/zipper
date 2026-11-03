#!/usr/bin/env python3
"""Cases for scripts/plant.py; the `hooks` gate step runs them."""
from pathlib import Path
import subprocess
import sys
import tempfile

PLANT = Path(__file__).resolve().parent / "plant.py"


def plant(file, old, new, caught_when, check, timeout=30):
    command = [sys.executable, str(PLANT), str(file), "--old", old, "--new", new,
               "--caught-when", caught_when, "--timeout", str(timeout), "--", sys.executable, "-c", check]
    return subprocess.run(command, capture_output=True, text=True)


def main():
    failures = []
    with tempfile.TemporaryDirectory() as work:
        target = Path(work) / "guarded.txt"
        target.write_text("limit = 3\n")
        reads = f"import sys; text = open({str(target)!r}).read(); "
        cases = [
            ("edit that does not apply", "missing", "x", "FAILED", "pass", 2),
            ("check that fails on the break", "limit = 3", "limit = 9",
             "FAILED", reads + "print('test result: FAILED' if '9' in text else 'ok'); sys.exit('9' in text)", 0),
            ("check that crashes for another reason", "limit = 3", "limit = 9",
             "test result: FAILED", "raise SystemExit('error: could not compile')", 1),
            ("check that times out", "limit = 3", "limit = 9", "FAILED", "import time; time.sleep(5)", 1),
            ("check that passes despite the break", "limit = 3", "limit = 9", "FAILED", "print('ok')", 1),
        ]
        for name, old, new, caught_when, check, expected in cases:
            result = plant(target, old, new, caught_when, check, timeout=2 if "sleep" in check else 30)
            if result.returncode != expected:
                failures.append(f"{name}: exit {result.returncode}, wanted {expected}\n{result.stdout}{result.stderr}")
            if target.read_text() != "limit = 3\n":
                failures.append(f"{name}: the file was not restored")
        for name, command, words in [
            ("check passed as one word with spaces", [f"{sys.executable} -c pass"], "one word with spaces"),
            ("check that cannot start", ["zipper-no-such-check"], "could not start"),
        ]:
            result = subprocess.run([sys.executable, str(PLANT), str(target), "--old", "limit = 3", "--new", "limit = 9",
                                     "--caught-when", "FAILED", "--", *command], capture_output=True, text=True)
            if result.returncode != 2 or words not in result.stderr:
                failures.append(f"{name}: exit {result.returncode}, wanted 2 and {words!r}\n{result.stdout}{result.stderr}")
            if target.read_text() != "limit = 3\n":
                failures.append(f"{name}: the file was not restored")
    for failure in failures:
        print(failure, file=sys.stderr)
    failed = {failure.split(":")[0] for failure in failures}
    total = len(cases) + 2
    print(f"plant.py: {total - len(failed)} of {total} cases pass")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
