# Timestamp restore

The native ZIP engines now restore modification times like Info-ZIP `unzip` and bsdtar. That costs one system call per extracted file, about 3% of wall time on 10,000 tiny files and 11% on the dependency corpus. Listing is unchanged.

## Question

What does restoring file and directory modification times cost, and does it match the reference readers?

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds.
- Baseline is `eb9d175`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py` ran both binaries with `native` and `native-fast`, four threads, nine interleaved measured rounds after one warmup. Every round passed the byte and tree check.
- Workloads are 10,000 flat tiny files and the dependency corpus (3,981 files, 996 directories), both zipped by Python `zipfile`. Their entries carry only DOS times, so every time goes through the local-time conversion.
- The load average was between 2.6 and 3.3, so these numbers are noisier than earlier records.

Times come from the extended timestamp field (`0x5455`), then the older Info-ZIP Unix field (`0x5855`), then the DOS time read in the local time zone. The NTFS field is ignored, as both reference readers ignore it. On six archives from Info-ZIP, 7-Zip (with and without NTFS times), bsdtar, `ditto` and Python, both engines matched `unzip` and bsdtar for every file and directory.

## Results

| Workload | Reader | Wall ms, before | Wall ms, after | CPU ms, before | CPU ms, after |
| --- | --- | ---: | ---: | ---: | ---: |
| 10,000 flat files | native | 344.65 [326.49–472.06] | 354.16 [333.58–449.87] | 1147.2 | 1207.0 |
| 10,000 flat files | native-fast | 335.70 [325.87–390.10] | 356.30 [335.87–468.22] | 1145.0 | 1206.6 |
| Dependency sources | native | 181.15 [175.58–206.09] | 201.89 [194.69–233.86] | 560.3 | 635.2 |
| Dependency sources | native-fast | 172.22 [169.13–195.63] | 192.25 [185.60–255.91] | 534.3 | 609.4 |
| 10,000 flat files, `list` | native | 15.70 [15.32–23.67] | 15.70 [15.40–16.53] | 14.6 | 14.7 |

Values are medians with the observed range in brackets.

- **Files.** Each file gets one call to set its time after its last write. CPU time rose about 6 µs per file on both workloads. The flat-file wall-time ranges overlap. The dependency-corpus ranges are separate, at about 11%.
- **Directories.** A first version set 996 directory times one after another and cost about 20 ms more. Setting them in parallel removed most of that.
- **Listing.** A first version converted every DOS time with `mktime`, even for `list`, which made listing several times slower in the review's measurement. Now `list` skips times, and extraction caches the local offset per DOS date and hour.
- **Memory.** Peak RSS rose about 0.6 MiB on 10,000 entries, from the per-entry time fields.

An independent review found three problems, now fixed:

- A 1969 time stored in 32 bits read as 2106. Zipper now picks whichever reading, signed or unsigned, lies nearer the DOS time. That restores the true 1969 time, which `unzip` (1980) and bsdtar (2106) both miss.
- Invalid DOS fields, such as month 13, kept the extraction time. They are now normalized by `mktime`, which matches bsdtar.
- A failed time update aborted the extraction. Setting times is now best effort, as in both reference readers.

Tests now pin each rule. `tests/hostile_zip.rs` covers the 1969 and 2040 cases and the field priority. `tests/cli.rs` runs the CLI under `TZ=UTC`, `Asia/Tokyo` and `America/New_York` for DOS times, including daylight saving time. Planting an unsigned-only reading, a standard-time-only conversion, or the wrong field priority each fails one of them.

## Decision

Kept. Matching the reference readers requires one call per file, which bsdtar and `unzip` also make.

## Limits

- The machine was busy during measurement. Repeat on an idle machine before quoting the percentages.
- When central and local extended timestamps differ, Zipper uses the central one, while `unzip` and bsdtar use the local one. Only crafted archives do this.
- On Windows, directory times and DOS-only times are not restored.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py /tmp/perf/deps.zip /tmp/rust-dependencies \
  --reader before native zipper-before --reader after native zipper-after \
  --reader before-fast native-fast zipper-before --reader after-fast native-fast zipper-after \
  --runs 9 --threads 4 > dependencies.json
```

Build `zipper-before` at `eb9d175` and `zipper-after` at this commit.
