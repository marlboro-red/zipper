# Zipper's own Deflate decoder

**Kept as the ZIP Deflate decoder. Zipper's streaming decoder in safe Rust decodes Silesia's entries in 629.7 M cycles a round, against 681.3 for Apple's system zlib and 886.0 for zlib-rs.** In wall time on an M2 Max, a large compressible ZIP entry extracts 1.24 times faster than with the system zlib and Silesia ZIP 1.06 times faster at one thread, with ranges apart. Small-file ZIPs, which file creation limits, are unchanged. Nothing was measured on Linux, Windows or x86.

## Question

[M25](../../../docs/roadmap/m25-a-release-build-without-c.md) wants a release build with no C library. On macOS the ZIP reader streams Deflate through the system zlib, which the [system zlib record](../system-zlib/README.md) kept because zlib-rs was 1.3 times slower. Can a decoder in safe Rust, streaming with a 32 KiB history and fixed tables, match the system zlib? The metric is cycles for one round of decoding every Deflate entry of an archive in memory on one thread, with retired instructions beside it.

## Setup

- Apple M5 with 10 cores, macOS 26.7.1. Other agents were building on the machine, so only counters were read, never wall time.
- [`probe.rs`](probe.rs) with [`probe.Cargo.toml`](probe.Cargo.toml) holds the decoders. It includes `src/inflate.rs` by path. One build with the `system` and `libdeflate` features links `/usr/lib/libz.1.dylib`, zlib 1.2.12, through `flate2` 1.1.10, and libdeflate 1.26. A second build with the `rs` feature uses zlib-rs 0.6.8 through the same `flate2`.
- Each decoder gets 512 KiB of input a call and a 512 KiB output buffer, as `stream_entry` in `src/native_zip.rs` does. libdeflate decodes a whole entry in one call and is not a streaming decoder.
- A run first decodes every entry once and checks its size and CRC-32 against the archive, then decodes every entry for the given number of rounds.
- [`measure.py`](measure.py) runs the probe under `/usr/bin/time -l` with 1 round and with 6 rounds, or 11 for the small archives. A sample is the difference in cycles or instructions, divided by the extra rounds. The tables give the minimum of three samples. Every sample is in [`samples.txt`](samples.txt), and the extraction runs are in [`extraction.txt`](extraction.txt).
- The archives are the scoreboard's, from `cargo xtask bench`. Silesia has 12 entries and 211.9 MB of output. The dependency sources have 3,981 Deflate entries and 67.5 MB. Three archives of 64 MiB each hold zeros, a 3-byte pattern and a 12-byte pattern, from [`make_runs.py`](make_runs.py).
- Kernel steps were measured the same way while the decoder was written. Each row of the second table is one build.

## Results

The decoder at `824f21d`, M cycles a round with M instructions in brackets:

| Archive | Zipper | System zlib | zlib-rs | libdeflate, whole entry |
| --- | ---: | ---: | ---: | ---: |
| Silesia | 629.7 (1,384.6) | 681.3 (1,678.2) | 886.0 (2,867.0) | 636.1 (1,435.5) |
| Dependency sources | 153.3 (359.9) | 178.1 (434.9) | 223.7 (703.8) | 161.2 (352.8) |
| `mixed`, 4,002 entries | 49.8 (145.8) | 74.0 (228.0) | 66.7 (226.3) | not run |
| 10,000 flat tiny files | 9.3 (44.9) | 26.4 (103.3) | 18.9 (72.1) | not run |
| `grouped`, 4,000 small files | 15.1 (73.9) | 38.4 (139.7) | 19.0 (79.3) | not run |
| 256 MiB of 25-byte records | 62.5 (280.7) | 123.3 (162.0) | 123.3 (232.6) | 95.2 (153.8) |
| 64 MiB of zeros | 4.7 (25.9) | 40.5 (43.1) | 5.7 (41.5) | not run |
| 64 MiB of a 3-byte pattern | 20.8 (131.9) | 32.5 (43.2) | 200.3 (707.9) | not run |
| 64 MiB of a 12-byte pattern | 17.4 (125.8) | 33.1 (43.2) | 95.4 (705.7) | not run |

