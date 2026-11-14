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
- `corpus --source dependencies` builds the second corpus, `dependencies.tar`, from the crates.io sources that `Cargo.lock` pins. Each crate comes from the `.crate` file that `cargo fetch --locked` caches, and only when its sha256 matches the checksum in `Cargo.lock`. It does not read `~/.cargo/registry/src`, where Cargo and build scripts can add files.
- `corpus` refuses a crate entry outside its own folder, and a pair of names that a case-insensitive file system would merge. Either one would leave a different tree on the M2 Max than on Linux. At `86be136f` the corpus holds 114 crates in 5,501 files, and `dependencies.tar` has 90,931,200 bytes with sha256 `b8191b36e54aadd526c311ad00c9099a5bcd7544a18d37afc3b4f798eb7b63e7`. Another `Cargo.lock` gives other bytes.
- `corpus` writes `corpus-source.json` beside the tar, with the source's name, the tar's size and sha256, and where the tree came from. `extract` and `all` read it to find the tree, the streams and the WIM, and copy it into the JSON as `source`.
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

On the second machine, in a checkout of the measured commit, build both binaries and smoke one round first. The WIM row needs `wimlib-imagex`, which `brew install wimlib` provides.

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

The second corpus runs the same way from its own folder.

```sh
cargo fetch --locked
python3 scripts/m31_decoders.py corpus --harness target/release/zipper-decode-bench \
  --source dependencies --out target/m31/corpus-dependencies
python3 scripts/m31_decoders.py all --harness target/release/zipper-decode-bench \
  --zipper target/release/zipper --corpus target/m31/corpus-dependencies \
  --rounds 1 --runs 1 --trial --allow-busy --out target/m31/smoke-dependencies.json
python3 scripts/m31_decoders.py all --harness target/release/zipper-decode-bench \
  --zipper target/release/zipper --corpus target/m31/corpus-dependencies \
  --rounds 11 --runs 7 --out target/m31/m31-decoders-m2-max-dependencies.json
```

On a hosted x86-64 Linux runner, dispatch [`m31-decoders.yml`](../../.github/workflows/m31-decoders.yml). Its `corpus` input picks `silesia` or `dependencies`. It installs 7-Zip, bsdtar and `wimlib-imagex`, builds the same binaries, fetches the locked crates and builds the corpus. It uploads the JSON, `corpus-source.json` and the streams' `corpus.json` as the artifact `m31-decoders-linux-x86_64-CORPUS`, also when a step fails. It runs only when dispatched.

```sh
gh workflow run m31-decoders.yml --ref main -f corpus=dependencies -f rounds=11 -f runs=7
gh run list --workflow m31-decoders.yml --limit 1
gh run download RUN_ID --name m31-decoders-linux-x86_64-dependencies --dir target/m31/linux-dependencies
```

The first Linux run listed `7zz` under `references_missing`. Ubuntu's `7zip` package installs `7z`, `7za` and `7zr` but no `7zz`, which comes in `7zip-standalone`. The step printed the version through a pipe, and the default shell has no `pipefail`, so the missing command did not fail it. The workflow now installs the `7zz` of 7-Zip 26.06 from its release on GitHub, checks the archive's sha256 `2b539cf067b2ad48e1baa5093dbaa8818b73542d1222a0ffde9dc5f2922ea62e`, and fails unless `7zz`, `bsdtar` and `wimlib-imagex` are all on the path. The M2 Max runs 7-Zip 26.06 from Homebrew, so both machines run one version.

The smoke test in `benchmarks/decoders/decode-bench/tests/smoke.rs` runs every decoder for one round on a 150,000-byte input and checks the JSON, so the gate keeps the bench working. Two more build the dependencies corpus from a made-up Cargo home. They check that it takes only the crate whose sha256 the lockfile pins and writes the same tar twice. They also check that it refuses an entry outside its crate's folder, a pair of names that differ only in case, and a crate with another checksum.

## Verdicts

Each decoder ran on Silesia and on the dependencies corpus, on the M2 Max and on hosted x86-64 Linux. On 14 November 2026 the user decided that the best alternative is the fastest of the pure-Rust crates and the crates that wrap a C library. A decoder becomes a crate only when it leads that one with ranges apart on both machines and both corpora.

| Decoder | Verdict | Against the best pure-Rust crate | Against the best crate that wraps C |
| --- | --- | --- | --- |
| [Zstandard](../../benchmarks/decoders/zstandard/README.md) | Stays inside Zipper | 0.292 to 0.447 times `ruzstd`'s time, ranges apart in all four runs | Ahead of libzstd on the M2 Max, behind it on Linux at 1.342 and 1.343, ranges apart |
| [Deflate and gzip](../../benchmarks/decoders/deflate-and-gzip/README.md) | Stays inside Zipper | 0.678 to 0.721 times `zlib-rs`'s time on the M2 Max, a tie on Linux | Behind libdeflate on Linux at 1.229 to 1.328, tied or behind on the M2 Max |
| [LZMA and xz](../../benchmarks/decoders/lzma-and-xz/README.md) | Stays inside Zipper | xz at 0.615 to 0.749 and LZMA at 0.817 to 0.894 times `lzma-rust2`'s time, ranges apart in all four runs | Ahead of liblzma on the M2 Max, behind it on Linux at 1.089 to 1.199, ranges apart |
| [BZip2](../../benchmarks/decoders/bzip2/README.md) | Becomes a crate | 0.609 to 0.701 times `libbz2-rs-sys`'s time, ranges apart in all four runs | 0.634 to 0.723 times libbzip2's time, ranges apart in all four runs |
| [XPRESS](../../benchmarks/decoders/xpress/README.md) | Becomes a crate | 0.279 to 0.330 times `xpress-huffman`'s time, ranges apart in all four runs | No such crate reads XPRESS |

Each ratio is Zipper's median decode time over the other's, at one thread, across the four runs. The records give each run's range, the extraction rows and the limits.

- M23 does not start. LZMA's verdict is not a crate, and M31 made a crate verdict the condition for M23.
- As a finding, Silesia WIM extraction takes 1.348 times 7-Zip's time on the M2 Max and 1.304 times on Linux, though the XPRESS decoder leads `xpress-huffman` three times over.

## Closed on 14 November 2026

- Exit check 1 passes. Each record gives decode rows on both machines and both corpora against every alternative this page names. It also gives extraction rows against bsdtar and 7-Zip, with ranges. LZMA's extraction is the `.tar.xz` row, as the Measure section says. XPRESS has a WIM row on the M2 Max for Silesia and on Linux for both corpora.
- Exit check 2 passes. Each record ends in a verdict, and the table above lists them.
- Exit check 3 passes. Each crate verdict rests on two machines and two corpora, and no record claims a lead that trails or overlaps in any run.
- `86be136f` wrote the first records from Silesia. `5e6dd97b` added the dependencies corpus and the Linux run with 7-Zip, and this close adds the M2 Max WIM row and the user's rule.
- Both M2 Max decode runs passed `--allow-busy` with a busy process at the start, as each record's limits state. Every verdict rests on rows whose ranges are apart in all four runs.
