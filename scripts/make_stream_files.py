#!/usr/bin/env python3
"""Build single compressed files that 7-Zip's parity corpus cannot hold.

7-Zip reads no lzip and no LZ4, so tests/data/stream holds hello.txt in
those formats, written by libarchive through bsdtar's raw format. Each file
must decode, through a second decoder, to the text below. The lzip check
uses Python's raw LZMA and the CRC and sizes in the lzip trailer. The LZ4
check is a small frame reader. Run on macOS with bsdtar."""
import argparse
import lzma
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

TEXT = b"hello from the parity corpus\n"


def lzip_members(data):
    """Each lzip member is a 6-byte header, raw LZMA with an end marker,
    then the CRC32, the data size and the member size."""
    out = b""
    while data:
        assert data[:5] == b"LZIP\x01", "not an lzip member"
        exponent = data[5] & 0x1F
        dictionary = (1 << exponent) - ((1 << exponent) // 16) * (data[5] >> 5)
        decoder = lzma.LZMADecompressor(lzma.FORMAT_RAW, filters=[{
            "id": lzma.FILTER_LZMA1, "dict_size": dictionary, "lc": 3, "lp": 0, "pb": 2}])
        decoded = decoder.decompress(data[6:])
        trailer = decoder.unused_data
        crc, size, member = struct.unpack("<IQQ", trailer[:20])
        assert crc == zlib.crc32(decoded) and size == len(decoded), "lzip trailer disagrees"
        assert member == len(data) - len(trailer) + 20, "lzip member size disagrees"
        out += decoded
        data = trailer[20:]
    return out


def xxh32(data, seed=0):
    """xxHash32, which LZ4 frames use for their checksums."""
    primes = [2654435761, 2246822519, 3266489917, 668265263, 374761393]
    mask = 0xFFFFFFFF
    rotate = lambda value, bits: (value << bits | value >> (32 - bits)) & mask
    round_ = lambda lane, word: rotate((lane + word * primes[1]) & mask, 13) * primes[0] & mask
    index = 0
    if len(data) >= 16:
        lanes = [(seed + primes[0] + primes[1]) & mask, (seed + primes[1]) & mask, seed,
                 (seed - primes[0]) & mask]
        while index + 16 <= len(data):
            words = struct.unpack("<4I", data[index:index + 16])
            lanes = [round_(lane, word) for lane, word in zip(lanes, words)]
            index += 16
        value = (rotate(lanes[0], 1) + rotate(lanes[1], 7) + rotate(lanes[2], 12) + rotate(lanes[3], 18)) & mask
    else:
        value = (seed + primes[4]) & mask
    value = (value + len(data)) & mask
    while index + 4 <= len(data):
        (word,) = struct.unpack("<I", data[index:index + 4])
        value = rotate((value + word * primes[2]) & mask, 17) * primes[3] & mask
        index += 4
    for byte in data[index:]:
        value = rotate((value + byte * primes[4]) & mask, 11) * primes[0] & mask
    value ^= value >> 15
    value = value * primes[1] & mask
    value ^= value >> 13
    value = value * primes[2] & mask
    return value ^ value >> 16


def lz4_frame(data):
    """One LZ4 frame, with its header, block and content checksums checked."""
    assert data[:4] == b"\x04\x22\x4d\x18", "not an LZ4 frame"
    flags = data[4]
    assert flags >> 6 == 1 and flags & 0x01 == 0, "an unknown version or a dictionary ID"
    position = 6 + (8 if flags & 0x08 else 0)
    assert data[position] == xxh32(data[4:position]) >> 8 & 0xFF, "LZ4 header checksum"
    position += 1
    out = bytearray()
    while True:
        (size,) = struct.unpack("<I", data[position:position + 4])
        position += 4
        if size == 0:
            break
        block = data[position:position + (size & 0x7FFFFFFF)]
        position += size & 0x7FFFFFFF
        if flags & 0x10:
            assert struct.unpack("<I", data[position:position + 4])[0] == xxh32(block), "LZ4 block checksum"
            position += 4
        out += block if size & 0x80000000 else lz4_block(block)
    if flags & 0x04:
        assert struct.unpack("<I", data[position:position + 4])[0] == xxh32(bytes(out)), "LZ4 content checksum"
        position += 4
    assert position == len(data), "data after the LZ4 frame"
    return bytes(out)


def lz4_length(block, index, length):
    """A 4-bit length of 15 continues in bytes until one is below 255."""
    if length == 15:
        while True:
            more = block[index]
            index += 1
            length += more
            if more != 255:
                break
    return length, index


def lz4_block(block):
    out, index = bytearray(), 0
    while True:
        token = block[index]
        literals, index = lz4_length(block, index + 1, token >> 4)
        out += block[index:index + literals]
        index += literals
        if index == len(block):
            return out
        offset = block[index] | block[index + 1] << 8
        assert 0 < offset <= len(out), "LZ4 match before the start"
        length, index = lz4_length(block, index + 2, token & 15)
        for _ in range(length + 4):
            out.append(out[-offset])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("tests/data/stream"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="zipper-stream-") as work:
        (Path(work) / "hello.txt").write_bytes(TEXT)
        for option, extension, check in [("--lzip", "lz", lzip_members), ("--lz4", "lz4", lz4_frame)]:
            target = (args.output / f"hello.txt.{extension}").resolve()
            subprocess.run(["bsdtar", "--format", "raw", option, "-cf", str(target), "hello.txt"],
                           cwd=work, check=True, env={**os.environ, "COPYFILE_DISABLE": "1"})
            assert check(target.read_bytes()) == TEXT, f"{target.name} does not decode to the text"


if __name__ == "__main__":
    main()
