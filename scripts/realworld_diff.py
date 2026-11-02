#!/usr/bin/env python3
"""Compare Zipper with bsdtar on popular real-world archives.

`resolve` picks archives from package registries and GitHub and pins each URL
with its SHA-256 in a manifest. `fetch` downloads them into a cache and checks
the hashes. `compare` extracts every archive with Zipper's default engine and
with default bsdtar, which applies the umask as Zipper does, then compares paths, file bytes, symlink targets, permission bits
and file modification times. Archives are untrusted data: they stay in the
cache and the work folders and are never executed. Run under `python3 -I`."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import urllib.request
import xml.etree.ElementTree

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / 'target' / 'realworld' / 'archives'
WORK = ROOT / 'target' / 'realworld' / 'work'
AGENT = 'zipper-realworld (https://github.com/marlboro-red/zipper)'
MAX_BYTES = 150 * 1024 * 1024

NPM = ['lodash', 'react', 'express', 'typescript', 'axios', 'chalk', 'commander', 'debug', 'tslib', 'semver',
       'uuid', 'moment', 'webpack', 'eslint', 'prettier', 'next', 'vue', 'rxjs', 'jquery', 'yargs',
       'esbuild', 'rollup', 'vite', '@babel/core', 'core-js', 'zod', 'dotenv', 'glob', 'minimatch', 'ws',
       'node-fetch', 'bluebird', 'underscore', 'classnames', 'prop-types', 'date-fns', 'three', 'socket.io',
       'mongoose', 'sharp']
MAVEN = ['com.google.guava:guava', 'org.apache.commons:commons-lang3', 'com.fasterxml.jackson.core:jackson-databind',
         'org.slf4j:slf4j-api', 'junit:junit', 'org.junit.jupiter:junit-jupiter-api', 'com.google.code.gson:gson',
         'org.apache.httpcomponents:httpclient', 'commons-io:commons-io', 'org.mockito:mockito-core',
         'org.jetbrains.kotlin:kotlin-stdlib', 'org.springframework:spring-core', 'io.netty:netty-common',
         'org.yaml:snakeyaml', 'ch.qos.logback:logback-classic']
GO = ['golang.org/x/text', 'golang.org/x/net', 'golang.org/x/sys', 'golang.org/x/crypto', 'github.com/spf13/cobra',
      'github.com/stretchr/testify', 'github.com/gin-gonic/gin', 'google.golang.org/grpc', 'github.com/google/uuid',
      'github.com/sirupsen/logrus', 'gopkg.in/yaml.v3', 'github.com/pkg/errors', 'golang.org/x/tools',
      'github.com/prometheus/client_golang', 'github.com/gorilla/mux']
RELEASES = [
    ('python-source', 'https://www.python.org/ftp/python/3.13.1/Python-3.13.1.tar.xz'),
    ('node-binary', 'https://nodejs.org/dist/v22.11.0/node-v22.11.0-linux-x64.tar.xz'),
    ('coreutils', 'https://ftp.gnu.org/gnu/coreutils/coreutils-9.5.tar.xz'),
    ('tar-source', 'https://ftp.gnu.org/gnu/tar/tar-1.35.tar.xz'),
    ('zstd-source', 'https://github.com/facebook/zstd/releases/download/v1.5.6/zstd-1.5.6.tar.zst'),
    ('go-binary', 'https://go.dev/dl/go1.23.3.linux-amd64.tar.gz'),
    ('cmake-source', 'https://github.com/Kitware/CMake/releases/download/v3.31.0/cmake-3.31.0.tar.gz'),
    ('curl-source', 'https://curl.se/download/curl-8.11.0.tar.xz'),
    ('git-source', 'https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.47.0.tar.xz'),
    ('openssl-source', 'https://github.com/openssl/openssl/releases/download/openssl-3.4.0/openssl-3.4.0.tar.gz'),
]


def get_json(url):
    request = urllib.request.Request(url, headers={'User-Agent': AGENT, 'Accept': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def get_text(url):
    request = urllib.request.Request(url, headers={'User-Agent': AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read().decode()


def pypi_sources(count):
    top = get_json('https://hugovk.github.io/top-pypi-packages/top-pypi-packages.min.json')['rows'][:count]
    found = []
    for row in top:
        name = row['project']
        files = get_json(f'https://pypi.org/pypi/{name}/json')['urls']
        sdist = next((f for f in files if f['packagetype'] == 'sdist' and f['filename'].endswith('.tar.gz')), None)
        wheels = [f for f in files if f['packagetype'] == 'bdist_wheel']
        wheel = next((f for f in wheels if f['filename'].endswith('none-any.whl')), wheels[0] if wheels else None)
        for kind, chosen in [('pypi-sdist', sdist), ('pypi-wheel', wheel)]:
            if chosen:
                found.append({'name': f'{kind}:{chosen["filename"]}', 'url': chosen['url'],
                              'sha256': chosen['digests']['sha256']})
    return found


def npm_sources():
    found = []
    for name in NPM:
        latest = get_json(f'https://registry.npmjs.org/{name}/latest')
        found.append({'name': f'npm:{name}@{latest["version"]}', 'url': latest['dist']['tarball']})
    return found


def crate_sources(count):
    crates = get_json(f'https://crates.io/api/v1/crates?sort=downloads&per_page={count}')['crates']
    return [{'name': f'crate:{c["id"]}-{c["max_stable_version"]}',
             'url': f'https://static.crates.io/crates/{c["id"]}/{c["id"]}-{c["max_stable_version"]}.crate'}
            for c in crates if c.get('max_stable_version')]


def maven_sources():
    found = []
    for coordinate in MAVEN:
        group, artifact = coordinate.split(':')
        base = f'https://repo1.maven.org/maven2/{group.replace(".", "/")}/{artifact}'
        metadata = xml.etree.ElementTree.fromstring(get_text(f'{base}/maven-metadata.xml'))
        version = metadata.findtext('versioning/release')
        found.append({'name': f'maven:{artifact}-{version}.jar', 'url': f'{base}/{version}/{artifact}-{version}.jar'})
    return found


def go_sources():
    found = []
    for module in GO:
        version = get_json(f'https://proxy.golang.org/{module}/@latest')['Version']
        found.append({'name': f'go:{module}@{version}', 'url': f'https://proxy.golang.org/{module}/@v/{version}.zip'})
    return found


def github_sources(count):
    listing = json.loads(subprocess.check_output(
        ['gh', 'api', f'search/repositories?q=stars:>30000&sort=stars&per_page={count}']))
    found = []
    for repo in listing['items']:
        if repo['size'] > 100_000:
            continue
        name, branch = repo['full_name'], repo['default_branch']
        head = json.loads(subprocess.check_output(['gh', 'api', f'repos/{name}/commits/{branch}']))['sha']
        for form in ['tar.gz', 'zip']:
            found.append({'name': f'github:{name}@{head[:12]}.{form}',
                          'url': f'https://codeload.github.com/{name}/{form.replace(".", "")}/{head}'
                          if form == 'zip' else f'https://codeload.github.com/{name}/tar.gz/{head}'})
    return found


def resolve(args):
    sources = []
    for label, collect in [('PyPI', lambda: pypi_sources(40)), ('npm', npm_sources),
                           ('crates.io', lambda: crate_sources(40)), ('Maven', maven_sources), ('Go', go_sources),
                           ('GitHub', lambda: github_sources(40)),
                           ('releases', lambda: [{'name': f'release:{n}', 'url': u} for n, u in RELEASES])]:
        print(f'resolve: {label}', file=sys.stderr, flush=True)
        try:
            sources += collect()
        except Exception as error:
            print(f'  skipped {label}: {error!r}', file=sys.stderr)
    for source in sources:
        path = download(source)
        if path is None:
            continue
        source['sha256'] = sha256(path)
        source['bytes'] = path.stat().st_size
    kept = [source for source in sources if 'bytes' in source]
    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    args.manifest.write_text(json.dumps(kept, indent=1) + '\n', encoding='utf-8')
    print(f'{len(kept)} of {len(sources)} archives pinned', file=sys.stderr)


def sha256(path):
    hasher = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            hasher.update(block)
    return hasher.hexdigest()


def cached(source):
    digest = hashlib.sha256(source['url'].encode()).hexdigest()[:16]
    suffix = ''.join(Path(source['url'].split('?')[0]).suffixes[-2:]) or '.bin'
    return CACHE / f'{digest}{suffix}'


def download(source):
    path = cached(source)
    if path.exists():
        return path
    CACHE.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(source['url'], headers={'User-Agent': AGENT})
    partial = path.with_suffix(path.suffix + '.part')
    try:
        with urllib.request.urlopen(request, timeout=300) as response, partial.open('wb') as out:
            copied = 0
            for block in iter(lambda: response.read(1 << 20), b''):
                copied += len(block)
                if copied > MAX_BYTES:
                    raise ValueError('larger than 150 MiB')
                out.write(block)
    except Exception as error:
        partial.unlink(missing_ok=True)
        print(f'  skipped {source["name"]}: {error!r}', file=sys.stderr)
        return None
    partial.rename(path)
    return path


def fetch(args):
    for source in json.loads(args.manifest.read_text(encoding='utf-8')):
        path = download(source)
        if path is None or sha256(path) != source['sha256']:
            sys.exit(f'{source["name"]} is missing or no longer matches its pinned hash')


def tree(root):
    """Map of NFC path to (kind, content hash or link target, mode, mtime)."""
    found = {}
    for directory, folders, files in os.walk(root):
        for name in folders + files:
            path = Path(directory) / name
            key = unicodedata.normalize('NFC', path.relative_to(root).as_posix())
            info = path.lstat()
            if path.is_symlink():
                found[key] = ('link', os.readlink(path), None, None)
            elif path.is_dir():
                found[key] = ('dir', None, info.st_mode & 0o777, None)
            else:
                found[key] = ('file', sha256(path), info.st_mode & 0o777, int(info.st_mtime))
    return found


def unlock(root):
    for directory, folders, _ in os.walk(root):
        for name in folders:
            path = Path(directory) / name
            if not path.is_symlink():
                path.chmod(path.stat().st_mode | 0o700)


def differences(zipper, reference):
    found = {}
    for path in sorted(set(zipper) | set(reference)):
        mine, theirs = zipper.get(path), reference.get(path)
        if mine is None or theirs is None:
            kind = 'only bsdtar' if mine is None else 'only zipper'
        elif mine[0] != theirs[0] or mine[1] != theirs[1]:
            kind = 'content'
        elif mine[2] != theirs[2]:
            kind = 'mode'
        elif mine[3] != theirs[3]:
            kind = 'mtime'
        else:
            continue
        found.setdefault(kind, []).append(path)
    return found


def extract_bsdtar(archive, out, bsdtar):
    out.mkdir()
    if archive.name.endswith('.zst'):
        unpack = subprocess.Popen(['zstd', '-dc', str(archive)], stdout=subprocess.PIPE)
        result = subprocess.run([bsdtar, '-xf', '-', '-C', str(out)], stdin=unpack.stdout, capture_output=True)
        unpack.stdout.close()
        unpack.wait()
    else:
        result = subprocess.run([bsdtar, '-xf', str(archive), '-C', str(out)], capture_output=True)
    return result.returncode, result.stderr.decode(errors='replace').strip()[-300:]


def compare(args):
    sources = json.loads(args.manifest.read_text(encoding='utf-8'))
    WORK.mkdir(parents=True, exist_ok=True)
    results = []
    for index, source in enumerate(sources):
        archive = cached(source)
        print(f'[{index + 1}/{len(sources)}] {source["name"]}', file=sys.stderr, flush=True)
        with tempfile.TemporaryDirectory(dir=WORK) as work:
            work = Path(work)
            zipper = subprocess.run([str(args.zipper), 'extract', str(archive), '-o', str(work / 'zipper')],
                                    capture_output=True)
            code, bsdtar_error = extract_bsdtar(archive, work / 'bsdtar', args.bsdtar)
            row = {'name': source['name'], 'zipper_exit': zipper.returncode, 'bsdtar_exit': code}
            if zipper.returncode:
                row['zipper_error'] = zipper.stderr.decode(errors='replace').strip()[-300:]
            if code:
                row['bsdtar_error'] = bsdtar_error
            if zipper.returncode == 0 and code == 0:
                diff = differences(tree(work / 'zipper'), tree(work / 'bsdtar'))
                row['differences'] = {kind: {'count': len(paths), 'examples': paths[:5]} for kind, paths in diff.items()}
                row['files'] = sum(1 for _ in (work / 'zipper').rglob('*'))
            for folder in ['zipper', 'bsdtar']:
                if (work / folder).exists():
                    unlock(work / folder)
            results.append(row)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(results, indent=1) + '\n', encoding='utf-8')
    same = sum(1 for row in results if row.get('differences') == {})
    print(f'{same} of {len(results)} archives extract identically', file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ['resolve', 'fetch', 'compare']:
        command = commands.add_parser(name)
        command.add_argument('--manifest', type=Path, default=ROOT / 'benchmarks' / 'realworld' / 'sources.json')
        if name == 'compare':
            command.add_argument('--zipper', type=Path, default=ROOT / 'target' / 'release' / 'zipper')
            command.add_argument('--bsdtar', default=shutil.which('bsdtar') or 'bsdtar')
            command.add_argument('--output', type=Path, default=ROOT / 'benchmarks' / 'realworld' / 'results.json')
    args = parser.parse_args()
    {'resolve': resolve, 'fetch': fetch, 'compare': compare}[args.command](args)


if __name__ == '__main__':
    main()
