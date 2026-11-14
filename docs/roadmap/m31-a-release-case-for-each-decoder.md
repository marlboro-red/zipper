# M31. A release case for each decoder

Each of Zipper's own decoders is measured against the Rust crates users already have, on more than one kind of machine. A decoder becomes its own crate only where that measurement shows a clear lead.

Zipper's decoders were measured against C libraries, such as libdeflate, libzstd and zlib, almost only on Apple Silicon. A separate crate adds value only if it beats what a Rust program can use today, and only where the lead holds on the machines most programs run on.

Scope:

- Compare each decoder with its Rust alternatives. Zstandard goes against `ruzstd` and libzstd, and Deflate and gzip against `zlib-rs`, `miniz_oxide` and libdeflate. LZMA and xz go against `lzma-rs`, `lzma-rust2` and liblzma. BZip2 goes against `libbz2-rs-sys` and libbzip2, and XPRESS against any Rust crate that reads it. The LZX, LZMS, LZFSE and LZVN decoders were removed on 12 November 2026 for licence reasons, as [M32](m32-every-reader-under-zippers-licence.md) records.
- Measure decode alone in one process, and end-to-end extraction, on the second machine, an Apple M2 Max, and on an x86-64 Linux CI runner.
- Record each decoder's case as one of three verdicts. It becomes a crate, stays inside Zipper, or is not ready yet. Name the lead and its spread.
- M23, the LZMA decoder as its own crate, starts only if LZMA's verdict is a crate.

Exit checks:

1. A record for each decoder above gives its decode and extraction numbers on both machines, against every alternative it names, with ranges.
2. Each record ends in a verdict, and this page lists them.
3. No claim of leadership rests on one machine or one corpus.

## Measure

[`scripts/m31_decoders.py`](../../scripts/m31_decoders.py) drives both measurements. The decode bench in [`benchmarks/decoders/decode-bench`](../../benchmarks/decoders/decode-bench) holds every decoder this page names and is never published.

- `corpus` copies or fetches Silesia and writes `silesia.tar` with fixed owners, modes and times, so its bytes and its sha256 match on every machine. The bench then writes one stream per codec from that tar.
- The streams come from libraries that the lockfile pins. They are raw Deflate and gzip from `zlib-rs` at level 6, Zstandard from libzstd at level 3 with its checksum, and xz and LZMA from liblzma at preset 6. BZip2 comes from `libbz2-rs-sys` at level 9.
- XPRESS comes from the bench's own greedy encoder, written from [MS-XCA](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-xca/), in 32 KiB chunks as WIM stores them. When `wimlib-imagex` is installed, `corpus` also writes `silesia.wim` with XPRESS chunks for the extraction row.
- `decode` runs each decoder in its own process for each sample. The process times only the decode call, into an output buffer written beforehand. It then compares every byte with the original and reports wall and CPU time. Its parent records the peak RSS.
- Every round runs the decoders in a new seeded order, after one warmup round. `decode` refuses fewer than 7 rounds without `--trial`, and it refuses to start while another process uses 25% of a core, as `scripts/bench_common.py` decides.
- `extract` runs [`scripts/compare_zip_readers.py`](../../scripts/compare_zip_readers.py) on each tar stream, with Zipper at one thread and at all threads against bsdtar and 7-Zip at one thread. The WIM runs against 7-Zip. LZMA has no extraction row. Zipper writes `silesia.tar.lzma` out as one file, `silesia.tar`, where bsdtar reads the tar inside, so the trees differ.
- A row whose comparison fails is kept in the JSON as an error with the end of its stderr, and the run goes on. The JSON is written after the decode rows and after each extraction row. `all` and `extract` exit nonzero when any row failed. The JSON lists rows left out on purpose, such as LZMA, under `extract.skipped`.
- `corpus` moves the manifest that `scripts/fetch_silesia.py` writes beside `tree/silesia` out of `tree`, and refuses a `tree` that holds anything else. `wimlib-imagex` captures all of `tree`, so the manifest made the hosted run's WIM hold one file more than the source.
- The JSON names the corpus and stream sha256s, every crate version from `Cargo.lock`, the machine, the toolchain and the revision with whether the tree was clean.

The alternatives are these crates.

| Codec | Alternatives |
| --- | --- |
| Raw Deflate and gzip | `zlib-rs`, `miniz_oxide` and libdeflate through `libdeflater` |
| Zstandard | `ruzstd` and libzstd through `zstd` |
| xz and LZMA | `lzma-rs`, `lzma-rust2` and liblzma through `liblzma` |
| BZip2 | `libbz2-rs-sys` through `bzip2`, and libbzip2 through `bzip2-sys` |
| XPRESS | `xpress-huffman` |