Kernel experiments in the order they were made. Each row is one build, in M cycles a round:

| Step | Silesia | Dependency sources | Verdict |
| --- | ---: | ---: | --- |
| First version | 816.1 | 227.7 | Base |
| Matches copied in 16-byte pieces | 714.4 | 180.3 | Kept |
| Margin checks that rule out a wrap | 703.1 | 179.1 | Kept |
| Next table entry looked up before the refill | 631.6 | 163.7 | Kept |
| Doubling the copy distance inside every long match | 644.1 | 165.9 | Reverted |
| Long matches at 16 to 32 bytes read two periods back | 632.6 | 163.3 | Kept |
| Short-period matches double their distance, inline | 632.2 | 165.0 | Reverted |
| The same loop in a function of its own | 630.7 | 162.4 | Kept |
| Tables built by doubling, one write a code | 627.0 | 153.8 | Kept |
| Tables no wider than the longest code | 627.6 | 154.1 | Kept |
| Only the symbols that have a code are counted and sorted | 630.6 | 148.5 | Kept |

The last two steps aimed at small entries, in M cycles a round:

| Step | `grouped` | 10,000 flat tiny files | `mixed` |
| --- | ---: | ---: | ---: |
| Before | 23.4 | 17.2 | 58.0 |
| Tables no wider than the longest code | 21.3 | 15.7 | 55.7 |
| Only the symbols that have a code are counted and sorted | 13.9 | 12.4 | 47.7 |
| Tables in a box, allocated at the first dynamic block | 15.1 | 9.3 | 49.8 |

- The first version let LLVM vectorize the match copy, with about 25 instructions of setup before each match.
- The margin checks cut Silesia from 1,759 to 1,453 M instructions and 11 M cycles. The bounds checks they removed were well predicted.
- Looking up the next literal or length entry from bits already held takes the refill off the chain from one symbol to the next. That one step saved 10% on Silesia.
- The copy changes moved the 256 MiB entry of 25-byte records from 126.7 to 62.5 M cycles, where the system zlib takes 123.3. Each 16-byte load had waited for the store just before it.
- Counting code lengths added one to a counter in memory for each of up to 288 symbols. Most lengths in a small block are zero, so each add waited for the store before it. zlib-rs took 19.0 on `grouped` and was ahead until that step.
- The box is the fix for the review finding under Decision. It costs `grouped` an allocation an entry and saves the tiny files the zeroing of 12 KB an entry, because their fixed blocks need no table of their own.

Extraction with `zipper extract --zip-engine native`, three runs each in turn, in M cycles for the whole process with peak resident memory:

| Archive | Threads | Before, `90d1a27` | After | Peak memory before and after |
| --- | ---: | --- | --- | --- |
| Silesia | 1 | 799.0 to 801.2 | 757.0 to 762.8 | 4.72 to 4.73 MB, 4.80 to 5.00 MB |
| Silesia | 4 | 839.8 to 842.6 | 793.2 to 798.1 | 8.24 to 8.31 MB, 8.13 to 8.16 MB |
| Dependency sources | 1 | 1,512.8 to 1,589.7 | 1,468.3 to 1,531.0 | 8.04 to 8.16 MB, 8.03 to 8.18 MB |
| Dependency sources | 4 | 3,168.1 to 3,237.1 | 3,065.1 to 3,144.9 | 8.60 to 8.70 MB, 8.40 to 8.57 MB |

Silesia at one thread shows 0.05 to 0.08 MB more resident memory in three further runs, and 0.03 to 0.07 MB less peak memory footprint, 2.65 to 2.69 MB against 2.72 MB. The decoder's code now sits in the binary, where the system zlib's sat in a shared library. The extracted Silesia tree equals the corpus byte for byte.

