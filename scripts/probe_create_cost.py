#!/usr/bin/env python3
"""Measure the kernel cost of creating small files, outside any Zipper code.

Each variant creates FILES empty or 512-byte files with O_CREAT|O_EXCL, in
interleaved shuffled rounds, and records wall and CPU time per file. Threads
release the GIL inside os.open, os.write and os.close.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import tempfile
import threading
import time

DATA = b'x' * 512


def create(directory, start, count, write, times):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    for index in range(start, start + count):
        fd = os.open(os.path.join(directory, f'record-{index:06}.dat'), flags, 0o644)
        if write and index % 5:
            os.write(fd, DATA)
        if times:
            os.utime(fd, (1600000000, 1600000000))
        os.close(fd)


def variant(root, files, threads, shared_directory, write, times):
    top = Path(tempfile.mkdtemp(dir=root))
    per_thread = files // threads
    directories = [top / ('shared' if shared_directory else f'thread-{n}') for n in range(threads)]
    for directory in set(directories):
        directory.mkdir()
    workers = [threading.Thread(target=create, args=(str(directories[n]), n * per_thread, per_thread, write, times))
               for n in range(threads)]
    cpu, wall = time.process_time(), time.perf_counter()
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join()
    wall, cpu = time.perf_counter() - wall, time.process_time() - cpu
    shutil.rmtree(top)
    total = per_thread * threads
    return {'wall_us_per_file': wall * 1e6 / total, 'cpu_us_per_file': cpu * 1e6 / total}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True, help='directory on the volume under test')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--files', type=int, default=10000)
    parser.add_argument('--runs', type=int, default=7)
    args = parser.parse_args()
    variants = {
        'create-close-1': (1, True, False, False),
        'create-write-close-1': (1, True, True, False),
        'create-write-times-close-1': (1, True, True, True),
        'create-close-4-shared-dir': (4, True, False, False),
        'create-close-4-own-dirs': (4, False, False, False),
        'create-write-times-close-2': (2, True, True, True),
        'create-write-times-close-4': (4, True, True, True),
        'create-write-times-close-8': (8, True, True, True),
    }
    rng = random.Random(20261102)
    samples = {name: [] for name in variants}
    for round_index in range(args.runs + 1):
        order = list(variants)
        rng.shuffle(order)
        for name in order:
            row = variant(args.root, args.files, *variants[name])
            if round_index:
                samples[name].append(row)
    result = {'platform': platform.platform(), 'machine': platform.machine(), 'cpu_count': os.cpu_count(),
              'root': str(args.root.resolve()), 'files': args.files, 'runs': args.runs, 'warmup_rounds': 1,
              'samples': samples, 'medians': {}}
    for name, rows in samples.items():
        wall = statistics.median(row['wall_us_per_file'] for row in rows)
        cpu = statistics.median(row['cpu_us_per_file'] for row in rows)
        spread = (min(row['wall_us_per_file'] for row in rows), max(row['wall_us_per_file'] for row in rows))
        result['medians'][name] = {'wall_us_per_file': wall, 'cpu_us_per_file': cpu, 'wall_range': spread}
        print(f'{name:28} wall {wall:6.1f} [{spread[0]:.1f}-{spread[1]:.1f}] us/file, CPU {cpu:6.1f} us/file')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
