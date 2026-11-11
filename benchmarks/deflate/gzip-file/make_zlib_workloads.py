"""Write the zlib workloads of this record from the Silesia files.

Usage: make_zlib_workloads.py SILESIA_FOLDER OUT_FOLDER

It writes OUT/blob.bin, the twelve files in name order, OUT/blob.bin.gz at level 6 with mtime 0,
OUT/blob.swf, a zlib-compressed SWF whose body is a 13-byte movie header and then the blob, with
OUT/blob~.swf, the uncompressed SWF it must decode to, and OUT/silesia.xar, the folder packed by
the system `xar` with zlib at level 6.
"""
import gzip
import struct
import subprocess
import sys
import zlib
from pathlib import Path

source, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=True)
blob = b"".join(path.read_bytes() for path in sorted(source.iterdir()) if path.is_file())
(out / "blob.bin").write_bytes(blob)
with (out / "blob.bin.gz").open("wb") as sink:
    with gzip.GzipFile(filename="", mode="wb", compresslevel=6, fileobj=sink, mtime=0) as packed:
        packed.write(blob)
movie_header = bytes([0x78, 0x00, 0x05, 0x5F, 0x00, 0x00, 0x0F, 0xA0, 0x00]) + bytes([0x00, 0x0C, 0x01, 0x00])
body = movie_header + blob
header = struct.pack("<3sBI", b"FWS", 10, 8 + len(body))
(out / "blob~.swf").write_bytes(header + body)
(out / "blob.swf").write_bytes(b"C" + header[1:] + zlib.compress(body, 6))
archive = out / "silesia.xar"
archive.unlink(missing_ok=True)
subprocess.run(["xar", "--compression=gzip", "--compression-args=6", "-cf", str(archive), source.name],
               cwd=source.parent, check=True)
for path in sorted(out.iterdir()):
    print(path.name, path.stat().st_size)
