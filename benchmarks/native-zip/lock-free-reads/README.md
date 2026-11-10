# ZIP workers read the archive without a lock

Kept. Every worker now reads a plain archive file by position, without the shared lock. A ZIP of 512 MiB of stored entries extracts 1.18 times faster at 64 threads, 104 ms against 123 ms, with ranges apart. Silesia and the dependency sources are unchanged, and the change removes code. The same session measured the serial folder setup at about 20% of four-thread extraction on the dependency sources.

## Question

Does the lock around archive reads serialize ZIP workers? `zip_reader::Cursor` held `Arc<Mutex<ArchiveFile>>`, so every read took the lock, though `ArchiveFile` already reads by position. The research report put archive reads at 1.1 to 1.4% of samples, and the Go tools pzip and fastzip read without a lock. The hypothesis was a gain on stored or fast-decoding entries at many threads, where reading is a larger share of the work, and none elsewhere.

A second question came from the same read of the Go tools. How much of extraction is the serial folder setup that runs before the workers start?

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- `main` is `8b2ae9d`, binary SHA-256 prefix `1bb002946cc58c82`. `nolock` is the change at `a9af867` on `8b2ae9d`, prefix `a53595c34d956d94`.
- `stored.zip` holds 8 entries of 64 MiB of random bytes, stored, 536.9 MB, written by Python's `zipfile`. Silesia and the dependency sources are the scoreboard ZIPs.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each pair after one warmup round, in shuffled order, and checked every extracted byte.
- For the setup, `examples/count_zip.rs` built with `--features counters` ran three times per row. It reports `setup_nanos`, the time inside `create_directories_in_archive_order` and `later_spellings_of_same_directory`, against `wall_nanos` for the whole read.

## Results

Median wall milliseconds with the range:

| Archive | Threads | `main` | `nolock` |
| --- | --- | --- | --- |
| Stored, 512 MiB | 64 | 123 (121 to 127) | 104 (99 to 116) |
| Stored, 512 MiB | 10 | 109 (107 to 113) | 99 (95 to 113) |
| Silesia | 64 | 114 (111 to 117) | 111 (107 to 113) |
| Silesia | 10 | 116 (112 to 122) | 117 (115 to 295) |
| Dependency sources | default | 334 (326 to 345) | 323 (317 to 330) |

Only the 64-thread stored row has ranges apart. One Silesia sample at 10 threads took 295 ms, and the medians there match.

The serial setup as a share of the whole read, three runs each:

| Archive | Folders | 1 thread | 4 threads |
| --- | ---: | --- | --- |
| Dependency sources | 997 | 14.9%, 10.7%, 10.6% | 19.8%, 19.0%, 19.9% |
| Mixed tree | 44 | 1.1%, 1.1%, 1.1% | 2.0%, 2.0%, 2.0% |
| Flat tiny files | 1 | 0.4%, 0.4%, 0.4% | 0.7%, 0.7%, 0.7% |

The raw JSON and `setup/counters.txt` are beside this page.

## Decision

Kept.

- **Change.** `Cursor` holds `Arc<ArchiveFile>` and reads with `read_at`. A split set still reads through `Joined`, which keeps its own lock around the open volume. A test extracts 240 stored and Deflate entries as a plain ZIP and as a split set at 1, 4, 16 and 64 threads, against the `zip` crate's bytes. Two planted read errors were each caught. An adversarial review found nothing.
- **Setup.** The counter stays, as `Counter::SetupNanos` under the `counters` feature. The setup is a fifth of four-thread extraction on a tree with about a thousand folders. Creating those folders in parallel, while the first spelling of each still wins on file systems that ignore case, is the next experiment.

## Limits

- One machine. The gain shows only with many threads on stored entries, which few real archives hold.
- Split sets still take turns on one lock and one open volume.
- The setup shares come from counters inside one process, not from interleaved timing.

## Reproduce

Build `8b2ae9d`, and `8b2ae9d` after `git cherry-pick a9af867`, with `cargo build --release`. Then:

```sh
python3 scripts/compare_zip_readers.py stored.zip stored --archive-format zip --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader main native <main binary> --reader nolock native <nolock binary> \
  --reader-threads main 64 --reader-threads nolock 64
```

For the setup, run `cargo build --release --features counters --example count_zip`, then `target/release/examples/count_zip target/bench-work/dependencies.zip 4` and divide `setup_nanos` by `wall_nanos`.
