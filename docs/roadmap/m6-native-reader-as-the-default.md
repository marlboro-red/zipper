# M6. Native reader as the default

`zipper extract` uses the in-process reader for ZIP without any flag.

Scope:

- Make the native reader the default `--zip-engine` and choose between `native` and `native-fast` from M3 data. Done. The default `auto` engine uses `native`, which peaks under 9 MiB on every scoreboard workload.
- Keep 7-Zip for formats other than ZIP.
- Update the Windows package and the docs.

Exit checks:

1. Default ZIP extraction runs in-process on all three operating systems in CI.
2. The M1 and M2 checks pass on all three operating systems.
3. `docs/usage.md` and `docs/reference/zip-reader.md` describe the new default.

[M25](m25-a-release-build-without-c.md) retired `native-fast`, and [M20](m20-zipper-without-7-zip.md) removed `--zip-engine` with the 7-Zip backend on 13 November 2026. Every format now goes to Zipper's own reader, with no engine to choose.
