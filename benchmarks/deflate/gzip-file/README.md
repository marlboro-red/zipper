# One gzip or zlib stream, end to end

Kept five changes of thirteen experiments. On a quiet Apple M2 Max, `zipper extract` of one 212 MB `.gz` file now takes 209.5 ms against 218.7 ms for `libdeflate-gunzip`, where it took 227.1 ms, with the two ranges apart. A zlib-compressed SWF of the same data takes 214.5 ms against 218.5 ms for libdeflate's zlib call, where it took 348.9 ms. Those two ranges overlap, so the SWF is level with libdeflate, not ahead of it. A zlib xar takes 322.1 ms against 343.1 ms for the system `xar`. One machine and one corpus.

## Question

Why is the whole `zipper extract` of one `.gz` file about 5% slower than `libdeflate-gunzip`, when the decoder alone is faster than libdeflate 1.26 on the same machine? The [last pass over the decoder](../last-pass/README.md) left the decode loop at its limit, so this record looks around it, in three places. They are how bytes stream from the archive to the file, what staging and start-up cost, and the CRC-32. The same questions apply to zlib streams, whose checksum is Adler-32 and which reach users through SWF, xar, DMG, SquashFS, CramFS and VMDK.

Each experiment has one hypothesis and one metric, stated in the stage table. The published sentence is "`zipper extract` of one gzip file takes ___ ms against ___ ms for `libdeflate-gunzip`", with the same sentence for zlib.

## Setup

- A second machine, an Apple M2 Max with 12 cores and macOS 27.0, on 12 November 2026. Nothing else ran on it during a timed run. `time_tools.py` refuses to start while another process uses 25% of a core, and the machine's own services stopped it six times.
- `base` is `42ae4e8`, binary SHA-256 prefix `86874b9b8ef050f3`. `final` is the five kept changes on it, prefix `c88893ab14b5750d`. Both were built with Rust 1.99.1 for `aarch64-apple-darwin`, `base` and the builds of stages 1 to 5 on another Mac and the builds from stage 7 on the timed one. The branch was moved onto a newer `main` after the timing, with no change to its own code.
- The gzip file is the twelve Silesia files in name order, 211,938,580 bytes, under Python `gzip` at level 6 with mtime 0, 68,227,957 bytes. A second gzip file holds 268,435,456 random bytes, which gzip writes as stored blocks.
- The zlib workloads are three. `blob.swf` is a zlib-compressed SWF whose body is a 13-byte movie header and the same 212 MB, 68,227,974 bytes. `silesia.xar` is the Silesia folder packed by the system `xar` with zlib at level 6, 68,222,206 bytes. `silesia.dmg` is the folder as an HFS+ image in zlib blocks from `hdiutil`, 78,471,990 bytes.
- The references are `libdeflate-gunzip` 1.25 from Homebrew, `/usr/bin/gzip`, 7-Zip 26.06 and the system `xar`. For zlib, [`zlib_reference.c`](zlib_reference.c) calls `libdeflate_zlib_decompress` over the whole file in memory and writes the result, as `libdeflate-gunzip` does for gzip. `libdeflate-gunzip` and the zlib reference write to a file through stdout.
- [`time_tools.py`](time_tools.py) runs every tool once a round in rotating order, one warmup round and then 11 to 21 measured rounds. It checks the SHA-256 of every output and records wall time, user and system CPU time and peak resident memory from `wait4`. Zipper runs as `zipper extract ARCHIVE -o NEW_FOLDER` at default threads. The single-file reader uses one thread.
- The stage results are in `stages/`, named by stage number. In them `e1`, `e2`, `e4` and `e3` are the builds after stages 1, 5, 7 and 8, in that order. The `w` and `r` rows of stages 2 to 4 are one exploration build of stage 1 that reads its window or read size in KiB from an environment variable.
- Profiles ran outside the timed runs. [`sample_split.py`](sample_split.py) sums `sample` at 1 ms over 8 or 12 runs. [`syscount.c`](syscount.c) is a library that counts and times the file system calls of one process, since neither machine gives `dtrace` the root it needs.
- [`probe.rs`](probe.rs) times checksum candidates over the 212 MB in pieces of 32 KiB, 512 KiB and the whole, best of nine rounds. It first checks each against a reference on 1,508 lengths at three offsets. [`probe.Cargo.toml`](probe.Cargo.toml) and the three `probe_*.rs` modules sit beside it, and [`checksum-probe.txt`](checksum-probe.txt) holds its output.

