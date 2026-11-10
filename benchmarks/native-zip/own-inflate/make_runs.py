import sys
import zipfile

out = sys.argv[1]
size = 64 << 20
with zipfile.ZipFile(out + "/runs-1.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    z.writestr("zeros.bin", bytes(size))
with zipfile.ZipFile(out + "/runs-3.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    z.writestr("abc.bin", (b"abc" * (size // 3 + 1))[:size])
with zipfile.ZipFile(out + "/runs-12.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    z.writestr("twelve.bin", (bytes(range(12)) * (size // 12 + 1))[:size])
