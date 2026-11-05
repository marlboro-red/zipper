#!/usr/bin/env python3
"""Run the fixed extraction scoreboard and render docs/results.md from its JSON."""
import argparse
import datetime
import hashlib
import json
import statistics
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

from extraction_matrix import write_zip

from bench_common import refuse_when_busy

ROOT = Path(__file__).resolve().parent.parent
SCOREBOARD = ROOT / 'benchmarks' / 'scoreboard'
RESULTS = ROOT / 'docs' / 'results.md'
REALWORLD = ROOT / 'benchmarks' / 'realworld'
CODECS = SCOREBOARD / 'codecs'
CODEC_ORDER = ['zip-deflate64', 'zip-bzip2', 'zip-lzma', 'zip-xz', 'zip-ppmd', 'zip-zstd', '7z-deflate64',
               '7z-bzip2', '7z-ppmd', '7z-bcj2', '7z-swap4', 'tar-bzip2', 'tar-compress', 'tar-lzip', 'tar-lz4']
REALWORLD_SOURCES = {'pypi-sdist': 'PyPI source packages', 'pypi-wheel': 'PyPI wheels', 'npm': 'npm packages',
                     'crate': 'Crates', 'maven': 'Maven jars', 'go': 'Go modules',
                     'github': 'GitHub source archives', 'release': 'Release tarballs'}
CORPORA = ROOT / 'target' / 'bench-corpora'
WORK = ROOT / 'target' / 'bench-work'
THREADS = ['1', '4']
COUNT_ZIP = ROOT / 'target' / 'counters' / 'release' / 'examples' / ('count_zip.exe' if platform.system() == 'Windows' else 'count_zip')
COUNTED_ENGINES = ['native', 'native-fast']

GENERATORS = {
    'flat-tiny': ['make_tiny_corpus.py', '--files', '10000', '--layout', 'flat'],
    'mixed': ['make_benchmark_corpus.py', '--mib', '16', '--skip-binaries'],
    'dependencies': ['make_dependency_corpus.py'],
    'silesia': ['fetch_silesia.py'],
    'large': ['make_large_zip_corpus.py', '--mib', '256'],
}

WORKLOADS = {
    'flat-tiny': ('flat-tiny', '.', '10,000 flat tiny files'),
    'grouped': ('mixed', 'small-files', '4,000 grouped files'),
    'dependencies': ('dependencies', '.', 'Rust dependency sources'),
    'silesia': ('silesia', '.', 'Silesia'),
    'large-compressible': ('large', 'compressible', 'One large compressible entry'),
    'large-random': ('large', 'random', 'One large random entry'),
    'mixed': ('mixed', '.', 'Mixed tree'),
}

ROW_ORDER = ['native-auto', 'native-1', 'native-4', 'native-fast-1', 'native-fast-4', 'bsdtar-zip', '7zip', 'bsdtar-tar']
PARITY_ROW = 'bsdtar-zip'
TAR_PARITY = {'tar-native': 'bsdtar-tar-default', 'bsdtar-tar-default': 'bsdtar-tar-default',
              'tar-gz-native': 'bsdtar-tar-gz', 'bsdtar-tar-gz': 'bsdtar-tar-gz'}


def git(*args):
    return subprocess.check_output(['git', *args], cwd=ROOT, text=True).strip()


def prepared(corpus):
    """Generate a corpus once into target/bench-corpora, marking it complete."""
    home = CORPORA / corpus
    if (home / '.complete').exists():
        return home / corpus
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    script, *options = GENERATORS[corpus]
    subprocess.run([sys.executable, str(ROOT / 'scripts' / script), str(home / corpus), *options],
                   cwd=ROOT, check=True)
    (home / '.complete').write_text('')
    return home / corpus


def digest_manifest(result):
    text = json.dumps(result.pop('manifest'), sort_keys=True).encode()
    result['manifest_sha256'] = hashlib.sha256(text).hexdigest()


