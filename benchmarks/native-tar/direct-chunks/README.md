# Decoding tar data straight into writer chunks

**Kept. Decoding file data straight into recycled 1 MiB writer chunks makes the 256 MiB random gzip entry 1.11× faster and the compressible one 1.06× faster.** Silesia gains 1.01×. The large random entry's full ranges overlap, because each binary has one sample near 85 ms. The other ten samples of each are separate.

## Question

The scoreboard at `05233ac` had the default engine extracting the 256 MiB random `.tar.gz` in 53 ms, against 50 ms for default bsdtar, and the compressible one level at 63 against 62 ms. Each byte was decoded into a 1 MiB read buffer and then copied into a writer chunk. Does decoding into the chunk itself, and reusing 1 MiB chunks, remove enough of that copy to close the gap? The metric is the wall median against `05233ac` on the same archive.

## Setup

- Apple M5, 10 logical CPUs, macOS 26.7.1, with no other load. Release builds.
- `before` is `05233ac`, binary SHA-256 prefix `8f5a995e8f3f`. `after` is this change, prefix `4b74487b3d94`.
- Both use the default four writers and the default engine.
- After the measurement, review moved one step in `Chunk`'s drop, so a returned buffer reaches the spare list before its bytes are released. The committed binary differs from `after` only by that order.
- The fixtures are the scoreboard's ustar archives in gzip level 6 form, from `write_tar` and `write_gzip` in `scripts/extraction_matrix.py`.
- `scripts/compare_zip_readers.py --archive-format tar` ran 11 interleaved rounds after one warmup. Every round passed the byte and tree check.

## Results

Wall medians in milliseconds with the range, then median CPU milliseconds and peak MiB. Raw samples are in this folder.

| Archive | `before` | `after` |
| --- | ---: | ---: |
| 256 MiB random entry | 53.9 [53.5–83.6], CPU 73, 11.9 | 48.7 [48.2–86.4], CPU 65, 11.0 |
| 256 MiB compressible entry | 63.5 [63.3–66.4], CPU 80, 7.6 | 60.1 [59.8–63.1], CPU 77, 7.6 |
| Silesia | 189.0 [188.7–189.9], CPU 204, 10.9 | 186.3 [185.9–187.2], CPU 201, 9.0 |
| 10,000 flat tiny files | 352.1 [338.9–390.0], CPU 1,315, 6.7 | 350.2 [337.4–358.0], CPU 1,313, 6.8 |

The random entry's sorted samples were 53.5 to 56.6 ms and one 83.6 ms before, and 48.2 to 50.4 ms and one 86.4 ms after.

## Decision

Kept. The compressible entry and Silesia moved beyond their ranges, and the random entry did so in ten of eleven samples on each side. Flat tiny files did not change. CPU fell by 3 to 8 ms on the large rows, which fits one fewer memory copy of 256 MiB. The random entry is now at about the 50 ms default bsdtar took in the `05233ac` scoreboard, which is a different session.

## Limits

One Mac, one session, warm caches. bsdtar was not in this comparison. Linux and Windows were not measured.

## Reproduce

```sh
python3 scripts/compare_zip_readers.py large-random.tar.gz target/bench-corpora/large/large/random \
  --archive-format tar --runs 11 --threads 4 \
  --reader before native zipper-before --reader after native zipper-after \
  --reader-threads before auto --reader-threads after auto > large-random.tar.gz.json
```

Build `zipper-before` at `05233ac` and `zipper-after` at this commit. The fixtures come from `write_tar` and `write_gzip`, as in the [writer pool record](../writer-pool/README.md).
