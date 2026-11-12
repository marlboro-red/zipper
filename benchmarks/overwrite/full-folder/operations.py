"""Exploration only: per-file cost of the operations a full-folder publish could use on APFS.

Usage: ops2.py WORK
28,000 files of about 1.5 kB in 280 folders, as the M29 fixture has.
"""
import ctypes, os, sys, time, shutil
from concurrent.futures import ThreadPoolExecutor

libc = ctypes.CDLL(None, use_errno=True)
SWAP, EXCL = 2, 4
work = sys.argv[1]
folders = [f"pkg{p:03}/sub{s}" for p in range(28) for s in range(10)]
names = [(folder, f"f{n:03}") for folder in folders for n in range(100)]


def setup(sides):
    shutil.rmtree(work, ignore_errors=True)
    for side in sides:
        for folder in folders:
            os.makedirs(f"{work}/{side}/{folder}")
            for n in range(100):
                with open(f"{work}/{side}/{folder}/f{n:03}", "wb") as out:
                    out.write(b"x" * 1500)


def report(label, seconds, count=28000):
    print(f"{label:44} {seconds:6.3f} s {seconds / count * 1e6:7.1f} us/file", flush=True)


def renames(label, pairs, flags):
    start = time.perf_counter()
    for a, b in pairs:
        if libc.renamex_np(a, b, flags) != 0:
            raise OSError(ctypes.get_errno(), a)
    report(label, time.perf_counter() - start, len(pairs))


def unlink_folder(folder):
    fd = os.open(folder, os.O_RDONLY)
    try:
        for name in os.listdir(fd):
            os.unlink(name, dir_fd=fd)
    finally:
        os.close(fd)
    os.rmdir(folder)


def unlink_tree(side, threads):
    paths = [f"{work}/{side}/{folder}" for folder in folders]
    start = time.perf_counter()
    if threads == 1:
        for path in paths:
            unlink_folder(path)
    else:
        with ThreadPoolExecutor(threads) as pool:
            list(pool.map(unlink_folder, paths))
    report(f"unlink by dirfd, {threads} thread(s)", time.perf_counter() - start)


for threads in (1, 2, 3, 4, 8):
    setup("a")
    time.sleep(1)
    unlink_tree("a", threads)

setup("ab")
start = time.perf_counter()
if libc.renamex_np(f"{work}/a".encode(), f"{work}/b".encode(), SWAP) != 0:
    raise OSError(ctypes.get_errno())
report("swap two top folders, one call", time.perf_counter() - start, 1)
pairs = [(f"{work}/a/{d}".encode(), f"{work}/b/{d}".encode()) for d in folders]
renames("swap 280 leaf folders", pairs, SWAP)
file_pairs = [(f"{work}/a/{d}/{f}".encode(), f"{work}/b/{d}/{f}".encode()) for d, f in names]
renames("swap each file across trees", file_pairs, SWAP)
renames("rename replacing each file", file_pairs, 0)
start = time.perf_counter()
shutil.rmtree(f"{work}/b")
report("shutil.rmtree", time.perf_counter() - start)

setup("a")
for folder in folders:
    os.makedirs(f"{work}/b/{folder}")
renames("move each file, RENAME_EXCL", [(f"{work}/a/{d}/{f}".encode(), f"{work}/b/{d}/{f}".encode()) for d, f in names], EXCL)
start = time.perf_counter()
for d, f in names:
    os.lstat(f"{work}/b/{d}/{f}")
report("lstat each file", time.perf_counter() - start)
start = time.perf_counter()
count = 0
for folder in folders:
    with os.scandir(f"{work}/b/{folder}") as entries:
        for entry in entries:
            count += entry.inode() != 0
report("scandir each folder", time.perf_counter() - start)
start = time.perf_counter()
for d, f in names:
    with open(f"{work}/b/{d}/{f}", "rb") as handle:
        handle.read()
report("open, read, close each file", time.perf_counter() - start)
shutil.rmtree(work, ignore_errors=True)
