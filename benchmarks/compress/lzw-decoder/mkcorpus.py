"""Write silesia.tar and tar-compress.tar.Z into the folder given first, from the Silesia folder given second."""
import subprocess, sys, tarfile
from pathlib import Path
source = Path(sys.argv[2])
out = Path(sys.argv[1])
def anonymous(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info
plain = out / "silesia.tar"
with tarfile.open(plain, "w", format=tarfile.USTAR_FORMAT) as tar:
    tar.add(source, arcname=source.name, filter=anonymous)
with plain.open("rb") as i, (out / "tar-compress.tar.Z").open("wb") as o:
    subprocess.run(["compress", "-c"], stdin=i, stdout=o, check=True)
