#!/usr/bin/env python3
"""Compare two Zipper binaries on the scoreboard workloads, interleaved.

For each workload and archive format, write the scoreboard's fixture once and
run scripts/compare_zip_readers.py with both binaries in shuffled rounds,
extracting under target/bench-work as the scoreboard does. Extracting into the
Windows temp folder ran small-file rows about five times slower than the
scoreboard. Both binaries run on the same host in the same session, so their
ratio holds even where absolute times drift between sessions."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

from extraction_matrix import write_gzip, write_tar, write_zip
from scoreboard import ROOT, WORKLOADS, prepared

FORMATS = ['zip', 'tar', 'tar.gz']


def fixture(name, source, work, form):
    archive = work / f'{name}.{form}'
    if archive.exists():
        return archive
    plain = work / f'{name}.tar'
    if form == 'zip':
        write_zip(source, archive)
    elif form == 'tar':
        write_tar(source, archive)
    else:
        if not plain.exists():
            write_tar(source, plain)
        write_gzip(plain, archive)
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path, required=True)
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--workloads', nargs='+', choices=list(WORKLOADS), default=list(WORKLOADS))
    parser.add_argument('--formats', nargs='+', choices=FORMATS, default=FORMATS)
    parser.add_argument('--runs', type=int, default=11)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    work = ROOT / 'target' / 'bench-work' / 'compare'
    work.mkdir(parents=True, exist_ok=True)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    summary = {}
    for name in args.workloads:
        corpus, subpath, _ = WORKLOADS[name]
        source = (prepared(corpus) / subpath).resolve()
        for form in args.formats:
            archive = fixture(name, source, work, form)
            report = args.output_dir / f'{name}.{form}.json'
            print(f'compare: {name} {form}', file=sys.stderr, flush=True)
            with report.open('w', encoding='utf-8') as out:
                subprocess.run([sys.executable, str(ROOT / 'scripts' / 'compare_zip_readers.py'),
                                str(archive), str(source),
                                '--archive-format', 'zip' if form == 'zip' else 'tar',
                                '--runs', str(args.runs), '--work-dir', str(work),
                                '--reader', 'before', 'native', str(args.before.resolve()),
                                '--reader', 'after', 'native', str(args.after.resolve()),
                                '--reader-threads', 'before', 'auto', '--reader-threads', 'after', 'auto'],
                               cwd=ROOT, check=True, stdout=out)
            rows = json.loads(report.read_text(encoding='utf-8'))['results']
            summary[f'{name}.{form}'] = {label: {key: row[key] for key in ('median_seconds', 'min_seconds', 'max_seconds')}
                                         for label, row in rows.items()}
            before, after = (rows[label]['median_seconds'] for label in ('before', 'after'))
            print(f'  before {before * 1000:.1f} ms, after {after * 1000:.1f} ms, '
                  f'{before / after:.2f}x', file=sys.stderr, flush=True)
    (args.output_dir / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
