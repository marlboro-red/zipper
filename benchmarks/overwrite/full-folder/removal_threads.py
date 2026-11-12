"""Exploration only: how the removal of 28,000 old files in 280 folders depends on threads and order on APFS.

Usage: ops3.py WORK
"""
import os, sys, time, shutil
from concurrent.futures import ThreadPoolExecutor

work = sys.argv[1]
folders = [f"pkg{p:03}/sub{s}" for p in range(28) for s in range(10)]


def setup():
    shutil.rmtree(work, ignore_errors=True)
    for folder in folders:
        os.makedirs(f"{work}/a/{folder}")
        for n in range(100):
            with open(f"{work}/a/{folder}/f{n:03}", "wb") as out:
                out.write(b"x" * 1500)
    time.sleep(1)


def unlink_folder(job):
    folder, by_inode = job
    fd = os.open(folder, os.O_RDONLY)
    try:
        with os.scandir(folder) as entries:
            names = [(entry.inode(), entry.name) for entry in entries]
        if by_inode:
            names.sort()
        for _, name in names:
            os.unlink(name, dir_fd=fd)
    finally:
        os.close(fd)


def run(threads, by_inode, rounds=3):
    times = []
    for _ in range(rounds):
        setup()
        jobs = [(f"{work}/a/{folder}", by_inode) for folder in folders]
        start = time.perf_counter()
        if threads == 1:
            for job in jobs:
                unlink_folder(job)
        else:
            with ThreadPoolExecutor(threads) as pool:
                list(pool.map(unlink_folder, jobs))
        times.append(time.perf_counter() - start)
    order = "inode order" if by_inode else "listing order"
    print(f"{threads} thread(s), {order:13} " + " ".join(f"{seconds:.3f}" for seconds in sorted(times)), flush=True)


for threads in (1, 2, 3, 4, 5, 6, 8, 12):
    run(threads, False)
for threads in (1, 4):
    run(threads, True)
shutil.rmtree(work, ignore_errors=True)
