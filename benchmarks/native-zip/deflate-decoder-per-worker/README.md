# One Deflate decoder per ZIP worker

Reverted, with no measurable gain. Each ZIP worker kept one Deflate decoder across its entries, so it allocated the tables of dynamic Huffman blocks once instead of once per entry. On the dependency sources and on 10,000 flat tiny files, at default threads and at one thread, every timing range overlapped the old build's and peak memory stayed the same.

## Question

Does reusing one Deflate decoder per worker make ZIPs of many small Deflate entries extract faster? The idea came from the research report and the read of pzip and fastzip, as [M24](../../../docs/roadmap/m24-zip-and-7z-throughput-from-the-research-report.md) lists it. Zipper's own decoder in `src/inflate.rs` boxed its dynamic Huffman state, about 11 KiB of tables, once for each entry. The hypothesis was a lower wall time on archives of many small entries, and no change where they are large.

## Setup

- The second machine, an arm64 Mac with 12 cores on macOS 27.0, on 14 November 2026.
- `old` is main at `080ea38`. `new` is the code branch at `3730d5e`, whose source and test changes are in [`decoder-per-worker.patch`](decoder-per-worker.patch) beside this page.
- `new` parks each worker's decoder after an entry and resumes it on the next. Resuming calls `Inflate::restart`, which clears the window position, the bit buffer, the block state and the input count, so only the table storage carries over.
- `scripts/compare_zip_readers.py` ran 9 interleaved rounds after one warm-up, with both builds at default threads and at `--threads 1`, and checked every extracted byte. The dependency sources are 67,479,735 bytes in a 14,278,317-byte ZIP. The flat tiny files are 9,536,000 bytes in a 3,015,491-byte ZIP.
- The flat tiny run used `--allow-busy`, since fseventsd spikes after large extractions.

## Results

Medians of 9 rounds in ms, with the range:

| Workload | Threads | `old` | `new` |
| --- | --- | --- | --- |
| Dependency sources | default | 223.1 (201 to 235) | 223.7 (205 to 239) |
| Dependency sources | 1 | 434.7 (398 to 460) | 429.8 (405 to 449) |
| Flat tiny files | default | 314.0 (308 to 318) | 317.4 (313 to 324) |
| Flat tiny files | 1 | 590.8 (584 to 624) | 593.8 (587 to 613) |

Every range overlaps. Peak memory is the same for both builds in every row, 8.0 to 10.4 MiB. The raw samples are in [`data/`](data/), with the second machine's build folder written as `<target>`.

## Decision

Reverted. The extra decoder state is complexity without a measured gain, so the code is not on main.

- **Work done.** The code branch head is `3730d5e`. It added a differential test of one resumed decoder against fresh ones over every Deflate entry under `tests/data`, their cuts, flips and insertions, and damaged and random streams. Two extraction tests on one worker checked that an entry after a long one decodes as if alone, and that a match reaching into the entry before is refused. `cargo xtask gate fast --remote` passed at that head, and the `inflate`, `zip-crypto`, `zip-decoder-budget` and `progress-callback` sections were synced.
- **Plants.** Skipping the window reset in `restart`, skipping the bit buffer reset, and resuming without a restart were each caught.
- **Review.** An adversarial review found nothing. It ran 1,602 pairs of entries on one worker, each giving the verdict and bytes of its second entry alone, along with 250 zlib mutants and a mixed archive at 1, 2 and 4 threads.

## Limits

- One machine. Another CPU or allocator may weigh an 11 KiB allocation per entry differently.
- fseventsd used 42% of a core during the flat tiny run.
- The per-entry allocation was not profiled, so its share of an entry's time is unknown. The overlapping ranges bound it only by the noise of these runs.

## Reproduce

Build release binaries at `080ea38` and with `git apply benchmarks/native-zip/deflate-decoder-per-worker/decoder-per-worker.patch` on it. Run `scripts/compare_zip_readers.py` on each workload ZIP and its source with `--runs 9`, one `--reader` per build, and `--reader-threads` set to 1 for a second label of each build.
