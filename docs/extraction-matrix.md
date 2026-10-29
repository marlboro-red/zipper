# Portable extraction evidence

`scripts/extraction_matrix.py` compares Zipper's native and native-fast ZIP
readers at explicit thread counts with direct 7-Zip and libarchive bsdtar. Every
ZIP reader receives the identical archive. An uncompressed TAR contains the same
source tree. ZIP/TAR metadata and entry order are normalized; archive hashes,
binary hashes, commands, corpus manifest, run order and all samples are retained.

```sh
python3 scripts/make_tiny_corpus.py /tmp/flat --files 10000
python3 scripts/extraction_matrix.py /tmp/flat \
  --zipper target/release/zipper --output results/flat.json \
  --runs 7 --threads 1 2 4 8 --sevenzip 7zz --tar /usr/bin/tar
```

Use `--layout grouped` or `--layout deep` when generating a separate tiny corpus.
Use `--work-dir` to select the output filesystem. The input source can also be a
real regular-file tree. Symlinks are rejected. The TAR baseline uses USTAR, so
paths outside that format's limits fail instead of silently changing formats.

Each reader runs once for warmup, then the requested measured rounds. A seeded
shuffle interleaves readers. Wall time includes process startup and extraction;
source generation, archive creation, direct byte verification and cleanup are
outside timing. Verification checks the complete output root, directory/file
membership, sizes and every byte. Any mismatch aborts the report.

This is a bytes-and-directories comparison, not metadata-fidelity parity. Zipper
stages and renames the output; external readers write directly. Their required
empty output root is created outside timing, slightly favoring them. The bsdtar
baseline uses 1 MiB records and disables supported metadata restoration options.
7-Zip uses its default extraction parallelism. No reader is asked to fsync; this
measures completed extraction, not crash-durable restoration. Cache state is
uncontrolled and warmed by archive creation and earlier runs.

Memory is the individual process high-water mark, not a process-tree sum:
`wait4.ru_maxrss` on macOS, GNU time child peak RSS on Linux, and `GetProcessMemoryInfo.PeakWorkingSetSize` on Windows.
The Windows query uses the retained child handle after waiting, avoiding sampled
RSS missing short peaks. Memory-query failures abort rather than report zero.
The Linux GNU time wrapper avoids an inherited Python-parent high-water mark
observed with direct wait4 in the first hosted run; wall timing includes the
wrapper overhead. Earlier Linux reports must not be used for memory comparisons.
These OS metrics are not interchangeable measures of committed/private memory.
[Microsoft API](https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getprocessmemoryinfo),
[counter definition](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters).

The manually dispatched `Portable extraction matrix` workflow runs Linux,
macOS and Windows. It measures flat and grouped tiny files and one large ZIP
entry, plus the existing RAM-materialization diagnostic. It records filesystem
and Windows antivirus status without changing protection settings. Hosted runners
are useful for portability and bottleneck discovery, not controlled hardware
leadership claims. Run identifiers and source revisions must accompany results.
The configurable file count stops at 90,000 to leave room for directories under
the native ZIP reader's 100,000-entry limit.

This harness is the first measurement gate in the
[extraction architecture review](research-2026-10.md#extraction-first-review-30-october-2026).
It does not implement the proposed new executor or claim an extraction speedup.