Wall time on a second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. `main` is `02f31b2` with the system zlib, prefix `398480389510be91`, and `inflate` is this change on it, prefix `10b8094edf5e34cf`. `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair in shuffled order and checked every byte. No build ran, and no run lists a busy process. Median milliseconds with the range:

| ZIP | Threads | `main` | `inflate` |
| --- | --- | --- | --- |
| Large compressible entry | 1 | 130.2 (128.9 to 137.5) | 104.9 (103.7 to 107.7) |
| Silesia | 1 | 279.0 (271.7 to 283.2) | 262.0 (259.7 to 270.3) |
| Silesia | default | 116.5 (114.3 to 118.7) | 113.4 (110.4 to 117.2) |
| Dependency sources | 1 | 524.1 (512.5 to 536.2) | 515.0 (498.3 to 525.8) |
| Dependency sources | default | 323.4 (316.5 to 333.6) | 324.1 (317.9 to 340.9) |
| Mixed tree | default | 215.3 (212.9 to 218.3) | 215.6 (212.1 to 228.0) |
| Flat tiny files | default | 425.2 (411.4 to 426.5) | 415.9 (412.4 to 426.7) |
| Grouped files | default | 207.2 (205.8 to 216.3) | 204.6 (200.1 to 208.0) |
| Large random entry | 1 | 113.4 (112.1 to 114.9) | 117.2 (112.8 to 120.4) |

The first two rows have ranges apart. The rest overlap. Peak memory matched on every row. The raw JSON is in `wall-m2-max/`.

## Decision

Kept, and `stream_entry` now decodes Deflate entries with it. The decoder is below the system zlib on every archive measured, by 8% on Silesia and 14% on the dependency sources. It is below zlib-rs on every archive too, by 29% and 31% on those two and by at least 17% on the others. It is within 1% of libdeflate on Silesia, which decodes a whole entry at once.

`flate2` stays for the other readers, and `Cargo.toml` still links the system zlib on macOS. Removing it waits for timed runs and for those readers to move.

The adversarial review found no stream that the decoder and zlib treat differently, in 763,000 generated and mutated streams, and no archive that publishes on failure. It confirmed one regression. With the 11,952-byte decoder in `stream_entry`'s stack frame, 100,000 small entries at 64 threads peaked at 67.90 to 68.93 MB against 66.57 to 67.72 MB before. The decoder is now under 128 bytes and its tables live in a box. Two more sessions of five runs each followed the fix. In the first the new binary peaked at 68.68 to 69.16 MB against 68.42 to 68.62 MB for the old one. In the second it peaked at 67.11 to 69.07 MB against 67.93 to 68.57 MB, and those ranges overlap.

Three ideas were not tried. Table entries that hold two literals would shorten the chain on literal-heavy data and cost more per table. Unchecked reads would remove the bounds checks that remain on the second refill of a pass. The [LZMA record](../../lzma/unchecked-tables/README.md) found such checks cost far less time than their share of instructions. Subtracting a whole table entry from the bit count saves an instruction on x86 and none on Arm, where the subtract extends its operand.

## Limits

- Counters, not time. Cycles follow time on one core at one frequency, and a round here never leaves the cache of one core. The coordinator's timed runs with `scripts/compare_zip_readers.py` decide the scoreboard rows.
- The final table and the extraction rows were measured while other agents loaded the machine, at a load average near 10. Cycles then run up to 2% above the idle numbers of the kernel tables.
- Peak memory at 64 threads moves by 2 MB between sessions for one binary, so a difference of 0.3 MB is not established either way.
- One Apple M5. On x86, zlib-rs has SIMD paths this decoder lacks, so the result on Linux and Windows is unknown. `scripts/compare_workloads.py` on CI runners can measure it.
- The dependency sources at one thread overlap their baseline, because file creation limits that workload.
- The archives come from Python `zipfile` at level 6, apart from the three pattern files.
- The system zlib comes from the operating system, so a macOS update can change its numbers.

## Reproduce

```sh
mkdir -p probe/src && cp probe.rs probe/src/main.rs && cp probe.Cargo.toml probe/Cargo.toml
cargo build --release --features system,libdeflate --manifest-path probe/Cargo.toml
python3 measure.py probe/target/release/inflate-probe ours,zlib,libdeflate target/bench-work/silesia.zip 3 1 6
```

Set the `#[path]` line in `probe/src/main.rs` to this repository's `src/inflate.rs` first. Build with `--features rs` for zlib-rs, where the decoder named `zlib` is zlib-rs. Use 11 as the last argument for the small archives. For the extraction rows, build `zipper` at `90d1a27` and at this commit and run each under `/usr/bin/time -l`.
