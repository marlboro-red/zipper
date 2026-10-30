#!/usr/bin/env python3
"""Deterministic large single-entry ZIP fixtures for decode/output diagnostics."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)
rng = random.Random(20261030)
mixed = rng.getrandbits(8 * 32768).to_bytes(32768, 'little') + b'record=example; enabled=true\n' * 1024
for name in ['mixed', 'compressible', 'random']:
    root = args.output / name
    root.mkdir()
    path = root / 'large.bin'
    with path.open('wb') as out:
        for _ in range(1024 if name == 'mixed' else 32):
            if name == 'mixed':
                block = mixed
            elif name == 'compressible':
                block = (b'record=example; value=42\n' * 50000)[:1024*1024]
            else:
                block = rng.getrandbits(8*1024*1024).to_bytes(1024*1024, 'little')
            out.write(block)
    archive = root.with_suffix('.zip')
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as out:
        info = zipfile.ZipInfo(name + '/large.bin', (2020, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.create_system = 3
        info.external_attr = 0o100644 << 16
        with path.open('rb') as source, out.open(info, 'w') as entry:
            for block in iter(lambda: source.read(1024*1024), b''):
                entry.write(block)
    def digest(p):
        h = hashlib.sha256()
        with p.open('rb') as source:
            for block in iter(lambda: source.read(1024*1024), b''):
                h.update(block)
        return h.hexdigest()
    root.with_suffix('.manifest.json').write_text(json.dumps({
        'seed': 20261030, 'bytes': path.stat().st_size, 'sha256': digest(path),
        'archive_bytes': archive.stat().st_size, 'archive_sha256': digest(archive)}, indent=2)+'\n')
    print(name, path.stat().st_size, archive.stat().st_size)
