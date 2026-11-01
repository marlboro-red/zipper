#!/usr/bin/env python3
"""Run the fixed extraction scoreboard and render docs/results.md from its JSON."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys

from extraction_matrix import write_zip

ROOT = Path(__file__).resolve().parent.parent
SCOREBOARD = ROOT / 'benchmarks' / 'scoreboard'
RESULTS = ROOT / 'docs' / 'results.md'
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

ROW_ORDER = ['native-1', 'native-4', 'native-fast-1', 'native-fast-4', 'bsdtar-zip', '7zip', 'bsdtar-tar']
PARITY_ROW = 'bsdtar-zip'


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
    zipper = ROOT / 'target' / 'release' / ('zipper.exe' if platform.system() == 'Windows' else 'zipper')
    names = args.only or list(WORKLOADS)
    if args.counters_only:
        for name in names:
            path = SCOREBOARD / f'{name}.json'
            result = json.loads(path.read_text(encoding='utf-8'))
            result['counters'] = counted(name, result)
            path.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        RESULTS.write_text(render(), encoding='utf-8')
        return
    WORK.mkdir(parents=True, exist_ok=True)
    SCOREBOARD.mkdir(parents=True, exist_ok=True)
    for name in names:
        corpus, subpath, _ = WORKLOADS[name]
        source = (prepared(corpus) / subpath).resolve()
        raw = WORK / f'{name}.json'
        command = [sys.executable, str(ROOT / 'scripts' / 'extraction_matrix.py'), str(source),
                   '--zipper', str(zipper), '--output', str(raw), '--work-dir', str(WORK),
                   '--threads', *THREADS, '--runs', str(args.runs), '--save-zip', str(WORK / f'{name}.zip')]
        for flag, tool in [('--tar', 'bsdtar'), ('--sevenzip', '7zz')]:
            if shutil.which(tool):
                command += [flag, tool]
        print(f'scoreboard: {name}', file=sys.stderr, flush=True)
        subprocess.run(command, cwd=ROOT, check=True)
        result = json.loads(raw.read_text(encoding='utf-8'))
        digest_manifest(result)
        result['workload'] = name
        result['revision'] = git('rev-parse', 'HEAD')
        result['tree_clean'] = not dirty
        result['recorded'] = datetime.date.today().isoformat()
        result['previous_session'] = previous_session(SCOREBOARD / f'{name}.json', result)
        result['counters'] = counted(name, result)
        (SCOREBOARD / f'{name}.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    RESULTS.write_text(render(), encoding='utf-8')


def count(number, noun):
    return f"{number:,} {noun}{'' if number == 1 else 's'}"


def ms(seconds):
    return f'{seconds * 1000:,.2f}'


def table(result):
    rows = result['results']
    order = [name for name in ROW_ORDER if name in rows] + sorted(set(rows) - set(ROW_ORDER))
    parity = rows.get(PARITY_ROW)
    earlier = (result.get('previous_session') or {}).get('median_seconds', {})
    lines = ['| Reader | Wall ms, median [range] | CPU ms | Cores | Peak MiB | bsdtar-zip time ÷ row time '
             '| Drift from previous session |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: |']
    for name in order:
        row = rows[name]
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
             'This page is generated by `cargo xtask bench` from the JSON in `benchmarks/scoreboard/`.',
             'Do not edit it by hand. The gate fails when it differs from that JSON.', '',
             'Every number is a local measurement on the named machine. None is a cross-machine or',
             'state-of-the-art claim. The [experiment log](../benchmarks/README.md) holds earlier records,',
             'such as the Silesia frontier, the dependency sweeps and the Windows runs.', '',
             '## How the scoreboard runs', '',
             '- Each workload is a fixed directory. `scripts/extraction_matrix.py` packs it into one Deflate',
             '  ZIP and one TAR with fixed metadata, then extracts them in interleaved, shuffled rounds.',
             '- Every extraction is compared byte for byte with the source, outside the timed region.',
             '- `native` and `native-fast` rows are Zipper ZIP engines at 1 and 4 threads.',
             '  `bsdtar-zip` restores default metadata, which matches what the native engines restore.',
             '  `bsdtar-tar` uses reduced-metadata flags. `7zip` restores times and attributes.',
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
    return '\n'.join(lines).rstrip('\n') + '\n'


def check(_args):
    if RESULTS.read_text(encoding='utf-8') != render():
        sys.exit('docs/results.md differs from benchmarks/scoreboard; run `python3 scripts/scoreboard.py render`')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    measure = commands.add_parser('run', help='measure workloads and rewrite the JSON and docs/results.md')
    measure.add_argument('--runs', type=int, default=7)
    measure.add_argument('--only', nargs='+', choices=list(WORKLOADS))
    measure.add_argument('--allow-dirty', action='store_true', help='for trials; the JSON records tree_clean false')
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