## Results

### Where the time goes

`base` extracts the gzip file in 229.1 ms (225.5 to 239.8), with 219.9 ms of CPU. `libdeflate-gunzip` takes 217.3 ms (215.7 to 230.1) with 208.3 ms of CPU, and `gzip` 256.0 ms.

| Part of `base` | Time | How it was measured |
| --- | ---: | --- |
| Process start to `main` | 2.6 ms | `zipper --version`, 60 runs. `/usr/bin/true` takes 1.7 ms of it. |
| Format sniff, staging and publication | 1.3 ms | A 6-byte `.gz` extracts in 4.0 ms. 55 `open` calls take 0.5 ms, and two `mkdir` and one `rename` 0.13 ms. |
| Reading the archive | 3.4 ms | 121 `pread` calls for 68.3 MB |
| Writing the file | 34.5 ms | 204 `write` calls of up to 1 MiB |
| CRC-32 | 12.8 ms | The probe on 32 KiB pieces, the size `base` hands it. 3.9% of profile samples. |
| Copies | 12.6 ms | `memmove`, 5.5% of profile samples |
| Decoding | 162 ms | The rest. The profile puts 79.7% of samples in the decode loop. |

`libdeflate-gunzip` maps the archive, decodes into one 212 MB buffer and writes it with one call that takes 38.6 ms. It spends 35.3 ms of system time against 26.7 ms for `base`, and 173.2 ms of user time against 193.1 ms. So the gap is in user time, and streaming already costs less system time than the whole-buffer shape. `base` retires 2,001 M instructions in 800 M cycles, and `libdeflate-gunzip` 1,735 M in 758 M.

`base` decoded into a 64 KiB window, so each decode step gave at most 32 KiB. Each step moved the last 32 KiB to the window's front. The reader copied the step into a 1 MiB buffer, and a buffered writer copied it again. Every output byte was copied three times.

The zlib profile of `base` on the SWF, 349.5 ms in all, puts 46.0% of samples in the decode loop, 32.6% in Adler-32 and 17.1% in `write`, which is called 6,593 times for 55.9 ms. On the xar, 455.3 ms, SHA-1 takes another 25.1%.

### Stage table

Wall times are medians in milliseconds with the range, on the Silesia gzip file unless the row names another archive. Every sample is in the JSON beside this page.

