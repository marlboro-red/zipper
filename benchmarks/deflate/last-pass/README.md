# A last pass over Deflate ZIP extraction

Kept two changes of seventeen experiments. Looking up the symbol after a match before copying the match takes 1.2% off the decoder's time on Silesia, with ranges apart. The ZIP reader now hands a read of stored blocks to the file in one write instead of two. Neither moves a wall-time row beyond its range, so the scoreboard rows stand.

## Question

Is any measured gain left in Deflate ZIP extraction, in safe Rust and with every check kept? The [own decoder record](../../native-zip/own-inflate/README.md) left the decoder within 1% of libdeflate on an M5. This pass looked at four places, which are the decode loop, the path from archive bytes to the file, the cost of a small entry, and how entries are spread over workers. Each experiment has one hypothesis and one metric, stated in the stage table.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build ran during a timed run. Late in the session its file services took CPU between runs, and three rows of large writes show samples of 120 to 520 ms beside medians near 75 ms.
- `base` is `736e11b`, binary SHA-256 prefix `ad5cba1a29589402`. `final` is this change, prefix `1236b563178133c2`.
- The decode probe is [`probe.rs`](probe.rs) with [`probe.Cargo.toml`](probe.Cargo.toml). It includes one variant of `src/inflate.rs`, checks every entry's size and CRC-32 once, and then times whole rounds of decoding every Deflate entry in memory. It feeds 512 KiB of input a call into a 512 KiB window, as the ZIP reader did. [`measure.py`](measure.py) runs the builds in shuffled order, one process a sample. Two runs of the same build differ by up to 0.3%.
- Extraction rows come from `scripts/compare_zip_readers.py` with `--archive-format zip`, one warmup and 7 to 21 measured rounds in shuffled order, and every byte checked. They use no cooldown, as the scoreboard does. Two copies of `base` differed by 1.8% in median at one thread and by 3.4% at four.
- The archives are the scoreboard's. Silesia has 12 entries and 211.9 MB. The dependency sources have 3,981 files and 67.5 MB. `grouped` has 4,000 files and 16.4 MB. `large-random` is one Deflate entry of 256 MiB of random bytes, which `zipfile` writes as 4,097 stored blocks. `large-compressible` is one entry of 256 MiB that packs to 0.65 MB.
- Profiles came from `sample` at 1 ms, outside the timed runs.

## Results

### Where the time goes

At one thread Silesia extracts in 218 ms. The probe decodes its entries in 172.0 ms and the CRC-32 adds 2.9 ms. A profile of the extraction puts 80% of the worker's samples in the decode loop `fast`, 10% in `write` and 4% in the CRC-32. In the probe, `fast` takes 95% of Silesia's decode time and 84% of the dependency sources'.

The decode loop runs 24.0 million times over Silesia, about 25 cycles a pass. Each literal waits for the lookup before it, and each match for two. The rest is branch misses on whether the next symbol is a literal.

On this machine the decoder at `base` already takes 612.3 M cycles a round on Silesia. libdeflate 1.26 takes 628.2 and the system zlib 678.5. On the dependency sources it takes 153.2 against 160.2 for libdeflate.

Decoding is a small part of a small-file archive. The dependency sources decode in 43 ms of CPU, and their extraction takes 1,098 ms of CPU and 277 ms of wall time at default threads. `grouped` decodes in 4.3 ms and extracts in 167.5 ms.

### Stage table

Probe rows give milliseconds a round on Silesia and on the dependency sources, `base` first. Extraction rows give the median with its range. Every sample is in [`probe-samples.txt`](probe-samples.txt) and the JSON folders beside this page.

