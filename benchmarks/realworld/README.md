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