Zipper's BZip2 decoder runs twice, on a one-thread Rayon pool and on the global pool. `xpress-huffman` is the only Rust crate under a compatible licence that reads XPRESS with Huffman codes. `rust-lzxpress` reads only plain LZ77 XPRESS, and `ms-compress` and `newtua-mscompress` are LGPL, so they are left out. `xpress-huffman` stops once its input cursor reaches the end of a chunk, so the bench gives each coded chunk two more zero bytes before timing.

On the second machine, in a checkout of the measured commit, build both binaries and smoke one round first.

```sh
cargo build --release --locked --package zipper --package zipper-decode-bench
python3 scripts/m31_decoders.py corpus --harness target/release/zipper-decode-bench \
  --out target/m31/corpus --silesia target/bench-corpora/silesia/silesia
python3 scripts/m31_decoders.py all --harness target/release/zipper-decode-bench \
  --zipper target/release/zipper --corpus target/m31/corpus \
  --rounds 1 --runs 1 --trial --allow-busy --out target/m31/smoke.json
python3 scripts/m31_decoders.py all --harness target/release/zipper-decode-bench \
  --zipper target/release/zipper --corpus target/m31/corpus \
  --rounds 11 --runs 7 --out target/m31/m31-decoders-m2-max.json
```

On a hosted x86-64 Linux runner, dispatch [`m31-decoders.yml`](../../.github/workflows/m31-decoders.yml). It installs 7-Zip, bsdtar and `wimlib-imagex`, builds the same binaries, builds the corpus and uploads the JSON as the artifact `m31-decoders-linux-x86_64`, also when a step fails. It runs only when dispatched.

```sh
gh workflow run m31-decoders.yml --ref main -f rounds=11 -f runs=7
gh run list --workflow m31-decoders.yml --limit 1
gh run download RUN_ID --name m31-decoders-linux-x86_64 --dir target/m31/linux
```

The smoke test in `benchmarks/decoders/decode-bench/tests/smoke.rs` runs every decoder for one round on a 150,000-byte input and checks the JSON, so the gate keeps the bench working.

## Verdicts

The second machine ran at `49f68e30` and the hosted Linux runner at `479b42fd`, with the same decoders and bench code. A lead counts only when the ranges are apart on both machines. The best Rust alternative is the fastest pure-Rust crate, since a program that builds no C, as Zipper does, can use only those.

| Decoder | Verdict | Against the best Rust alternative | Against the C library |
| --- | --- | --- | --- |
| [Zstandard](../../benchmarks/decoders/zstandard/README.md) | Becomes a crate, once a second corpus repeats the lead | 0.292 times `ruzstd`'s time on the M2 Max and 0.332 on Linux, ranges apart | Ahead of libzstd on the M2 Max, ranges overlapping on Linux, so no lead |
| [Deflate and gzip](../../benchmarks/decoders/deflate-and-gzip/README.md) | Stays inside Zipper | 0.720 times `zlib-rs`'s time on the M2 Max, a tie on Linux | Tied with libdeflate on the M2 Max, behind it on Linux |
| [LZMA and xz](../../benchmarks/decoders/lzma-and-xz/README.md) | Becomes a crate, once a second corpus repeats the lead | 0.690 times `lzma-rust2`'s time on xz on the M2 Max and 0.719 on Linux, ranges apart | Ahead of liblzma on the M2 Max, tied on Linux xz and behind on Linux LZMA, so no lead |
| [BZip2](../../benchmarks/decoders/bzip2/README.md) | Becomes a crate, once a second corpus repeats the lead | 0.609 times `libbz2-rs-sys`'s time on the M2 Max and 0.651 on Linux, ranges apart | 0.663 times libbzip2's time on the M2 Max and 0.644 on Linux, ranges apart |
| [XPRESS](../../benchmarks/decoders/xpress/README.md) | Not ready yet | 0.329 times `xpress-huffman`'s time on the M2 Max and 0.314 on Linux, ranges apart | None measured |

Each ratio is Zipper's median decode time over the other's, at one thread. The records give the ranges, the extraction rows and the limits.

## What keeps M31 open

- Exit check 1 is not met for XPRESS. The M2 Max has no WIM extraction row, since `wimlib-imagex` is not installed there. The Linux run found no `7zz`, so its WIM row has no reader beside Zipper.
- Exit check 3 is not met for any lead. Every row comes from Silesia, and the check lets no lead rest on one corpus. The three crate verdicts stand once a second corpus repeats their leads.
- M23 should not start yet. LZMA's verdict is a crate only once a second corpus repeats its lead over `lzma-rust2`. M23's own exit check 2 then compares the crate with liblzma, and these rows show Zipper tied with liblzma on Linux xz and behind it on Linux LZMA.
