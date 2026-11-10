# Zipper against the Go ZIP extractors pzip and fastzip

Kept as a record. Zipper extracts Silesia ZIP about 2.5 times faster than pzip and fastzip at four threads. On the three small-file workloads, `--content-only` beats both by 10 to 12%, and default Zipper ties pzip while doing far more checks. Neither Go tool checks what Zipper checks, so their small-file numbers come with less work done.

## Question

Are pzip and fastzip, two parallel ZIP tools written in Go, faster than Zipper at extraction, and does their design show a gain Zipper could take? A read of both extraction paths found nothing on the hot path that Zipper lacks. They use the pure-Go klauspost Deflate decoder, write 32 KiB at a time and take entries in archive order. The hypothesis was that Zipper wins on large entries and that the Go tools may win on small files by skipping work.

## Setup

- A second machine, an Apple M2 Max with 12 cores, macOS 27.0, on 11 November 2026. No build, Kani or TLC ran, and no timed run lists a busy process.
- Zipper is main at `dc60a40`, binary SHA-256 prefix `2283d2b452c667aa`. It ran with its defaults, and with `--content-only`, which writes contents and the execute bit only, as pzip does.
- pzip is `punzip` from `github.com/ybirader/pzip` at commit `9eb1349`, prefix `8645a7baeebf1249`. fastzip is `github.com/saracen/fastzip` at commit `75b2248` behind the driver in [`fastzip-driver/`](fastzip-driver/main.go), prefix `b127e2ddefbaf8cf`, because fastzip ships no command. Both read through `klauspost/compress`, and both were built with Go 1.24.6.
- The Go tools ran with `-concurrency N` and `GOMAXPROCS=N` at four threads, and with neither at their default, which is every core.
- The archives are the scoreboard's ZIPs of the dependency sources, the mixed tree, 10,000 flat tiny files and Silesia, all Deflate with no links or duplicate names.
- `scripts/compare_zip_readers.py --runs 7 --cooldown 2` ran each set after one warmup round, in shuffled order, and checked every extracted byte against the source tree. Both Go tools exit 0 on some damaged entries, so the byte check is the only verdict.

## Results

Median wall milliseconds with the range:

| Archive | Threads | Zipper | `--content-only` | pzip | fastzip |
| --- | --- | --- | --- | --- | --- |
| Silesia | 4 | 118 (115 to 127) | 126 (117 to 130) | 295 (293 to 300) | 290 (285 to 291) |
| Silesia | default | 118 (114 to 121) | 117 (114 to 121) | 262 (256 to 269) | 256 (252 to 264) |
| Dependency sources | 4 | 302 (294 to 316) | 266 (261 to 278) | 294 (276 to 310) | 377 (362 to 381) |
| Dependency sources | default | 362 (356 to 387) | 325 (318 to 341) | 347 (327 to 357) | 356 (342 to 372) |
| Mixed tree | 4 | 217 (213 to 221) | 192 (192 to 198) | 215 (211 to 220) | 275 (271 to 281) |
| Mixed tree | default | 243 (237 to 245) | 251 (241 to 261) | 250 (238 to 255) | 253 (247 to 259) |
| Flat tiny files | 4 | 427 (425 to 429) | 380 (370 to 394) | 430 (413 to 443) | 583 (575 to 587) |
| Flat tiny files | default | 430 (416 to 433) | 380 (377 to 387) | 561 (552 to 569) | 546 (540 to 551) |

CPU time at four threads on Silesia was 0.35 s for Zipper against 0.91 s for pzip and 0.89 s for fastzip. At their defaults on the flat tiny files, the Go tools used 4.51 s and 4.03 s of CPU against Zipper's 1.55 s. The raw JSON is beside this page.

## Decision

Kept as a record. No change to Zipper follows from the timing.

- **Large entries.** Zipper's zlib decoder and write-behind thread keep it 2.2 to 2.5 times ahead.
- **Small files.** Like for like, `--content-only` beats pzip by 10 to 12% on all three workloads, with ranges apart on two. Default Zipper also restores times and modes and still ties pzip at four threads.
- **Defaults.** The Go tools use every core and slow down on APFS, as Zipper did before its [small-files cap](../small-files-cap/README.md). The default rows here predate that cap.

The read of their code found three ideas for Zipper. One is to time the serial folder setup before the worker pool starts. The others are to detect merged folder spellings from `mkdir` results, and to read the archive without a lock.

## Limits

- One machine and four archives. The Go tools were built once with one Go version.
- Like for like, the comparison is not exact. The Go tools skip:
  - staging and the final rename, the free-space check and the local-header checks;
  - CRC checks on entries whose stored CRC is zero, and the check for bytes after a Deflate stream.

  Probes of crafted archives also showed pzip writing outside the destination for `../x`, fastzip planting a link outside it through two links, and fastzip mixing two entries that share a name.
- fastzip restores modes and times by path after each file, and owners when an archive names them. These archives carry no owner fields.

## Reproduce

Clone both tools, build `go build -trimpath -o punzip ./cmd/punzip` in pzip, and build the driver beside fastzip with its `go.mod`. Then, for each archive and for `4` and `auto`:

```sh
python3 scripts/compare_zip_readers.py target/bench-work/dependencies.zip \
  target/bench-corpora/dependencies/dependencies --archive-format zip --runs 7 --cooldown 2 \
  --work-dir "$PWD/target/bench-work" \
  --reader zipper native <zipper> --reader content-only native-content-only <zipper> \
  --reader pzip pzip <punzip> --reader fastzip fastzip <fastzip-extract> \
  --reader-threads zipper 4 --reader-threads content-only 4 \
  --reader-threads pzip 4 --reader-threads fastzip 4
```
