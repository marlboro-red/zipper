#!/usr/bin/env python3
"""Materialize locked crates.io dependency sources as a real-code extraction corpus."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1'], text=True))
    packages = [p for p in metadata['packages'] if p.get('source') ==
                'registry+https://github.com/rust-lang/crates.io-index']
    args.output.mkdir(parents=True, exist_ok=False)
    manifest = {'cargo_lock_sha256': digest(Path(metadata['workspace_root']) / 'Cargo.lock'),
                'packages': [], 'files': {}, 'skipped_symlinks': []}
    for package in sorted(packages, key=lambda p: (p['name'], p['version'])):
        root = Path(package['manifest_path']).parent
        prefix = f"{package['name']}-{package['version']}"
        destination = args.output / prefix
        destination.mkdir(exist_ok=False)
        manifest['packages'].append({'name': package['name'], 'version': package['version'],
                                     'source': package['source']})
        for source in sorted(root.rglob('*')):
            relative = Path(prefix) / source.relative_to(root)
            target = args.output / relative
            if source.is_symlink():
                manifest['skipped_symlinks'].append(relative.as_posix())
            elif source.is_dir():
                target.mkdir(exist_ok=True)
            elif source.is_file():
                shutil.copyfile(source, target)
                manifest['files'][relative.as_posix()] = {
                    'bytes': target.stat().st_size, 'sha256': digest(target)}
            else:
                raise ValueError(f'unsupported source type: {source}')
    sidecar = args.output.with_name(args.output.name + '.manifest.json')
    sidecar.write_text(json.dumps(manifest, indent=2) + '\n')
    print(f"{len(packages)} packages, {len(manifest['files'])} files, "
          f"{sum(f['bytes'] for f in manifest['files'].values())} bytes")


if __name__ == '__main__':
    main()