| # | Experiment and hypothesis | Metric | Result | Verdict |
| --- | --- | --- | --- | --- |
| 1 | Decoding a `.gz` into a 544 KiB window and writing each piece from the window removes the three copies | Wall and user time | 213.8 (211.0 to 266.7) against 227.6 (224.8 to 235.5), user time 179.0 against 193.8 | Kept |
| 2 | A window of 128 KiB, 256 KiB, 1, 2 or 4 MiB beside the history beats 512 KiB | Wall time | 223.3, 216.7, 215.5, 213.0 and 212.7 against 213.8, with peak memory 5.3 to 9.2 MiB against 5.7 | Reverted, 512 KiB and larger are level and smaller is slower |
| 3 | Writing from the old 64 KiB window with no copy is enough | Wall time | 251.3 (247.1 to 259.0) against 227.6 for `base` | Reverted, 6,500 small writes cost more than the copies |
| 4 | Reads of 64 KiB, 256 KiB, 4 MiB, 16 MiB or the whole file beat 1 MiB | Wall time | 222.6, 214.7, 215.4, 220.6 and 224.6 against 213.3 | Reverted, 1 MiB is the fastest and the whole file the slowest |
| 5 | An Adler-32 the compiler can turn into vector code is faster than the 16-lane sum | The probe, then the SWF and the xar | 4.19 ms against 102.20 ms over 212 MB. SWF 248.8 (246.8 to 269.6) against 348.8 (346.4 to 356.2). xar 357.2 (353.9 to 362.1) against 459.1 (452.3 to 464.9) | Kept |
| 6 | Five other safe forms of Adler-32 | The probe at 512 KiB pieces | Blocks of 16, 32 and 64 bytes with two sums a block took 102.23, 27.75 and 15.90 ms. Position sums over 32 and 128 bytes took 55.75 and 8.12 ms, against 4.19 for 64 | Reverted |
| 7 | A read that fills the caller's buffer cuts the writes of the SWF and xar readers | Write calls, then wall time | SWF 216.0 (213.2 to 270.7) against 252.9 (246.3 to 288.8), system time 26.7 against 55.7. xar 321.1 (318.3 to 324.3) against 357.4 (355.0 to 375.1) | Kept |
| 8 | Folding the CRC-32 with carry-less multiplies beats crc32fast's three chains of CRC instructions | The probe, then wall and CPU time | 3.84 ms against 8.02 ms over 212 MB. 210.8 (208.7 to 217.3) against 214.6 (212.7 to 231.0) over 21 rounds, CPU 202.9 against 206.8 | Kept on the probe and CPU time. These wall ranges overlap, and the `.tar.gz` row below has them apart |
| 9 | Other folds. 4, 12 or 16 lanes, no `eor3`, or the lanes held as `u128` | The probe at 512 KiB pieces | With `eor3`, 5.07, 3.85 and 3.87 ms for 4, 12 and 16 lanes. Without it, 4.66 ms for 8 lanes. As `u128`, 7.73 ms for 8 lanes | Reverted |
| 10 | One open file for the 55 sniff reads | Arithmetic from the profile | The 55 opens take 0.5 ms of 211, which is 0.25% and below what a timed run resolves | Rejected before building it |
| 11 | Staging costs time that a direct write would save | The call profile | Two `mkdir` and one `rename` take 0.13 ms | Rejected before building it, nothing to remove |
| 12 | A checksum computed inside the decoder's output step, on bytes still in the first cache, beats a pass over each piece | The probe at three piece sizes | The fold takes 4.10 ms on 32 KiB pieces and 3.84 ms on 512 KiB pieces and on one 212 MB piece. Adler-32 takes 4.29, 4.19 and 4.18 ms | Rejected before building it, the pass does not get faster in a nearer cache |
| 13 | Pieces under 64 KiB pass through a buffer before the file, after the review found one write a gzip member | Instructions retired on 400,000 one-byte members | 558 M against 11,244 M after stage 1 and 612 M for `base` | Kept |

Stage 3 explains why `base` copied. The copies bought writes of 1 MiB. Stage 1 keeps large writes without the copies, because the window itself is large.

Stage 7 changes every reader of a Deflate stream. `.tar.gz`, the 7z Deflate archive and the `.gz` file did not move, since their callers already asked again until a buffer was full.

libdeflate's own checksums take 3.48 ms for CRC-32 and 3.53 ms for Adler-32 on the same 212 MB. The kept forms take 3.84 and 4.19 ms.

### Final rows

`base` against `final` and the references, median milliseconds with the range, CPU milliseconds and peak resident memory. 15 measured rounds each, at default threads.

