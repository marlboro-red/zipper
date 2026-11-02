# Real-world archives against bsdtar

**All 248 popular archives extracted identically with Zipper's default engine and default bsdtar.** That covers 189,576 paths, with the same bytes, symlink targets, permission bits and file modification times. Zipper rejected none. The sources are registries and GitHub on one day, compared on macOS only, so the result does not cover Windows-made ZIPs or archives that only old tools write.

## Question

The reference corpora in `tests/data` hold archives that Zipper's own scripts wrote from small synthetic trees. Do archives that people download every day extract the same way with Zipper as with bsdtar? Or does Zipper reject them?

## Setup

- Apple M5, macOS 26.7.1, umask `022`. Zipper at `56d6f74`, release build, default engine with no flags. bsdtar 3.5.3 with libarchive 3.7.4, run as `bsdtar -xf`, which applies the umask as Zipper does. A `.tar.zst` is piped through `zstd -dc`, because this bsdtar lacks zstd.
- `scripts/realworld_diff.py resolve` picked the archives on 2026-11-03 and pinned each URL with its SHA-256 in `sources.json`:
  - The 40 most-downloaded PyPI projects, one source archive and one wheel each.
  - 40 well-known npm packages, the 40 most-downloaded crates, 15 Maven jars and 15 Go module ZIPs.
  - 24 of the 25 most-starred GitHub repositories under 100 MB, as `.tar.gz` and `.zip` source archives of their default branch. The manifest leaves out one repository of software-licensing workarounds.
  - 10 release tarballs of Python, Node.js, Go, GNU coreutils and tar, curl, git, CMake, OpenSSL and zstd.
- By format, that is 67 `.tar.gz`, 40 `.tgz`, 40 `.crate`, 6 `.tar.xz` and 1 `.tar.zst`, plus 39 `.zip`, 40 `.whl` and 15 `.jar`. Together they are 600,929,086 bytes.
- `compare` extracted each archive with both tools into fresh folders and compared every path under Unicode NFC. It checked the kind, the file SHA-256 or link target, the permission bits and the file modification time in whole seconds.

### Archives

`sources.json` holds the download address and hash of each archive.

