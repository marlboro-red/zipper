# ZIP folders made in parallel before the workers start

Reverted. Making the ZIP reader's folders in parallel left the serial setup where it was on APFS. On the dependency sources the setup took 46.4 ms against 53.8 ms at four threads, with ranges overlapping, because on this file system making a folder does not get faster with more threads. Three tests of folder spellings that the experiment added are kept.

## Question

Can the folder setup that runs before the ZIP workers start run on several threads? Before the pool starts, `create_directories_in_archive_order` makes each folder in archive order. The [lock-free reads record](../lock-free-reads/README.md) measured that setup at about 20% of four-thread extraction on the dependency sources, which have 997 folders, so the setup is a fixed serial cost. The hypothesis was a smaller setup at four threads, with the first spelling of each folder still winning on a file system that ignores case and Unicode normalization.

## Setup

- This Mac, an Apple M5 with 10 cores, macOS 26.7.1, on 11 November 2026. Two other builders were running, with a load average of 7 to 10, so the numbers are counters inside one process, not timings.
- `before` is main at `90d1a27`. `after` is [`parallel-folders.patch`](parallel-folders.patch) applied to it.
- The patch makes each depth's folders in archive order on one task. It groups the folders below by a key that ignores ASCII case and trailing dots and spaces, and makes the groups in parallel, keeping archive order inside each group. A non-ASCII name, or one with `~`, keeps its level serial, since such names can match ASCII ones, as the Kelvin sign matches `k`. It stays serial on Windows.
- `examples/count_zip.rs`, built with `--features counters`, ran 15 interleaved rounds per build on `dependencies.zip`. It reports `setup_nanos` against `wall_nanos`.

## Results

Medians of 15 rounds, with the range:

| Build | Threads | Setup ms | Wall ms | Setup share |
| --- | --- | --- | --- | --- |
| `before` | 1 | 52.2 (44.4 to 75.2) | 560 (488 to 905) | 9.1% |
| `after` | 1 | 52.2 (44.4 to 93.6) | 533 (472 to 1,133) | 9.0% |
| `before` | 4 | 53.8 (45.0 to 78.7) | 371 (305 to 589) | 14.3% |
| `after` | 4 | 46.4 (40.5 to 69.1) | 364 (310 to 537) | 12.9% |

Every range overlaps. A probe showed all four threads making folders, 217 to 241 each, while the time from the first to the last of 993 creations stayed near 40 ms at both thread counts. The raw samples are in `counters.txt` beside this page.

## Decision

Reverted. The patch is kept beside this page and is not on main.

- **Tests kept.** `tests/hostile_zip.rs` gains a `folder_spellings` module. It compares 2,500 entries under folders spelled with different case and normalization against a reference that makes each folder in turn, at 1, 4 and 64 threads. It also checks that two files meeting under different spellings are refused, and that a folder that cannot be made leaves nothing. Planting a last-spelling-wins order and a wrong mode filter were each caught.
- **Review.** An adversarial review of the patch found nothing. It compared the old and new binaries over 1,300 random seeds, and peak memory at 99,990 folders fell from about 305 to 238 MiB.

## Limits

- One file system, under load. Linux ext4 may make folders in parallel faster, and that was not measured.
- Starting the file workers while folders are still being made was not tried. It would make workers wait on their parent folders and change the pass loop two TLA+ models cover, for a gain capped by the 13% setup share.

## Reproduce

Run `cargo build --release --features counters --example count_zip` at `90d1a27`, and again after `git apply benchmarks/native-zip/parallel-folders/parallel-folders.patch`. Run each binary as `target/release/examples/count_zip target/bench-work/dependencies.zip 4` fifteen times, alternating, and divide `setup_nanos` by `wall_nanos`.