| Archive | Tool | Wall ms | CPU ms | Peak MiB |
| --- | --- | --- | ---: | ---: |
| Silesia `.gz` | `base` | 227.1 (225.4 to 238.6) | 220.0 | 6.3 |
| Silesia `.gz` | `final` | 209.5 (208.4 to 215.5) | 202.8 | 5.8 |
| Silesia `.gz` | `7zz` | 665.3 (661.8 to 670.3) | 655.6 | 4.3 |
| Silesia `.gz` | `libdeflate-gunzip` | 218.7 (217.1 to 230.2) | 209.1 | 268.9 |
| Silesia `.gz` | `gzip` | 255.3 (251.1 to 272.6) | 246.5 | 1.3 |
| random `.gz` | `base` | 71.5 (70.9 to 89.0) | 62.9 | 7.1 |
| random `.gz` | `final` | 62.4 (61.7 to 79.9) | 53.9 | 5.6 |
| random `.gz` | `libdeflate-gunzip` | 77.0 (76.2 to 83.8) | 66.1 | 513.8 |
| random `.gz` | `gzip` | 83.6 (81.8 to 133.6) | 74.0 | 1.3 |
| zlib SWF | `base` | 348.9 (345.5 to 355.4) | 340.6 | 5.4 |
| zlib SWF | `final` | 214.5 (213.5 to 218.0) | 206.8 | 6.3 |
| zlib SWF | `7zz` | 704.4 (702.4 to 725.9) | 695.8 | 4.3 |
| zlib SWF | `libdeflate-zlib` | 218.5 (216.0 to 220.6) | 209.2 | 268.9 |
| zlib xar | `base` | 456.8 (454.3 to 498.7) | 449.2 | 6.5 |
| zlib xar | `final` | 322.1 (320.9 to 368.7) | 316.0 | 7.4 |
| zlib xar | `xar` | 343.1 (340.7 to 355.1) | 333.3 | 51.8 |
| zlib xar | `7zz` | 811.2 (809.8 to 816.5) | 803.8 | 4.5 |
| zlib DMG | `base` | 357.0 (353.0 to 362.3) | 351.5 | 72.5 |
| zlib DMG | `final` | 255.0 (251.9 to 260.1) | 249.2 | 72.5 |

Against `libdeflate-gunzip` the Silesia gzip file takes 0.958 of the median, and the slowest Zipper run, 215.5 ms, is below the fastest libdeflate run, 217.1 ms. Against libdeflate's zlib call the SWF takes 0.982, and the ranges overlap from 216.0 to 218.0 ms. The xar takes 0.939 of the system `xar`, with one Zipper run of fifteen, 368.7 ms, above its range. The random gzip file is all stored blocks, where libdeflate pays for a 512 MiB buffer. The DMG takes 0.714 of `base`.

Peak memory rose by 1 MiB on the SWF and the xar. Their readers always held a 1 MiB buffer, and a read now fills all of it where it used to touch the first 32 KiB. A DMG run now and then peaks far above the others, on both builds. One `base` run of three reached 138.0 MiB under `/usr/bin/time -l`, and one run of fifteen of an earlier `final` build reached 111.2 MiB, where every other run stayed at 72.5 MiB. That peak belongs to the DMG reader and not to this change. The runs are in `profile/dmg-peaks.txt` and `stages/dmg-before-stage-13.json`.

### Rows that must not get slower

`base` against `final` through `scripts/compare_zip_readers.py`, which compares every extracted byte. 11 measured rounds each.

| Archive | Threads | `base` | `final` | CPU ms | Peak MiB |
| --- | ---: | --- | --- | --- | --- |
| Silesia `.tar.gz` | 4 | 200.8 (200.0 to 233.5) | 193.9 (193.2 to 195.7) | 220.7, 213.5 | 17.8, 13.9 |
| random `.tar.gz` | 4 | 42.3 (41.4 to 47.6) | 41.5 (40.7 to 45.5) | 66.6, 61.9 | 11.9, 11.9 |
| dependency sources `.tar.gz` | 4 | 249.3 (242.0 to 264.3) | 250.8 (241.8 to 262.1) | 978.6, 975.9 | 11.5, 11.0 |
| Silesia ZIP | 1 | 217.4 (214.1 to 235.8) | 215.9 (214.6 to 238.2) | 208.0, 207.7 | 4.8, 4.8 |
| Silesia ZIP | 4 | 72.2 (70.2 to 82.3) | 71.4 (70.3 to 73.6) | 230.4, 230.8 | 8.1, 8.1 |
| dependency sources ZIP | 4 | 266.9 (258.0 to 278.5) | 266.7 (260.6 to 277.3) | 853.8, 851.2 | 9.2, 9.5 |
| Silesia 7z Deflate | 4 | 73.4 (72.2 to 98.2) | 73.2 (71.9 to 76.9) | 239.9, 239.9 | 13.8, 15.8 |

