#!/usr/bin/env python3
"""Write the FLV and SWF corpus with ffmpeg, and record what 7-Zip extracts.

    python3 tests/data/swf-corpus/make_corpus.py

ffmpeg writes each FLV and uncompressed SWF from its own test pattern and sine
tone. The compressed SWF files hold an ffmpeg SWF, compressed here with zlib
and with LZMA as Flash Player reads them. trees.json records the SHA-256 of
every file `7zz x` writes from each one, or null when 7-Zip refuses it, as it
refuses ffmpeg's SWF of audio alone, whose header declares a length it never
fills in."""
import hashlib
import json
import lzma
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
VIDEO = ["-f", "lavfi", "-i", "testsrc=size=32x24:rate=5:duration=0.6"]
AUDIO = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=22050:duration=0.6"]
QUIET = ["-hide_banner", "-loglevel", "error", "-y"]
OUTPUT = ["-map_metadata", "-1", "-fflags", "+bitexact", "-flags", "+bitexact"]
FFMPEG = {
    "flv1-mp3.flv": VIDEO + AUDIO + ["-c:v", "flv", "-c:a", "libmp3lame", "-f", "flv"],
    "h264-aac.flv": VIDEO + AUDIO + ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-f", "flv"],
    "flashsv-adpcm.flv": VIDEO + AUDIO + ["-c:v", "flashsv", "-c:a", "adpcm_swf", "-f", "flv"],
    "flashsv2.flv": VIDEO + ["-c:v", "flashsv2", "-f", "flv"],
    "h264.flv": VIDEO + ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-f", "flv"],
    "mp3.flv": AUDIO + ["-c:a", "libmp3lame", "-f", "flv"],
    "nellymoser.flv": AUDIO + ["-c:a", "nellymoser", "-f", "flv"],
    "pcm.flv": AUDIO + ["-c:a", "pcm_s16le", "-f", "flv"],
    "pcm-u8.flv": AUDIO + ["-c:a", "pcm_u8", "-f", "flv"],
    "flv1-mp3.swf": VIDEO + AUDIO + ["-c:v", "flv", "-c:a", "libmp3lame", "-f", "swf"],
    "mjpeg.swf": VIDEO + ["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-f", "swf"],
    "mp3.swf": AUDIO + ["-c:a", "libmp3lame", "-f", "swf"],
    "flv1-avm2.swf": VIDEO + ["-c:v", "flv", "-f", "avm2"],
}
COMPRESSED = {
    "flv1-mp3-zlib.swf": ("flv1-mp3.swf", "zlib"),
    "flv1-mp3-lzma.swf": ("flv1-mp3.swf", "lzma"),
    "mjpeg-zlib.swf": ("mjpeg.swf", "zlib"),
    "mjpeg-lzma.swf": ("mjpeg.swf", "lzma"),
}


def compressed(swf, method):
    body = swf[8:]
    if method == "zlib":
        return b"CWS" + swf[3:8] + zlib.compress(body, 9)
    packed = lzma.compress(body, format=lzma.FORMAT_ALONE)
    properties, data = packed[:5], packed[13:]
    return b"ZWS" + swf[3:8] + struct.pack("<I", len(data)) + properties + data


def tree(root):
    return {
        path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def main():
    sevenzip = shutil.which("7zz") or sys.exit("make_corpus: 7zz is not on PATH")
    ffmpeg = shutil.which("ffmpeg") or sys.exit("make_corpus: ffmpeg is not on PATH")
    for name, arguments in FFMPEG.items():
        subprocess.run([ffmpeg, *QUIET, *arguments, *OUTPUT, str(HERE / name)], check=True)
    for name, (source, method) in COMPRESSED.items():
        (HERE / name).write_bytes(compressed((HERE / source).read_bytes(), method))
    trees = {}
    for name in sorted([*FFMPEG, *COMPRESSED]):
        with tempfile.TemporaryDirectory() as work:
            out = Path(work) / "out"
            run = subprocess.run([sevenzip, "x", "-y", f"-o{out}", str(HERE / name)],
                                 capture_output=True)
            trees[name] = tree(out) if run.returncode == 0 else None
    (HERE / "trees.json").write_text(json.dumps(trees, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
