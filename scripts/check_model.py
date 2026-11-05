#!/usr/bin/env python3
"""Run TLC on TLA+ models from a copy outside the repository; pass only when every rule holds.

Usage: check_model.py FOLDER MODEL.tla:CONFIG.cfg [MODEL.tla:CONFIG.cfg ...]

Each pair runs through the pstack statechart runner, found at $STATECHART or at its default path
under ~/.claude/skills. `cargo xtask verify-sync` runs this as a section's full check, so a TLA+
check is never recorded as passed without TLC running on the code being synced.
"""
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

DEFAULT_RUNNER = Path.home() / ".claude/skills/statechart/scripts/statechart"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    folder = Path(sys.argv[1]).resolve(strict=True)
    runner = Path(os.environ.get("STATECHART", DEFAULT_RUNNER))
    if not runner.is_file():
        sys.exit(f"no TLC runner at {runner}; install the pstack statechart skill or set STATECHART")
    failed = []
    with tempfile.TemporaryDirectory(prefix="zipper-tlc-") as scratch:
        copy = Path(scratch) / folder.name
        shutil.copytree(folder, copy)
        for pair in sys.argv[2:]:
            model, config = pair.split(":")
            out = Path(scratch) / f"{Path(model).stem}-{Path(config).stem}"
            run = subprocess.run([str(runner), str(copy / model), "--config", str(copy / config), "--out", str(out)],
                                 capture_output=True, text=True)
            report = out / "report.md"
            holding = ""
            if report.exists():
                lines = report.read_text().splitlines()
                if "## Rules that hold" in lines:
                    holding = lines[lines.index("## Rules that hold") + 1]
            print(f"{folder.name} {model} {config}: exit {run.returncode}; rules that hold: {holding or 'none'}")
            if run.returncode != 0:
                print(run.stdout[-2000:] + run.stderr[-2000:], file=sys.stderr)
                failed.append(pair)
    if failed:
        sys.exit(f"{folder.name}: {', '.join(failed)} did not pass")


if __name__ == "__main__":
    main()