No row is slower beyond its range. Silesia as `.tar.gz` takes 0.966 of `base` with the ranges apart, which is the CRC-32 fold, since tar already read in full buffers. The random `.tar.gz` saves 4.7 ms of CPU and no wall time beyond its range, because its reader thread waits for the writers. The ZIP rows do not pass through the changed code. Peak memory on the 7z row is 2 MiB higher, from buffers that are now filled.

## Decision

Kept stages 1, 5, 7, 8 and 13.

- **Change.** `src/native_stream.rs` gives a `.gz` file a gzip reader with a 544 KiB window, built by `Reader::gzip_with_window`, and writes each decoded piece from the window to the staged file. A piece under 64 KiB goes through a buffer of that size first, so a file of many small members is not written one member a call. The two 1 MiB copy buffers go for gzip. `Adler32::update` in `src/framing.rs` keeps one 16-bit sum for each byte position of a 64-byte block and weights them once in a run of 5,504 bytes. `Read::read` on the Deflate reader decodes until the caller's buffer is full, and an error that follows decoded bytes waits for the next call with its own kind and message. `src/crc32.rs` folds a gzip member's CRC-32 on aarch64 CPUs that have `aes` and `sha3`, and hands the last lanes and short pieces to crc32fast.
- **Safety.** Staging, publication, the output cap and cancellation are unchanged. The fold is safe Rust inside one function marked with the three CPU features it needs. Its one `unsafe` call follows a runtime check for those features.
- **Memory.** The gzip file reader holds 1 MiB of input, the 544 KiB window and a 64 KiB write buffer. Peak resident memory on the Silesia gzip file fell from 6.3 to 5.7 MiB. The SWF and xar readers peak 1 MiB higher, because a read now fills the 1 MiB buffer they always held.
- **Tests.** `a_gzip_reader_with_any_window_hands_out_what_flate2_reads_whole_cut_and_flipped` did not build before stage 1. Before stage 7, `a_read_fills_the_callers_buffer_until_the_stream_ends` failed with 4,091 of 300,000 bytes, and `an_error_after_decoded_bytes_reaches_the_next_read_with_its_own_message` failed too. In `tests/hostile_stream.rs`, a gzip file of two members and 3 MB at levels 0, 1 and 6 must extract whole and hold an exact cap. It must leave nothing when cut, damaged, one byte over its cap or cancelled. The Adler-32 tests compare a byte-at-a-time sum on every length up to 700 bytes and at the block and run edges, and start from the largest sums over the largest bytes with overflow checks on. The CRC-32 tests compare a bit-at-a-time CRC on every length up to 1,700 bytes at four offsets and three splits. The two checksum tests pass on the old code as well, since the sums did not change, so the plants show that they can fail.
- **Plants.** `scripts/plant.py` ran eleven breaks on the second machine's copy of the tree, and each failed its test. One was a write buffer of no bytes, which undoes stage 13. In the single-file reader they were a cap one byte loose and a piece consumed short of what was written. In the Deflate reader they were a windowed reader that skips the gzip framing, a dropped error after decoded bytes, and a read that returns after one step. In Adler-32 they were a block weight left out and a run twice as long. In the CRC-32 they were a fold constant one bit off, the running CRC left out of the first lane, and a fold that restarts from zero, which the 3 MB gzip file catches. A twelfth break, a piece consumed one byte short, made the reader loop forever and was dropped as a plant.
- **Review.** An adversarial review confirmed two findings and no safety failure. A `.gz` of 400,000 one-byte members cost 14 times the instructions of `base`, because each member's piece was written with its own call. Stage 13 fixes it, and `many_small_gzip_members_reach_the_file_in_one_write_and_large_pieces_with_no_copy` failed with 20,000 writes before the fix. The second finding is the 1 MiB rise in peak memory on a compressible SWF, which stays and is stated above. The review compared `base` and the change on 15,590 cut and flipped gzip, SWF and `.tar.gz` files with no difference in outcome, and on edge sizes around the window, the fold and the Adler-32 run. Nothing was published or left staged after a failure. It read the `unsafe` call as sound.