| # | Experiment and hypothesis | Metric | Result | Verdict |
| --- | --- | --- | --- | --- |
| 1 | A 10-bit or 12-bit literal table fits the cache or the codes better than 11 bits | Probe | 10 bits 174.8 and 43.20, 12 bits 172.3 and 43.83, against 171.8 and 42.97 | Reverted |
| 2 | A 7-bit or 9-bit distance table beats 8 bits | Probe | 7 bits 172.5 and 43.31, 9 bits 171.4 and 43.01, against 171.8 and 42.97 | Reverted, 9 bits is inside the noise |
| 3 | Matches at distances 8 to 15 copy 8 bytes at a time inline | Probe | 176.0 and 43.71 against 171.9 and 43.00 | Reverted |
| 4 | Two margin compares a pass instead of four | Probe | 174.3 and 43.22 against 171.9 and 43.00 | Reverted, the bounds checks came back |
| 5 | A refill before every distance removes a branch | Probe | 173.6 and 43.39 against 172.2 and 43.03 | Reverted |
| 6 | The second 16-byte copy only for matches past 16 bytes | Probe | 175.4 and 46.43 against 172.2 and 42.99 | Reverted |
| 7 | Table entries that hold two literals shorten literal runs | Count of literal pairs that fit 11 bits | 311,255 of 8,075,538 pairs on Silesia, 3.9% | Rejected before building it |
| 8 | The lookup after a match, made before the match is copied, starts the next symbol sooner after a branch miss | Probe | 169.8 and 42.50 against 172.1 and 43.03, ranges apart | Kept |
| 9 | The same lookup before the distance arithmetic too, or before a literal's store | Probe | 170.0 and 42.50, and 169.5 and 42.48, against 169.8 and 42.55 | Reverted, inside the noise |
| 10 | Runs of one byte filled in the out-of-line copy, or as 16-byte stores | Probe | 173.4 and 43.05, and 169.6 and 42.37, against 169.5 and 42.45 | Reverted |
| 11 | A window of 128 or 256 KiB keeps output in the first cache | Silesia and `large-random` at one thread | Silesia 233.6 and 221.2 ms against 218.4, `large-random` 87.5 and 81.4 ms against 75.7 | Reverted, more writes cost more than the cache saves |
| 12 | A 2 MiB window halves the writes again | The same | Silesia 215.9 ms (213.4 to 218.8) against 217.8 (215.6 to 224.2), `large-random` 72.0 (69.5 to 74.8) against 74.9 (72.9 to 78.3), peak 6.4 against 4.8 MiB | Reverted, ranges overlap and each worker grows by 1.5 MiB |
| 13 | Setting a large file's length before writing saves the file system work | The same | Silesia 220.0 ms against 217.3, `large-random` 71.9 (70.6 to 73.7) against 73.8 (72.4 to 75.9) | Reverted, ranges overlap |
| 14 | Writes that start and end on 16 KiB boundaries cost the kernel less | The same | `large-random` 86.7 ms against 77.8, Silesia 220.3 against 221.0 | Reverted |
| 15 | `Inflate::copy_stored` into a third 512 KiB of buffer, as the tar path does | `large-random` at one thread | 71.0 ms (68.9 to 516.0) against 76.5 (72.8 to 122.2), 399.8 M instructions against 437.8 | Reverted for stage 16, which needs no more memory |
| 16 | A read of stored blocks that fits the window's room leaves in one write, not two | Write calls and instructions on `large-random`, then wall time | 548 writes against 1,059 and 405.8 M instructions against 437.8. Wall time under Final rows | Kept on the counts |
| 17 | Passing stored bytes to the file from the input buffer, with no copy | Arithmetic from stage 11 | The copy takes 4.8 ms of 75. Stored blocks hold 64 KiB, so 4,097 writes would replace 548, at about 5 µs each | Rejected before building it |

Stage 8 does not show in wall time. Silesia at one thread took 216.1 ms (213.6 to 218.3) against 217.9 (215.4 to 224.8), and CPU time fell from 211.3 to 208.4 ms. On six archives the probe gives 0.988 of `base` on Silesia and the dependency sources, 0.974 on `mixed`, and no change on `grouped`, the flat tiny files and `large-compressible`.

