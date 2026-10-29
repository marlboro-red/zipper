#!/usr/bin/env python3
"""Portable, interleaved extraction comparisons with direct byte verification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from process_metrics import timed
from compare_revisions import inventory, verify


def digest(path):
    with path.open('rb') as stream:
        hasher = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(block)
        return hasher.hexdigest()



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('--zipper', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--work-dir', type=Path, help='parent for temporary archives and restored trees')
    parser.add_argument('--threads', type=int, nargs='+', default=[1, 2, 4])
    parser.add_argument('--runs', type=int, default=7)
    parser.add_argument('--native-format', action='store_true', help='also create/extract native level-1, 1 MiB-block archive')
    parser.add_argument('--sevenzip')
    parser.add_argument('--tar', help='libarchive bsdtar executable, not GNU tar')
    args = parser.parse_args()
    if args.runs < 1 or any(not 1 <= n <= 64 for n in args.threads):
        parser.error('positive runs and threads 1..64 required')
    source = args.source.resolve(strict=True)
    if not source.is_dir() or source.is_symlink():
        parser.error('source must be a regular directory')
    zipper = args.zipper.resolve(strict=True)
    expected = inventory(source)
    total = sum(n for n in expected.values() if n is not None)
    files = sum(n is not None for n in expected.values())
    result = {
        'platform': platform.platform(), 'machine': platform.machine(),
        'cpu_count': os.cpu_count(), 'python': platform.python_version(),
        'source': str(source), 'files': files, 'input_bytes': total,
        'runs': args.runs, 'warmup_rounds': 1,
        'cache': 'uncontrolled, warmed by creation and prior runs; not cold-cache',
        'semantics': 'bytes and directory membership; metadata fidelity not compared; no fsync',
        'memory': 'single process peak working set (Windows) or wait4 peak RSS (Unix); not process tree',
        'verification': 'direct bytes and exact full output tree, outside timed region',
        'zipper_sha256': digest(zipper), 'commands': {}, 'archives': {}, 'results': {},
        'manifest': {name: {'bytes': size, 'sha256': digest(source / name)} if size is not None
                     else None for name, size in sorted(expected.items())},
    }
    rng = random.Random(20261030)
    with tempfile.TemporaryDirectory(prefix='zipper-matrix-', dir=args.work_dir) as work:
        work = Path(work).resolve()
        archive = work / 'input.zip'
        plain = work / 'input.tar'
        print("Creating ZIP and TAR fixtures", file=sys.stderr, flush=True)
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as out:
            # Fixed metadata and ordering make identical fixture bytes portable.
            for path in [source] + sorted(source.rglob('*')):
                name = path.relative_to(source.parent).as_posix()
                info = zipfile.ZipInfo(name + '/' if path.is_dir() else name, (2020, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = ((0o40755 if path.is_dir() else 0o100644) << 16)
                info.compress_type = zipfile.ZIP_DEFLATED
                with out.open(info, 'w') as entry:
                    if path.is_file():
                        with path.open('rb') as data:
                            shutil.copyfileobj(data, entry)
        with tarfile.open(plain, 'w', format=tarfile.USTAR_FORMAT) as out:
            for path in [source] + sorted(source.rglob('*')):
                info = out.gettarinfo(str(path), path.relative_to(source.parent).as_posix())
                info.uid = info.gid = info.mtime = 0
                info.uname = info.gname = ''
                info.mode = 0o755 if path.is_dir() else 0o644
                if path.is_file():
                    with path.open('rb') as data:
                        out.addfile(info, data)
                else:
                    out.addfile(info)
        archives = [archive, plain]
        native = work / 'input.zipper'
        if args.native_format:
            create = [str(zipper), '--threads', '4', 'create', str(native), str(source),
                      '--level', '1', '--block-size-kib', '1024']
            subprocess.run(create, check=True, stdout=subprocess.DEVNULL)
            result['native_creation_command'] = create
            archives.append(native)
        for path in archives:
            result['archives'][path.name] = {'bytes': path.stat().st_size, 'sha256': digest(path)}
        output = work / 'out'
        commands = {}
        for threads in sorted(set(args.threads)):
            if args.native_format:
                commands[f'zipper-{threads}'] = [str(zipper), '--threads', str(threads),
                    'extract', str(native), '-o', str(output)]
            for mode in ['native', 'native-fast']:
                commands[f'{mode}-{threads}'] = [str(zipper), '--zip-engine', mode,
                    '--threads', str(threads), 'extract', str(archive), '-o', str(output)]
        for label, executable, version_args in [('7zip', args.sevenzip, ['i']), ('bsdtar', args.tar, ['--version'])]:
            if executable:
                resolved = shutil.which(executable)
                if not resolved:
                    parser.error(f'executable not found: {executable}')
                result[label] = {'binary': resolved, 'sha256': digest(Path(resolved)),
                    'version': subprocess.check_output([resolved] + version_args, text=True, errors='replace')}
                if label == '7zip':
                    commands[label] = [resolved, 'x', '-y', f'-o{output}', str(archive)]
                else:
                    for kind, path in [('zip', archive), ('tar', plain)]:
                        commands[f'bsdtar-{kind}'] = [resolved, '-xf', str(path), '-C', str(output),
                            '-b', '2048', '-m', '--no-same-owner', '--no-same-permissions',
                            '--no-acls', '--no-fflags', '--no-xattrs', '--no-mac-metadata']
        result['commands'] = commands
        samples = {name: [] for name in commands}
        result['order_including_warmup'] = []
        for round_index in range(args.runs + 1):
            order = list(commands)
            rng.shuffle(order)
            result['order_including_warmup'].append(order)
            for name in order:
                # bsdtar requires the extraction root; Zipper requires it absent.
                # External root creation is excluded for both external readers.
                if name.startswith('bsdtar') or name == '7zip':
                    output.mkdir()
                print(f'Round {round_index}: {name}', file=sys.stderr, flush=True)
                row = timed(commands[name], source.parent)
                print(f'  extracted in {row["seconds"]:.3f}s; verifying', file=sys.stderr, flush=True)
                wanted = {source.name: None}
                wanted.update({str(Path(source.name) / key): value for key, value in expected.items()})
                if inventory(output) != wanted:
                    raise RuntimeError('full output tree mismatch')
                row['verified_bytes'] = verify(source, output / source.name, expected)
                if row['verified_bytes'] != total:
                    raise RuntimeError('verified byte count mismatch')
                if round_index:
                    samples[name].append(row)
                shutil.rmtree(output)
                print('  verified and cleaned', file=sys.stderr, flush=True)
        for name, rows in samples.items():
            seconds = [row['seconds'] for row in rows]
            median = statistics.median(seconds)
            result['results'][name] = {'samples': rows, 'median_seconds': median,
                'min_seconds': min(seconds), 'max_seconds': max(seconds),
                'files_s': files / median, 'mib_s': total / 2**20 / median,
                'peak_rss_bytes': max(row['peak_rss_bytes'] for row in rows)}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    for name, row in result['results'].items():
        print(f"{name}: {row['median_seconds'] * 1000:.2f} ms, {row['peak_rss_bytes'] / 2**20:.2f} MiB")


if __name__ == '__main__':
    main()