def previous_session(path, result):
    """Medians of the last session that measured the same Zipper binary, for drift."""
    if not path.exists():
        return None
    earlier = json.loads(path.read_text(encoding='utf-8'))
    if earlier['zipper_sha256'] != result['zipper_sha256']:
        return None
    return {'recorded': earlier['recorded'], 'revision': earlier['revision'],
            'median_seconds': {name: row['median_seconds'] for name, row in earlier['results'].items()}}


def sha256(path):
    hasher = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            hasher.update(block)
    return hasher.hexdigest()


def counted(name, result):
    """Counters from one extraction per engine at one thread, outside timed runs."""
    archive = WORK / f'{name}.zip'
    if not archive.exists():
        corpus, subpath, _ = WORKLOADS[name]
        WORK.mkdir(parents=True, exist_ok=True)
        write_zip((prepared(corpus) / subpath).resolve(), archive)
    if not archive.exists() or sha256(archive) != result['archives']['input.zip']['sha256']:
        sys.exit(f'{archive} is not the ZIP the {name} session measured')
    rows = {}
    for engine in COUNTED_ENGINES:
        output = subprocess.check_output([str(COUNT_ZIP), str(archive), engine, '1'], cwd=ROOT, text=True)
        rows[f'{engine}-1'] = json.loads(output)
    return {'revision': git('rev-parse', 'HEAD'), 'tree_clean': not dirty_paths(), 'rows': rows}


def dirty_paths():
    status = subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True)
    return [line for line in status.splitlines()
            if not line[3:].startswith(('benchmarks/scoreboard/', 'docs/results.md'))]


