# Profile-guided optimization of the release binary

Reverted. A profile-guided build made no measured LZMA workload faster, even with a profile trained on the measured Silesia archives themselves. Only BZip2 tar gained, by 2.1%, and the other ten workloads had overlapping ranges.

## Question

Does a profile-guided release build (PGO) extract faster than the plain release build, above all on LZMA, whose decoder is the largest gap to 7-Zip? The hypothesis was a few percent from better branch layout in the decoder loop, with no workload slower beyond the spread.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1, on 10 November 2026, with Rust 1.96.0 and its `llvm-tools` component. No builders, Kani or TLC ran, and no timed run lists a busy process.
- Both binaries are main at `d776338` with the release profile, thin LTO and one codegen unit:
  - `plain`, binary SHA-256 prefix `6539f8bf8364ecb3`;
  - `pgo`, prefix `b2326bbf6f35fc71`, built by [`build.sh`](build.sh) with `-Cprofile-use`.
- The `pgo` profile comes from an instrumented binary that extracted the dependency sources, not Silesia, at one and four threads. The training archives were 7z LZMA2 solid, 7z LZMA non-solid, ZIP Deflate, ZIP LZMA, and tar with xz, gzip, zstd, BZip2 and lzip. No measured archive is in this profile.
- As an upper bound, a second profile came from the same instrumented binary extracting `silesia.7z` and `silesia.tar.xz` at one thread. That build is `pgo-silesia`, prefix `eb0408b664e23285`.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte against the Silesia tree. `silesia.tar.xz` (`xz -6`), `silesia.tar.gz` (`gzip -6`), `silesia.tar.zst` (`zstd -19`) and `silesia-deflate.zip` (7-Zip Deflate) were built for this record. The 7z archives are those of the [sevenz-rust2 record](../../lzma/sevenz-rust2/README.md), and the rest are the codec scoreboard's.

## Results

Median wall seconds with the range, at one thread unless the row says default threads:

| Workload | `plain` | `pgo` | `pgo` ÷ `plain` |
| --- | --- | --- | ---: |
| 7z LZMA2 solid | 1.145 (1.133 to 1.164) | 1.150 (1.143 to 1.184) | 1.004 |
| 7z LZMA solid | 1.138 (1.129 to 1.150) | 1.144 (1.134 to 1.149) | 1.005 |
| 7z non-solid, default threads | 0.389 (0.380 to 0.394) | 0.392 (0.383 to 0.397) | 1.006 |
| tar xz | 1.210 (1.196 to 1.214) | 1.214 (1.194 to 1.222) | 1.004 |
| tar lzip | 1.117 (1.098 to 1.125) | 1.127 (1.122 to 1.135) | 1.009 |
| tar BZip2 | 2.856 (2.843 to 2.877) | 2.796 (2.792 to 2.808) | 0.979 |
| tar gzip | 0.201 (0.192 to 0.205) | 0.202 (0.192 to 0.202) | 1.002 |
| tar zstd | 0.145 (0.142 to 0.157) | 0.150 (0.143 to 0.159) | 1.040 |
| ZIP Deflate | 0.218 (0.209 to 0.220) | 0.212 (0.193 to 0.221) | 0.972 |
| ZIP Deflate, default threads | 0.082 (0.081 to 0.093) | 0.089 (0.073 to 0.092) | 1.086 |
| ZIP LZMA | 1.133 (1.124 to 1.141) | 1.140 (1.134 to 1.146) | 1.006 |
| ZIP zstd | 0.165 (0.162 to 0.176) | 0.173 (0.154 to 0.176) | 1.047 |

The in-sample upper bound, in its own session:

| Workload | `plain` | `pgo-silesia` |
| --- | --- | --- |
| 7z LZMA2 solid | 1.139 (1.125 to 1.146) | 1.149 (1.144 to 1.154) |
| tar xz | 1.220 (1.215 to 1.223) | 1.226 (1.218 to 1.231) |

CPU time matched wall time at one thread, and peak memory moved by at most 1 MiB. The raw JSON is beside this page, one file per row.

## Decision

Reverted. No build setting changes. The LZMA decoder's hot loop was already laid out by hand in the [tree-preload](../../lzma/tree-preload/README.md) and [7-Zip study](../../lzma/7zip-study-kernel/README.md) experiments, and a profile of the measured data itself could not improve it. The 2.1% BZip2 gain does not justify a profile that every release on three platforms must rebuild and keep current.

## Limits

- One Mac, one compiler version and one LLVM. PGO on x86-64 or with a newer Rust may differ.
- The training set is small. A larger or more varied profile was not tried, but the in-sample bound makes a gain on LZMA unlikely.
- The BZip2 gain is real but was measured once. It may be worth a targeted look at the BZip2 decoder's branches.

## Reproduce

1. Run `benchmarks/pgo/lzma-and-readers/build.sh target/bench-work/pgo` at `d776338`. It writes `zipper-plain`, `zipper-pgo` and `merged.profdata`.
2. Build the four Silesia archives listed in Setup into `target/bench-work/pgo/measure`, from `target/bench-corpora/codecs/silesia.tar` and the Silesia folder.
3. For each workload, run:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/pgo/measure/silesia.tar.xz \
  target/bench-corpora/silesia/silesia --archive-format native --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader plain native target/bench-work/pgo/zipper-plain \
  --reader pgo native target/bench-work/pgo/zipper-pgo \
  --reader-threads plain 1 --reader-threads pgo 1
```

Use `--archive-format 7z` for the 7z archives, and `auto` in place of `1` for default threads.