| Source | Count | Archives |
| --- | ---: | --- |
| PyPI source packages | 40 | `boto3-1.43.123`, `packaging-26.3`, `typing_extensions-4.16.0`, `certifi-2026.10.23`, `idna-3.20`, `urllib3-2.8.0`, `charset_normalizer-3.5.2`, `requests-2.34.2`, `cryptography-50.0.2`, `cffi-2.1.1`, `pluggy-1.7.0`, `pyyaml-6.0.3`, `pycparser-3.12`, `pygments-2.21.0`, `python_dateutil-4.0.0`, `six-1.17.0`, `setuptools-84.2.1`, `pydantic-2.14.2`, `pydantic_core-2.50.2`, `click-8.5.0`, `numpy-2.5.4`, `botocore-1.43.123`, `anyio-4.16.0`, `annotated_types-0.8.0`, `pytest-9.2.0`, `iniconfig-2.3.1`, `typing_inspection-0.4.4`, `h11-0.16.0`, `attrs-26.1.0`, `markupsafe-3.0.4`, `httpx-0.28.1`, `httpcore-1.0.9`, `s3transfer-0.19.3`, `protobuf-7.36.2`, `python_dotenv-1.2.4`, `pyjwt-2.15.1`, `jinja2-3.1.6`, `pandas-3.1.0`, `aiobotocore-3.10.0`, `starlette-1.10.0` |
| PyPI wheels | 40 | `boto3 1.43.123`, `packaging 26.3`, `typing_extensions 4.16.0`, `certifi 2026.10.23`, `idna 3.20`, `urllib3 2.8.0`, `charset_normalizer 3.5.2`, `requests 2.34.2`, `cryptography 50.0.2`, `cffi 2.1.1`, `pluggy 1.7.0`, `pyyaml 6.0.3`, `pycparser 3.12`, `pygments 2.21.0`, `python_dateutil 4.0.0`, `six 1.17.0`, `setuptools 84.2.1`, `pydantic 2.14.2`, `pydantic_core 2.50.2`, `click 8.5.0`, `numpy 2.5.4`, `botocore 1.43.123`, `anyio 4.16.0`, `annotated_types 0.8.0`, `pytest 9.2.0`, `iniconfig 2.3.1`, `typing_inspection 0.4.4`, `h11 0.16.0`, `attrs 26.1.0`, `markupsafe 3.0.4`, `httpx 0.28.1`, `httpcore 1.0.9`, `s3transfer 0.19.3`, `protobuf 7.36.2`, `python_dotenv 1.2.4`, `pyjwt 2.15.1`, `jinja2 3.1.6`, `pandas 3.1.0`, `aiobotocore 3.10.0`, `starlette 1.10.0` |
| npm packages | 40 | `lodash 4.18.1`, `react 19.3.1`, `express 5.3.0`, `typescript 7.0.2`, `axios 1.21.0`, `chalk 6.0.1`, `commander 15.0.0`, `debug 4.4.3`, `tslib 2.9.1`, `semver 7.8.5`, `uuid 14.0.3`, `moment 2.31.0`, `webpack 5.112.2`, `eslint 10.14.0`, `prettier 3.9.10`, `next 16.4.4`, `vue 3.5.44`, `rxjs 7.8.2`, `jquery 4.0.0`, `yargs 18.2.0`, `esbuild 0.28.2`, `rollup 4.67.4`, `vite 8.3.5`, `@babel/core 8.1.0`, `core-js 3.51.0`, `zod 4.6.7`, `dotenv 18.0.9`, `glob 14.0.3`, `minimatch 11.0.4`, `ws 8.22.1`, `node-fetch 3.3.2`, `bluebird 3.7.2`, `underscore 1.13.8`, `classnames 2.5.1`, `prop-types 15.8.1`, `date-fns 4.4.0`, `three 0.187.0`, `socket.io 4.8.4`, `mongoose 9.11.4`, `sharp 0.35.5` |
| Crates | 40 | `hashbrown-0.17.1`, `syn-3.0.9`, `getrandom-0.4.3`, `bitflags-2.13.2`, `rand_core-0.10.1`, `rand-0.10.3`, `libc-0.2.191`, `quote-1.0.47`, `proc-macro2-1.0.107`, `base64-0.23.1`, `windows-sys-0.61.2`, `indexmap-2.14.2`, `itertools-0.15.0`, `thiserror-impl-2.0.21`, `thiserror-2.0.21`, `cfg-if-1.0.5`, `memchr-2.8.3`, `regex-syntax-0.8.11`, `serde-1.0.229`, `unicode-ident-1.0.27`, `rand_chacha-0.10.0`, `itoa-1.0.18`, `serde_derive-1.0.229`, `serde_json-1.0.154`, `heck-0.5.0`, `autocfg-1.5.1`, `once_cell-1.21.4`, `log-0.4.35`, `cc-1.11.0`, `regex-automata-0.4.18`, `socket2-0.6.5`, `smallvec-1.16.4`, `rustix-1.1.6`, `regex-1.13.1`, `aho-corasick-1.1.5`, `digest-0.11.3`, `clap-4.6.7`, `ryu-1.0.23`, `block-buffer-0.12.1`, `linux-raw-sys-0.12.1` |
| Maven jars | 15 | `guava-33.7.2-jre`, `commons-lang3-3.21.0`, `jackson-databind-2.22.4`, `slf4j-api-2.1.0-alpha1`, `junit-4.13.2`, `junit-jupiter-api-6.1.3`, `gson-2.15.0`, `httpclient-4.5.14`, `commons-io-2.22.0`, `mockito-core-5.24.0`, `kotlin-stdlib-2.5.0-Beta2`, `spring-core-7.1.0-RC1`, `netty-common-5.0.0.Alpha2`, `snakeyaml-2.7`, `logback-classic-1.6.6` |
| Go modules | 15 | `golang.org/x/text v0.43.0`, `golang.org/x/net v0.62.0`, `golang.org/x/sys v0.49.0`, `golang.org/x/crypto v0.58.0`, `github.com/spf13/cobra v1.10.2`, `github.com/stretchr/testify v1.12.1`, `github.com/gin-gonic/gin v1.12.0`, `google.golang.org/grpc v1.85.0`, `github.com/google/uuid v1.6.0`, `github.com/sirupsen/logrus v1.10.2`, `gopkg.in/yaml.v3 v3.0.1`, `github.com/pkg/errors v0.9.1`, `golang.org/x/tools v0.52.0`, `github.com/prometheus/client_golang v1.25.0`, `github.com/gorilla/mux v1.8.1` |
| GitHub source archives, each as `.tar.gz` and `.zip` | 24 | `codecrafters-io/build-your-own-x`, `sindresorhus/awesome`, `public-apis/public-apis`, `EbookFoundation/free-programming-books`, `donnemartin/system-design-primer`, `jwasham/coding-interview-university`, `awesome-selfhosted/awesome-selfhosted`, `vinta/awesome-python`, `obra/superpowers`, `mattpocock/skills`, `affaan-m/ECC`, `practical-tutorials/project-based-learning`, `trimstray/the-book-of-secret-knowledge`, `multica-ai/andrej-karpathy-skills`, `TheAlgorithms/Python`, `vuejs/vue`, `ossu/computer-science`, `DigitalPlatDev/FreeDomain`, `yt-dlp/yt-dlp`, `trekhleb/javascript-algorithms`, `ultraworkers/claw-code`, `microsoft/markitdown`, `avelino/awesome-go`, `ohmyzsh/ohmyzsh` |
| Release tarballs | 10 | Python 3.13.1 source, Node.js 22.11.0 for Linux x64, GNU coreutils 9.5, GNU tar 1.35, zstd 1.5.6 source (`.tar.zst`), Go 1.23.3 for Linux x64, CMake 3.31.0 source, curl 8.11.0 source, git 2.47.0 source, OpenSSL 3.4.0 source |

