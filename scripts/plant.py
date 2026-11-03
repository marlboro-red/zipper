#!/usr/bin/env python3
"""Prove a check can fail: plant one break, run the check, restore.

The edit replaces OLD with NEW in FILE and must apply exactly once. The run
counts as caught only when the check's output matches --caught-when, so a
compile error, a crash or a timeout is reported as missed rather than as a
catch. The file is always restored and compared byte for byte. When the
planted file is under src/, the release binary is rebuilt afterwards, so no
binary built with the break survives to be measured or shipped.

    python3 scripts/plant.py src/native_7z.rs --old 'a' --new 'b' \\
        --caught-when 'test result: FAILED' -- cargo test --release --test hostile_7z

Exit status 0 means caught, 1 missed, 2 a plant that did not apply or a
check that could not start. Pass the check as separate words; zsh does not
split an unquoted $var, so write ${=var}."""
import argparse
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("file", type=Path)
    parser.add_argument("--old", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--caught-when", required=True, help="regular expression the failing check prints")
    parser.add_argument("--timeout", type=float, default=1800)
    arguments = sys.argv[1:]
    if "--" not in arguments:
        parser.error("give the check to run after --")
    split = arguments.index("--")
    args = parser.parse_args(arguments[:split])
    command = arguments[split + 1:]
    if not command:
        parser.error("give the check to run after --")
    if len(command) == 1 and " " in command[0] and not shutil.which(command[0]):
        print(f"plant: NOT RUN, the check {command[0]!r} is one word with spaces; pass separate words",
              file=sys.stderr)
        sys.exit(2)
    if hasattr(signal, "SIGPIPE"):
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    path = args.file.resolve()
    original = path.read_bytes()
    text = original.decode()
    count = text.count(args.old)
    if count != 1:
        print(f"plant: NOT APPLIED, the old text occurs {count} times in {args.file}", file=sys.stderr)
        sys.exit(2)
    not_started = False
    try:
        path.write_text(text.replace(args.old, args.new))
        try:
            run = subprocess.run(command, cwd=Path.cwd(), capture_output=True, text=True, timeout=args.timeout)
            output = run.stdout + run.stderr
            status = f"exit {run.returncode}"
        except FileNotFoundError as error:
            output, status = "", f"could not start: {error}"
            not_started = True
        except subprocess.TimeoutExpired as expired:
            output = (expired.stdout or b"").decode(errors="replace") + (expired.stderr or b"").decode(errors="replace")
            status = f"timed out after {args.timeout:.0f} s"
    finally:
        path.write_bytes(original)
    if path.read_bytes() != original:
        print(f"plant: {args.file} was not restored", file=sys.stderr)
        sys.exit(2)
    if path.is_relative_to(ROOT / "src"):
        subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)
    if not_started:
        print(f"plant: NOT RUN, the check {status}", file=sys.stderr)
        sys.exit(2)
    matches = [line for line in output.splitlines() if re.search(args.caught_when, line)]
    if matches:
        print(f"plant: CAUGHT ({status})")
        for line in matches[:5]:
            print(f"  {line}")
        sys.exit(0)
    print(f"plant: MISSED ({status}); nothing matched {args.caught_when!r}. Last lines:")
    for line in output.splitlines()[-8:]:
        print(f"  {line}")
    sys.exit(1)


if __name__ == "__main__":
    main()