def run(args):
    dirty = dirty_paths()
    if dirty and not args.allow_dirty:
        sys.exit('commit first; the scoreboard names the measured revision:\n' + '\n'.join(dirty))
    busy = refuse_when_busy(args.allow_busy)
    zipper = ROOT / 'target' / 'release' / ('zipper.exe' if platform.system() == 'Windows' else 'zipper')
    names = args.only or list(WORKLOADS)
    if args.counters_only:
        for name in names:
            path = scoreboard / f'{name}.json'
            result = json.loads(path.read_text(encoding='utf-8'))
            result['counters'] = counted(name, result)
            path.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        if not args.no_render:
            RESULTS.write_text(render(), encoding='utf-8')
        return
    scoreboard = args.scoreboard_dir.resolve() if args.scoreboard_dir else SCOREBOARD
    WORK.mkdir(parents=True, exist_ok=True)
    scoreboard.mkdir(parents=True, exist_ok=True)
    for name in names:
        corpus, subpath, _ = WORKLOADS[name]
        source = (prepared(corpus) / subpath).resolve()
        raw = WORK / f'{name}.json'
        command = [sys.executable, str(ROOT / 'scripts' / 'extraction_matrix.py'), str(source),
                   '--zipper', str(zipper), '--output', str(raw), '--work-dir', str(WORK),
                   '--threads', *THREADS, '--runs', str(args.runs), '--save-zip', str(WORK / f'{name}.zip')]
        for flag, variable, names in [('--tar', 'ZIPPER_BENCH_TAR', ['bsdtar']),
                                      ('--sevenzip', 'ZIPPER_BENCH_7Z', ['7zz', '7z'])]:
            tool = os.environ.get(variable) or next(filter(shutil.which, names), None)
            if tool:
                command += [flag, tool]
        print(f'scoreboard: {name}', file=sys.stderr, flush=True)
        subprocess.run(command, cwd=ROOT, check=True)
        result = json.loads(raw.read_text(encoding='utf-8'))
        digest_manifest(result)
        result['workload'] = name
        result['revision'] = git('rev-parse', 'HEAD')
        result['tree_clean'] = not dirty
        result['busy_processes'] = busy
        result['recorded'] = datetime.date.today().isoformat()
        result['previous_session'] = previous_session(scoreboard / f'{name}.json', result)
        result['counters'] = counted(name, result)
        (scoreboard / f'{name}.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    if not args.no_render:
        RESULTS.write_text(render(), encoding='utf-8')


def count(number, noun):
    return f"{number:,} {noun}{'' if number == 1 else 's'}"


def ms(seconds):
    return f'{seconds * 1000:,.2f}'


def table(result, tar=False):
    rows = result['results']
    if tar:
        order = [name for name in TAR_PARITY if name in rows]
        parity_of = lambda name: rows.get(TAR_PARITY[name])
        heading = 'bsdtar time on the same file ÷ row time'
    else:
        order = [name for name in ROW_ORDER if name in rows] + sorted(set(rows) - set(ROW_ORDER) - set(TAR_PARITY))
        parity_of = lambda name: rows.get(PARITY_ROW)
        heading = 'bsdtar-zip time ÷ row time'
    if not order:
        return []
    earlier = (result.get('previous_session') or {}).get('median_seconds', {})
    lines = [f'| Reader | Wall ms, median [range] | CPU ms | Cores | Peak MiB | {heading} '
             '| Drift from previous session |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: |']
    for name in order:
        row = rows[name]
        parity = parity_of(name)
        ratio = f"{parity['median_seconds'] / row['median_seconds']:.2f}×" if parity else 'no bsdtar row'
        drift = f"{row['median_seconds'] / earlier[name] - 1:+.1%}" if name in earlier else 'none recorded'
        lines.append(f"| `{name}` | {ms(row['median_seconds'])} [{ms(row['min_seconds'])}–{ms(row['max_seconds'])}] "
                     f"| {ms(row['median_cpu_seconds'])} | {row['cpu_per_wall']:.2f} "
                     f"| {row['peak_rss_bytes'] / 2**20:.1f} | {ratio} | {drift} |")
    return lines


def counter_table(result):
    counters = result.get('counters')
    if not counters:
        return []
    zip_bytes = result['archives']['input.zip']['bytes']
    lines = [f"Counters from one extraction per engine at one thread, outside timed runs, at `{counters['revision'][:7]}`.", '',
             '| Engine | Decode ms | Archive reads | Archive MiB read | Read ÷ ZIP size | Files | Directory calls '
             '| Links | Output writes | Output MiB | Metadata calls |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for name, row in counters['rows'].items():
        lines.append(f"| `{name}` | {row['decode_nanos'] / 1e6:,.2f} | {row['archive_reads']:,} "
                     f"| {row['archive_bytes_read'] / 2**20:,.1f} | {row['archive_bytes_read'] / zip_bytes:.2f} "
                     f"| {row['files_created']:,} | {row['directory_create_calls']:,} | {row['symlinks_created']:,} "
                     f"| {row['output_writes']:,} | {row['output_bytes_written'] / 2**20:,.1f} | {row['metadata_calls']:,} |")
    return lines + ['']


def render():
    lines = ['# Results', '',
             'This page is generated by `cargo xtask bench` from the JSON in `benchmarks/scoreboard/`',
             'and `benchmarks/realworld/`.',
             'Do not edit it by hand. The gate fails when it differs from that JSON.', '',
             'Every number is a local measurement on the named machine. None is a cross-machine or',
             'state-of-the-art claim. The [experiment log](../benchmarks/README.md) holds earlier records,',
             'such as the Silesia frontier, the dependency sweeps and the Windows runs.', '',
             '## How the scoreboard runs', '',
             '- Each workload is a fixed directory. `scripts/extraction_matrix.py` packs it into one Deflate',
             '  ZIP and one TAR with fixed metadata, then extracts them in interleaved, shuffled rounds.',
             '- Every extraction is compared byte for byte with the source, outside the timed region.',
             '- `native` and `native-fast` rows are Zipper ZIP engines at 1 and 4 threads.',
             '  `native-auto` is the default engine with no `--threads` flag, so Zipper picks the workers.',
             '  `bsdtar-zip` restores default metadata, which matches what the native engines restore.',
             '  `bsdtar-tar` uses reduced-metadata flags. `7zip` restores times and attributes.',
             '- Tar rows extract the TAR and its gzip form. `tar-native` and `tar-gz-native` run the default',
             '  engine. `bsdtar-tar-default` and `bsdtar-tar-gz` restore default metadata, as Zipper does.',
             '- Peak memory is the peak resident size of one process. Caches are warm.',
             '- Drift compares each median with the previous session that measured the same Zipper binary.',
             '  It shows how far one host moves between sessions, so a change smaller than the drift is noise.', '']
    found = {path.stem: json.loads(path.read_text(encoding='utf-8'))
             for path in sorted(SCOREBOARD.glob('*.json'))}
    missing = [name for name in WORKLOADS if name not in found]
    if missing:
        lines += [f"These workloads are not yet measured. They are {', '.join(f'`{name}`' for name in missing)}.", '']
    for name, (_, _, title) in WORKLOADS.items():
        if name not in found:
            continue
        result = found[name]
        zip_bytes = result['archives']['input.zip']['bytes']
        lines += [f'## {title}', '',
                  f"- {count(result['files'], 'input file')} and {result['input_bytes']:,} input bytes.",
                  f"- A ZIP of {zip_bytes:,} bytes.",
                  f"- Revision `{result['revision'][:7]}`, recorded {result['recorded']}.",
                  f"- {count(result['runs'], 'measured round')} after one warmup.",
                  f"- `{result['platform']}` on `{result['machine']}` with {result['cpu_count']} logical CPUs.", '',
                  *table(result), '', *counter_table(result)]
        tar_rows = table(result, tar=True)
        if tar_rows:
            tar_bytes = result['archives']['input.tar']['bytes']
            gz_bytes = result['archives']['input.tar.gz']['bytes']
            lines += [f"Tar rows read a TAR of {tar_bytes:,} bytes and its gzip level 6 form of {gz_bytes:,} bytes.", '',
                      *tar_rows, '']
    lines += codecs()
    lines += realworld()
    return '\n'.join(lines).rstrip('\n') + '\n'


def codecs():
    """A section from `scripts/codec_scoreboard.py`, when its JSON exists."""
    found = [json.loads(path.read_text(encoding='utf-8')) for path in sorted(CODECS.glob('*.json'))]
    if not found:
        return []
    order = {name: index for index, name in enumerate(CODEC_ORDER)}
    found.sort(key=lambda result: order.get(result['codec'], len(order)))
    first = found[0]
    lines = ['## Codecs', '',
             f"Each row extracts one archive of the Silesia corpus, {first['input_bytes']:,} bytes in 12 files, with Zipper's",
             'default engine at one thread and with the named tool at one thread. 7-Zip extracts a compressed tar',
             'by decoding into a second 7-Zip that reads the tar, so its peak memory is the larger of the two.',
             '7-Zip reads no lzip or LZ4, so bsdtar is the reference there, and it reads no Zstandard in ZIP.',
             f"Every extraction is compared byte for byte with the source. Revision `{first['revision'][:7]}`, recorded",
             f"{first['recorded']}, {count(first['runs'], 'measured round')} after one warmup, on `{first['platform']}`.", '',
             '| Codec | Archive MB | Zipper ms, median [range] | Zipper CPU ms | Reference | Reference ms, median [range] '
             '| Reference CPU ms | Reference time ÷ Zipper time |',
             '| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: |']
    for result in found:
        rows = result['results']
        zipper = rows['zipper']
        cpu = lambda row: statistics.median(sample['cpu_seconds'] for sample in row['samples'])
        reference = rows.get(result['reference']) if result['reference'] else None
        if reference:
            theirs = (f"`{result['reference']}` | {ms(reference['median_seconds'])} "
                      f"[{ms(reference['min_seconds'])}–{ms(reference['max_seconds'])}] | {ms(cpu(reference))} "
                      f"| {reference['median_seconds'] / zipper['median_seconds']:.2f}×")
        else:
            theirs = 'none reads it | | |'
        lines.append(f"| `{result['codec']}` | {result['archive_bytes'] / 1e6:,.1f} "
                     f"| {ms(zipper['median_seconds'])} [{ms(zipper['min_seconds'])}–{ms(zipper['max_seconds'])}] "
                     f"| {ms(cpu(zipper))} | {theirs} |")
    return lines + ['']


def realworld():
    """A section from the real-world comparison, when its JSON exists."""
    timings_path, results_path = REALWORLD / 'timings.json', REALWORLD / 'results.json'
    if not timings_path.exists() or not results_path.exists():
        return []
    timings = json.loads(timings_path.read_text(encoding='utf-8'))
    results = json.loads(results_path.read_text(encoding='utf-8'))
    identical = sum(1 for row in results if row.get('differences') == {})
    zipper = sum(row['zipper_seconds'] for row in timings)
    bsdtar = sum(row['bsdtar_seconds'] for row in timings)
    faster = sum(row['zipper_seconds'] < row['bsdtar_seconds'] for row in timings)
    lines = ['## Real-world archives', '',
             f"The [real-world comparison](../benchmarks/realworld/README.md) extracts {len(timings)} popular archives "
             'from package registries, GitHub and release pages with the default engine and with default bsdtar.',
             f'{identical} of {len(results)} extract identically. Zipper is faster on {faster} of {len(timings)}, '
             f'taking {zipper:.2f} s in total against {bsdtar:.2f} s.',
             'Each time is the median of three rounds per tool on macOS. Zipper uses its default threads, and bsdtar one.', '',
             '| Source | Archives | Zipper s | bsdtar s | bsdtar time ÷ Zipper time |',
             '| --- | ---: | ---: | ---: | ---: |']
    for key, title in REALWORLD_SOURCES.items():
        rows = [row for row in timings if row['name'].split(':')[0] == key]
        if rows:
            mine = sum(row['zipper_seconds'] for row in rows)
            theirs = sum(row['bsdtar_seconds'] for row in rows)
            lines.append(f'| {title} | {len(rows)} | {mine:.2f} | {theirs:.2f} | {theirs / mine:.2f}× |')
    return lines + ['']


def check(_args):
    for path in sorted([*SCOREBOARD.glob('*.json'), *CODECS.glob('*.json')]):
        result = json.loads(path.read_text(encoding='utf-8'))
        if not result['tree_clean'] or not (result.get('counters') or {}).get('tree_clean', True):
            sys.exit(f'{path.name} was measured on a dirty tree, so its revision cannot reproduce it')
    if RESULTS.read_text(encoding='utf-8') != render():
        sys.exit('docs/results.md differs from benchmarks/scoreboard; run `python3 scripts/scoreboard.py render`')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    measure = commands.add_parser('run', help='measure workloads and rewrite the JSON and docs/results.md')
    measure.add_argument('--runs', type=int, default=7)
    measure.add_argument('--only', nargs='+', choices=list(WORKLOADS))
    measure.add_argument('--allow-dirty', action='store_true', help='for trials; the JSON records tree_clean false')
    measure.add_argument('--allow-busy', action='store_true',
                         help='measure while other processes load the CPU; the JSON lists them')
    measure.add_argument('--scoreboard-dir', type=Path, help='write the JSON here instead of benchmarks/scoreboard')
    measure.add_argument('--no-render', action='store_true', help='leave docs/results.md alone, as on other platforms')
    measure.add_argument('--counters-only', action='store_true',
                         help='keep the timings and recount each recorded ZIP with the counters build')
    commands.add_parser('render', help='rewrite docs/results.md from the JSON')
    commands.add_parser('check', help='fail when docs/results.md differs from the JSON')
    args = parser.parse_args()
    if args.command == 'run':
        run(args)
    elif args.command == 'render':
        RESULTS.write_text(render(), encoding='utf-8')
    else:
        check(args)


if __name__ == '__main__':
    main()
