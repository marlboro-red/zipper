# LZMA probability tables read without bounds checks

**Reverted. Reading three groups of probability tables with proof-backed unchecked indexing cut 3.1% of retired instructions, but no commit was faster than its parent, and all three together made solid Silesia 0.4% faster.** That gain does not justify `unsafe` in a decoder of hostile input. One Apple M2 Max.

## Question

The [write path record](../../native-7z/write-path/README.md) and the [kernel record](../7zip-study-kernel/README.md) left 7-Zip about 10% faster at one thread, and safe-Rust changes had stopped paying. 7-Zip's decoder does no bounds checks. The Kani proofs of the LZMA groups already show that every tree, literal and control-bit index stays inside its table. Does dropping the checks at those sites, each backed by its proof, make the decoder faster? The metric is Zipper's median time on the probe in `src/lzma.rs`, with the ratio to liblzma in the same round. The second metric is the median wall time of `zipper extract` on `silesia.7z` at one thread, with 7-Zip in the same rotation.

## Setup

- Apple M2 Max with 12 cores and 64 GB, macOS 27.0. macOS background services were active, as in the kernel record.
- Four probe binaries, the `decode_speed_against_liblzma` test built at each commit, with SHA-256 prefixes:
  - `46a8df0`, the base, `1dbbe44f97bad255`;
  - `afe6498`, the special-distance reverse tree, `aa278c6b04f5ec76`;
  - `d9fda10`, the literal coder and the plain literal's last store, `1f891f0d338448e6`;
  - `4dbc85d`, the six control-bit tables by state, `5becbdde138c2b7b`.
- Nine rounds in rotating order, three probe rounds each, the first round warmup, leave 24 samples per binary.
- For extraction, release binaries at `46a8df0`, prefix `c05cb51aa9a4217a`, and `4dbc85d`, prefix `bc0dfe582c827830`. `scripts/compare_zip_readers.py --runs 9 --cooldown 3 --allow-busy` ran them with 7-Zip 26.05.
- Each unchecked site asserts its bound under Kani and debug builds. Its backing proofs ran, a planted out-of-range index was caught at each commit, and an adversarial review found no failure.

## Results

Times are in milliseconds for the probe and seconds for extraction. Raw samples are in [`samples-unchecked.json`](samples-unchecked.json) and [`7z-t1.json`](7z-t1.json).

| Binary | Retired instructions | Probe median and range | Ratio to liblzma |
| --- | ---: | --- | ---: |
| `46a8df0` | 4.342 G | 441.4, 439.8 to 443.7 | 0.874 |
| `afe6498` | 4.307 G | 441.7, 440.8 to 444.2 | 0.876 |
| `d9fda10` | 4.270 G | 439.5, 439.0 to 441.3 | 0.872 |
| `4dbc85d` | 4.209 G | 439.2, 438.1 to 440.7 | 0.871 |

| Binary | `silesia.7z` at one thread |
| --- | --- |
| `46a8df0` | 1.4836 (1.4817 to 1.4861) |
| `4dbc85d` | 1.4780 (1.4747 to 1.4802) |
| 7-Zip | 1.3345 (1.3327 to 1.3384) |

## Decision

Reverted. Each commit's probe range overlaps its parent's, so no single change is shown to help. All three together lie below the base in extraction, by 0.4%. The project keeps `unsafe` for proved needs, and 6 ms on 1.48 s does not earn three groups of unchecked reads in a parser of hostile data. The commits stay on the branch `lzma-unchecked`.

A fourth change, reading input bytes in `normalize` unchecked, saved another 98 M instructions. Kani could not finish the proof that bounds the bytes one symbol reads, so it was never committed.

## Limits

- One machine and one session, with background services active.
- 7-Zip remains about 10% faster at one thread. With the safe-Rust changes and proof-backed unchecked reads tried, the remaining lever is hand-written code for the hot loop, as 7-Zip uses.
- Bounds checks are well predicted branches, so their cost in time is far below their share of instructions, as this result shows.

## Reproduce

Build the `decode_speed_against_liblzma` test binary at the four commits with `cargo test --release --lib --no-run`, and copy each beside the kernel record's [`run-kernel.py`](../7zip-study-kernel/run-kernel.py) as `probe-<commit>`, with `shas` set to the four commits. For extraction, build release binaries at `46a8df0` and `4dbc85d` and run `scripts/compare_zip_readers.py` as Setup describes.