Stage 16 comes from the write counts. At `base` the reader took 512 KiB of input into a window with 480 KiB of room beside its history. A read of stored blocks therefore left in two steps, of 480 and 32 KiB, and each step is a write. The reader now takes 480 KiB into a window of 544 KiB, and the decoder moves its history to the front before a stored read that would not fit.

### Final rows

`base` against `final`, median milliseconds with the range, CPU milliseconds and peak resident memory:

| ZIP | Threads | Rounds | `base` | `final` | CPU | Peak MiB |
| --- | --- | ---: | --- | --- | --- | --- |
| Silesia | 1 | 15 | 217.8 (215.0 to 222.0) | 215.5 (213.4 to 218.3) | 210.8, 208.5 | 4.8, 4.8 |
| Silesia | 4 | 15 | 80.1 (71.7 to 81.8) | 79.1 (71.1 to 81.5) | 234.4, 231.5 | 8.1, 8.1 |
| Silesia | default | 15 | 74.6 (71.9 to 131.1) | 74.4 (71.3 to 101.7) | 260.3, 255.3 | 16.5, 16.5 |
| Dependency sources | default | 11 | 286.1 (274.4 to 302.5) | 283.1 (271.5 to 299.2) | 1,097.7, 1,083.2 | 9.2, 9.1 |
| `grouped` | default | 11 | 167.5 (163.7 to 185.5) | 168.0 (163.0 to 182.7) | 625.4, 622.5 | 7.1, 7.1 |
| `large-compressible` | 1 | 11 | 72.0 (64.7 to 102.1) | 74.9 (65.1 to 91.0) | 55.8, 55.5 | 4.8, 4.8 |
| `large-random` | 1 | 21 | 81.0 (72.1 to 111.5) | 77.6 (69.9 to 148.0) | 64.1, 61.8 | 4.7, 4.7 |
| `large-random` | default | 15 | 46.9 (45.9 to 49.2) | 47.5 (46.6 to 81.5) | 74.1, 75.4 | 10.4, 10.3 |

Every pair of ranges overlaps. CPU time is lower on the Silesia rows by 1.1 to 1.9% and on `large-random` at one thread by 3.6%. At default threads `large-random` goes through the writer thread's 1 MiB chunks, so its write calls do not change. Three earlier sessions on `large-random` at one thread gave medians of 72.3 against 76.5 ms, 72.3 against 74.5 and 78.8 against 79.1, with CPU time lower by 2.2 to 5.7% each time. They are in `wall/stored3`, `wall/k2` and `wall/k2b`.

### Small entries and scheduling

No experiment was built for these two, because the counts leave nothing to win.

- A small entry's decode is 1 to 4% of its extraction's CPU time. The probe spends 30% of `grouped`'s 4.3 ms building tables and 13% clearing the 12 KB of table memory an entry allocates. Removing both would save about 2 ms of CPU in an extraction that takes 167.5 ms.
- At four threads Silesia extracts in 73 to 77 ms. Its largest entry, `mozilla`, extracts alone in 64.1 ms at one thread, and it starts first. Default threads give 74 ms. The archive waits for one entry, not for a worker.

A confirming run on the same quiet M2 Max on 12 November compared `main` at `e2c2843` with this pass rebased onto it, interleaved, every output checked byte for byte. The raw samples are in [`confirm-m2/`](confirm-m2/).

| ZIP | Threads | Runs | `main` median [range], ms | This pass, ms | Ratio | CPU ratio |
| --- | --- | ---: | --- | --- | ---: | ---: |
| `large-random` | 1 | 21 | 80.0 [70.3 to 165.8] | 73.7 [68.7 to 140.9] | 0.92 | 0.96 |
| Silesia | 1 | 11 | 222.4 [215.1 to 240.3] | 223.7 [214.5 to 335.4] | 1.01 | 1.00 |
| Silesia | 4 | 11 | 79.2 [71.3 to 82.9] | 75.4 [70.7 to 84.8] | 0.95 | 1.00 |
| Dependencies | default | 11 | 296.0 [280.6 to 305.4] | 289.8 [282.7 to 309.5] | 0.98 | 0.99 |