## Limits

- One machine, one compiler and one corpus. Nothing was measured on Linux, Windows or x86.
- The Adler-32 gain rests on the compiler turning a plain loop into vector code. The probe shows how narrow that is, since the same sum over 32 or 128 positions is 2 to 13 times slower. Another compiler version or target can lose it, and no test checks speed. On x86 the old and new forms were not timed.
- The CRC-32 fold compiles only for aarch64, and CI runs on x86-64, so CI tests the crc32fast path alone. The fold's tests run in the gate on Apple silicon.
- The CRC-32 fold needs `aes` and `sha3`, which Apple silicon has. An aarch64 CPU without them and every other target keep crc32fast. On the gzip file its gain is 1.8% of a median with overlapping ranges, and on Silesia as `.tar.gz` 3.4% with the ranges apart.
- The fold serves gzip members only. ZIP entries, 7z, xz and the other users of crc32fast were left alone.
- A 6-byte `.gz` extracts in 4.0 ms against 2.3 ms for `libdeflate-gunzip`. Loading the 5 MB binary takes 0.9 ms of that and the 55 sniff opens 0.5 ms. Neither was changed.
- The xar reader spends a quarter of its time in SHA-1, which this record did not touch.
- The DMG row has no reference, since 7-Zip unpacks a DMG into partition images and `hdiutil` mounts it.
- `libdeflate-gunzip` and the zlib reference hold the whole output in memory, 269 MiB at peak. Zipper streams in 6 MiB. The comparison is not like for like in memory.

## Reproduce

```sh
python3 make_zlib_workloads.py target/bench-corpora/silesia/silesia WORK/corpus
cp target/bench-corpora/large/large/random/large.bin WORK/corpus/random.bin
python3 - WORK/corpus/random.bin <<'EOF'
import gzip, sys
with open(sys.argv[1], "rb") as plain, open(sys.argv[1] + ".gz", "wb") as sink:
    with gzip.GzipFile(filename="", mode="wb", compresslevel=6, fileobj=sink, mtime=0) as packed:
        packed.write(plain.read())
EOF
hdiutil create -srcfolder target/bench-corpora/silesia/silesia -format UDZO -fs HFS+ -volname silesia WORK/corpus/silesia.dmg
cc -O2 -I "$(brew --prefix libdeflate)/include" -o WORK/zlib-reference zlib_reference.c "$(brew --prefix libdeflate)/lib/libdeflate.a"
./final_rows.sh WORK/final 15 <base binary> <final binary> WORK/corpus target/bench-corpora/silesia/silesia WORK/zlib-reference
python3 rows.py WORK/final/*.json
```

Build `base` at `42ae4e8` and `final` at this commit with `cargo build --release`. For the rows that must not get slower, put the archives the script names in one folder and run `./regress.sh WORK/regress 11 <base binary> <final binary> target/bench-corpora ARCHIVES`.

The profile and the probe:

```sh
cc -O2 -dynamiclib -o syscount.dylib syscount.c
DYLD_INSERT_LIBRARIES=$PWD/syscount.dylib SYSCOUNT_OUT=calls.txt <binary> extract WORK/corpus/blob.bin.gz -o WORK/out
python3 sample_split.py --work WORK/split --rounds 12 --name zipper -- <binary> extract WORK/corpus/blob.bin.gz -o '{out}'
mkdir -p probe/src && cp probe.rs probe/src/main.rs && cp probe.Cargo.toml probe/Cargo.toml && cp probe.build.rs probe/build.rs
cp probe_adler.rs probe/src/adler.rs && cp probe_crc.rs probe/src/crc.rs && cp probe_crc_vectors.rs probe/src/crc_vectors.rs
LIBDEFLATE_LIB="$(brew --prefix libdeflate)/lib" cargo run --release --features libdeflate --manifest-path probe/Cargo.toml -- WORK/corpus/blob.bin 9
```
