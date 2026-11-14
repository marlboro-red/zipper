"""Write hostile and edge .Z streams for the peak-memory comparison into the folder given."""
import os
import random
import subprocess
import sys
from pathlib import Path

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)


def pack(codes_with_widths):
    """Pack (code, width) pairs in compress groups: 8 codes per group, a width change pads the group."""
    data = bytearray(b"\x1f\x9d\x90")
    group, width = [], None

    def flush(padded):
        nonlocal group
        if not group:
            return
        bits = 0
        for index, code in enumerate(group):
            bits |= code << (index * width)
        size = width if padded else (len(group) * width + 7) // 8
        data.extend(bits.to_bytes(width, "little")[:size])
        group = []

    for code, bits in codes_with_widths:
        if width is not None and bits != width:
            flush(True)
        width = bits
        group.append(code)
        if len(group) == 8 or code == 256:
            flush(True)
    flush(False)
    return bytes(data)


zeros = subprocess.run(["compress", "-c"], input=bytes(64 << 20), capture_output=True, check=True).stdout
(out / "zeros.bin.Z").write_bytes(zeros)
clears = []
for _ in range(400_000):
    clears += [(65, 9), (256, 9)]
(out / "clears.bin.Z").write_bytes(pack(clears))
growing = [(97, 9)]
free, width = 257, 9
while free < 65536:
    if free > (1 << width) - 1 and width < 16:
        width += 1
    growing.append((free, width))
    free += 1
(out / "longest.bin.Z").write_bytes(pack(growing))
rng = random.Random(5)
(out / "random.bin.Z").write_bytes(b"\x1f\x9d\x90" + rng.randbytes(16 << 20))
whole = (out.parent / "out" / "tar-compress.tar.Z").read_bytes()
(out / "truncated.tar.Z").write_bytes(whole[: len(whole) // 2])
for path in sorted(out.iterdir()):
    print(path.name, path.stat().st_size)