Every pair of ranges overlaps, and no row is slower beyond the spread.

## Decision

Kept stages 8 and 16.

- **Change.** In `fast`, the table lookup for the symbol after a match now comes before `copy_match`. `Inflate::decode` moves the last 32 KiB to the window's front at the start of a call when it is inside a stored block and the input offered is larger than the room left. `stream_entry` in `src/native_zip.rs` reads 480 KiB at a time, and a compile-time check keeps a read no larger than the window's room beside the history.
- **Tests.** `stored_blocks_take_one_decode_step_per_read` failed before the change with 16 steps for 9 reads. The zlib differential tests in `inflate` and the flate2 tests in `framing` cover the early move in windows of 64 to 96 KiB with inputs from 1 byte to the whole stream. A second new test copies the history back after stored data, as far as zlib's matches reach, in reads of eight sizes.
- **Plants.** `scripts/plant.py` ran three breaks on the second machine's copy of the tree. A decoder that never moves its history early failed the test with 16 steps for 9 reads. A reader back at 512 KiB failed it the same way before the compile-time check existed, and fails to compile with it. A fourth plant only loosened the compile-time check, which changes no behaviour, and the test passed as it should. After the review, a plant that flips the newest history byte at each early move failed `matches_copy_the_history_moved_before_a_stored_read`. That one ran on a shared, loaded MacBook.
- **Review.** An adversarial review confirmed nothing. It compared the decoder with zlib on 400 generated streams of stored, fixed and dynamic blocks in 12 windows and 30 piece sizes, with every block boundary cut, truncations and bit flips. It extracted 183 damaged ZIPs, none of which published a file, and found peak memory at 1 and 64 threads level with `736e11b` or below it. It noted that a break in one byte of the moved history passed the tests of the time. `matches_copy_the_history_moved_before_a_stored_read` now fails on that break.

## Limits

- One machine and one corpus. Nothing was measured on Linux, Windows or x86.
- The decoder moves its history early only inside a stored block. When stored blocks end exactly where reads end, as blocks of 32,763 bytes do, a read still leaves in two steps, as it did before.
- Stage 16 is kept on write calls and instructions. Its wall-time ranges overlap `base` in every session, and the machine's file services disturbed the last ones.
- The probe's build places code differently from the `zipper` binary. Stages 3 and 10 moved by 2% from a change that touches under 1% of the samples, so a probe difference under about 2% from one build can be placement. Stage 8 was built in three forms, and each gave 1.2 to 1.4%.
- Two ideas were left unbuilt. A writer thread for the largest entry once other workers go idle could save `mozilla` about 4 ms of 74, and it changes the modelled write-behind. Running the decode loop over the last 29 bytes of an entry's input would speed up small entries by a few percent of 1 to 4%.
- The bounds checks that remain in the decode loop cost instructions, not measured time. Removing them needs `unsafe`.

## Reproduce

```sh
mkdir -p probe/src && cp probe.rs probe/src/main.rs && cp probe.Cargo.toml probe/Cargo.toml
cp ../../../src/inflate.rs probe/src/inflate.rs
cargo build --release --manifest-path probe/Cargo.toml
cp probe/target/release/dprobe probe-final
python3 measure.py decode 3 524288 5 target/bench-work/silesia.zip base,final
```

`measure.py` runs `probe-NAME` from its own folder for each name. Build `probe-base` the same way from `src/inflate.rs` at `736e11b`. For the extraction rows, build `zipper` at `736e11b` and at this commit, then run each pair.

```sh
python3 scripts/compare_zip_readers.py target/bench-work/silesia.zip target/bench-corpora/silesia/silesia \
  --archive-format zip --runs 15 --work-dir "$PWD/target/bench-work" \
  --reader base native <base binary> --reader final native <final binary> \
  --reader-threads base 1 --reader-threads final 1
```

Count the writes with `cargo run --release --example count_zip --features counters,zip -- ARCHIVE 1`.
