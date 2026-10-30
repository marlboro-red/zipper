#!/usr/bin/env python3
"""Deterministic ZIP entries spanning repeat distances and compression ratios."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    rng = random.Random(20261030)
    size = 16 * 1024 * 1024
    blocks = {
        'zero': bytes(1024),
        'text': b'record=example; enabled=true\n',
        'runs': b'A' * 4096 + b'B' * 4096 + b'C' * 4096,
        'period-256': rng.randbytes(256),
        'period-8192': rng.randbytes(8192),
        'period-32768': rng.randbytes(32768),
        'random': rng.randbytes(size),
    }
    manifest = {'seed': 20261030, 'deflate_level': 6, 'entries': {}}
    for name, block in blocks.items():
        root = args.output / name
        root.mkdir()
        data = (block * (size // len(block) + 1))[:size]
        (root / 'data.bin').write_bytes(data)
        path = root.with_suffix('.zip')
        with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            info = zipfile.ZipInfo(name + '/data.bin', (2020, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data, compresslevel=6)
        manifest['entries'][name] = {
            'input_bytes': len(data), 'input_sha256': hashlib.sha256(data).hexdigest(),
            'archive_bytes': path.stat().st_size,
            'archive_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
        }
    (args.output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


if __name__ == '__main__':
    main()
