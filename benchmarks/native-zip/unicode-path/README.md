# Unicode path field check

Rejecting Info-ZIP Unicode path fields that rename an entry stops Zipper and other readers from extracting one entry under two names. Listing 10,000 entries took the same time before and after.

## Question

Does checking every Unicode path field (`0x7075`) against the stored name cost measurable listing time, and does it reject real archives?

Before this change, a local-only field that renamed `a.txt` to `renamed.txt` extracted as `a.txt`, while bsdtar honors the local field and names it differently. A central field was rejected only by accident, with a misleading "overlapping ZIP entries" error, because the `zip` crate renames entries from it. `tests/hostile_zip.rs` now covers central, local, directory, duplicate and short fields.

## Setup

- Apple M5, 10 cores, 32 GiB RAM, macOS 26.7.1. Release builds.
- Baseline is `5b4f3a3`. The candidate is that revision plus this change.
- `scripts/compare_zip_readers.py --operation list` ran both binaries with `native`, four threads, nine interleaved measured rounds after one warmup, on 10,000 flat tiny files zipped by Python `zipfile`.
- `preflight` now reads each central name instead of skipping it, which is the only new work on the listing path.

An independent review crafted archives with renaming, short, malformed, duplicate and bad-CRC fields, including fields on directories. It compared Zipper with the `zip` crate, Apple `unzip`, bsdtar, 7-Zip and Python, and found no bypass. Real archives with non-ASCII names from Info-ZIP, `ditto`, bsdtar, 7-Zip and Python carry no Unicode path field and extract byte for byte.

## Results

| Reader | List ms, before | List ms, after |
| --- | ---: | ---: |
| native | 15.37 [15.17–15.50] | 15.35 [15.15–15.54] |

Values are medians with the observed range in brackets. The ranges overlap, so no time difference is established.

## Decision

Kept. A field must name the entry exactly as its stored name does.

## Limits

- Archives that use the field as intended, such as Windows-era Info-ZIP or WinZip output with an OEM code-page name and a UTF-8 field, are rejected and left to the compatibility backend. None was available to test. The [roadmap](../../../docs/roadmap/m2-metadata-fidelity.md) tracks accepting them in M2.
- `list` reads only central records, so it does not see a field present only in a local header. `test` and `extract` reject it.
- One machine, warm cache.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py /tmp/perf/flat.zip /tmp/perf/flat --operation list \
  --reader before native zipper-before --reader after native zipper-after --runs 9 --threads 4 > flat-list.json
```

Build `zipper-before` at `5b4f3a3` and `zipper-after` at this commit.