## Results

| Outcome | Archives |
| --- | ---: |
| Identical trees | 248 |
| Different bytes, paths or links | 0 |
| Different modes or file times | 0 |
| Rejected by Zipper | 0 |
| Rejected by bsdtar | 0 |

The largest archive was the CMake source tarball, with 28,611 paths. `results.json` holds the outcome and path count per archive.

The first run used `bsdtar -xpf`, which restores modes exactly and ignores the umask. That run covered 250 archives, including the two the manifest leaves out, and differed on modes in 108 of them, such as `775` for directories in GitHub archives. Zipper applies the umask, as default bsdtar does. Against `bsdtar -xf`, all four archives checked by hand matched on every path, and then the full run matched.

To check that the comparison can fail, a wrapper changed one file's bytes and another file's mode after Zipper extracted. On three archives, the comparison reported exactly one content difference and one mode difference each.

## Decision

No code change. The result adds a real-world check to the synthetic corpora. Rerun it after any change to name decoding, metadata restore or entry types with `fetch` and `compare`. The pinned hashes make sure the bytes are the same.

## Limits

One day's registry contents, on macOS only. bsdtar is the only reference here, and earlier records show it differs from GNU tar, Python and `unzip` in places. Directory times were not compared. Windows-made ZIPs, macOS Archive Utility ZIPs, self-extracting archives and archives over 150 MiB are absent. The archives themselves are not committed. The URLs and hashes are.

## Reproduce

```sh
python3 -I scripts/realworld_diff.py fetch
python3 -I scripts/realworld_diff.py compare
```

`resolve` picks a new set from the registries and rewrites `sources.json`.
