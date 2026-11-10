#!/usr/bin/env python3
"""Run the decode_speed_against_lzma_rust2 probe binaries in alternating order.

Each binary run decodes the probe stream six times with each decoder,
alternating which goes first. Round 0 of every run is warmup. The raw
lines go to samples-probe.json beside this script.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2] / "scripts"))
from bench_common import refuse_when_busy  # noqa: E402

PROBE = "lzma::speed::decode_speed_against_lzma_rust2"
LINE = re.compile(r"PROBE round (\d+): zipper ([\d.]+)(ms|s), lzma-rust2 ([\d.]+)(ms|s)")


def seconds(value, unit):
    return float(value) / (1000 if unit == "ms" else 1)


def main():
    work = Path(sys.argv[1])
    binaries = {"asm": work / "probe-asm", "safe": work / "probe-safe"}
    busy = refuse_when_busy(True)
    runs = []
    for session in range(4):
        for build in (["asm", "safe"] if session % 2 == 0 else ["safe", "asm"]):
            out = subprocess.run([binaries[build], "--ignored", "--exact", PROBE, "--nocapture"],
                                 capture_output=True, text=True, check=True, cwd=HERE.parents[2])
            rounds = [{"round": int(m[1]), "zipper": seconds(m[2], m[3]), "lzma_rust2": seconds(m[4], m[5])}
                      for m in LINE.finditer(out.stderr)]
            if len(rounds) != 6:
                raise RuntimeError(f"{build}: expected 6 rounds\n{out.stderr}")
            runs.append({"session": session, "build": build, "load": os.getloadavg(), "rounds": rounds})
            print(build, [(r["zipper"], r["lzma_rust2"]) for r in rounds], file=sys.stderr, flush=True)
    (HERE / "samples-probe.json").write_text(json.dumps({"busy_at_start": busy, "runs": runs}, indent=1) + "\n")


if __name__ == "__main__":
    main()
