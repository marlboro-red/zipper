# Stored Deflate blocks copied straight to the caller

Kept, pending the wall-time run. A `.tar.gz` of 256 MiB of random bytes extracts with 26% fewer instructions than before and 13% fewer than the flate2 build at `742ddef`. Silesia as `.tar.gz` and ZIP is unchanged. Cycle counts on this busy machine are too noisy to rank the builds, so the wall time belongs to a quiet run.

## Question

Why does a gzip stream of incompressible data, almost all stored blocks, extract slower on Zipper's own decoder than on flate2 over zlib? The coordinator measured a 1.15 times slower wall time against `742ddef` on the scoreboard's large-random corpus, with ranges apart. The hypothesis was that each stored byte is copied twice, input to window and window to caller, and that each read returns at most one window. zlib copies stored bytes straight to the caller and keeps only the last 32 KiB as history.

## Setup

- An Apple M2 Max on 11 November 2026, shared with builds and tests of other agents. No timed comparison ran. `/usr/bin/time -l` counted instructions retired and cycles elapsed, and `samply` sampled at 10 kHz outside those runs.
- `before` is `ea91e9c`, `after` is this change on `ea91e9c`, and `flate2` is `742ddef`, the last commit that read gzip through flate2's `MultiGzDecoder`.
- The archives are three, written by `mk.py` and `mkzip.py` in the scratch folder, which this record reproduces below. The first is `target/bench-corpora/large/large/random` (268,435,456 bytes) as a PAX tar under Python `gzip` level 6 with mtime 0 (268,517,711 bytes). The second is Silesia as the same kind of `.tar.gz` (68,229,788 bytes). The third is Silesia as a ZIP at Python `zipfile` level 6 (68,221,469 bytes).
- Each run was `zipper extract ARCHIVE -o NEW_DIR` at default threads. The raw counts are in [raw-counts.txt](raw-counts.txt).

## Results

The profile of `before` on the random archive showed the reader thread in three places. It spent 235 samples in `crc32fast`, 227 in `memmove` and 209 in `pread`. 147 of the `memmove` samples sat in `Inflate::decode`, which copied each stored byte into the window and moved the last 32 KiB to the window's front before each call. `Reader::read` then copied the bytes out again, at most 32 KiB per call. The `flate2` build spent 124 samples in `memmove` and 92 in zlib-rs's CRC-32.

After the change, the reader thread took 363 samples in all, with 149 in `pread`, 135 in `crc32fast` and 71 in `memmove`. Each `read` now fills the caller's 1 MiB chunk across as many stored blocks as fit.

Medians of the instruction counts, in millions, with the runs:

| Archive | Runs | `before` | `after` | `flate2` |
| --- | --- | --- | --- | --- |
| random `.tar.gz` | 6 | 503 | 372 | 427 |
| Silesia `.tar.gz` | 4 | 1,945 | 1,945 | 1,946 |
| Silesia ZIP | 4 | 1,879 | 1,878 | 1,934 |

Cycles ranged widely on the random archive, 238 to 316 million for `before`, 198 to 257 million for `after` and 206 to 263 million for `flate2`. On Silesia the three builds lay within 2% in cycles. Peak resident memory on the random archive was 10.3 to 12.4 MB for `before`, 11.3 to 12.3 MB for `after` and 11.6 to 11.7 MB for `flate2`.

In isolation, `crc32fast` 1.5.3 and zlib-rs 0.6.8 took 39 to 40 million cycles each for 256 MiB in 1 MiB pieces. In 32 KiB pieces they took 49 and 44 million. The two CRC-32 implementations differ little, and this change does not replace `crc32fast`.

## Decision

Kept, pending the coordinator's wall-time run.

- **Change.** `Inflate::copy_stored` in `src/inflate.rs` copies stored blocks from the input straight into the caller's buffer. It reads the block headers and stored lengths in between, and stops at the first Huffman block. It then keeps the last 32 KiB of what it wrote as the window's history. `framing::Reader::read` calls it whenever no decoded bytes are held, and falls back to the window for Huffman blocks. The block header and stored length readers are now functions that both paths share.
- **Tests.** The zlib differential tests in `inflate` also run every stream through `copy_stored` with outputs of 1, 300, 32,769 and 1 MiB bytes. A new test builds matches that reach back across stored blocks of 1 to 131,070 bytes after 0, 7 and 40,000 literals, at distances up to 32,768 and one past the start. The framing reader reads the same streams in output pieces from 1 byte to 1 MiB against flate2. A test that a stored stream fills a 1 MiB read in one call failed before the change.
- **Plants.** Six planted breaks were each caught. The list is in the branch report.

## Limits

- One machine, shared with other work, so no number here is a timing.
- The ZIP reader calls `Inflate::decode` itself and does not use the direct copy. A ZIP Deflate entry of incompressible data still copies each byte twice.
- `BufRead::fill_buf` on the framing reader still decodes through the window.

## Reproduce

Build `742ddef`, `ea91e9c` and this change with `cargo build --release`. Write the archives with these scripts:

```python
# mk.py SOURCE OUT: a PAX tar under gzip level 6 with mtime 0
import sys, tarfile, gzip, os
src, out = sys.argv[1], sys.argv[2]
with open(out, 'wb') as raw:
    with gzip.GzipFile(fileobj=raw, mode='wb', compresslevel=6, mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode='w', format=tarfile.PAX_FORMAT) as t:
            t.add(src, arcname=os.path.basename(src))
```

```python
# mkzip.py SOURCE OUT: a ZIP of a folder's files at level 6
import sys, zipfile, os
src, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    for name in sorted(os.listdir(src)):
        z.write(os.path.join(src, name), arcname=name)
```

Then count each run with `/usr/bin/time -l zipper extract ARCHIVE -o NEW_DIR`, alternating the binaries. For wall time, run `scripts/compare_zip_readers.py` on a quiet machine with `--archive-format native`.
