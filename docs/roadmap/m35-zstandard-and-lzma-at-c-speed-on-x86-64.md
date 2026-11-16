# M35. Zstandard and LZMA at C speed on x86-64

Zipper's Zstandard and LZMA decoders decode as fast as libzstd and liblzma on x86-64 Linux, so that both can become crates under [M31](m31-a-release-case-for-each-decoder.md)'s rule.

M31 found both decoders the fastest pure-Rust option in every run, and ahead of their C libraries on an Apple M2 Max. On x86-64 Linux they trail. Zstandard takes 1.34 times libzstd's time, and LZMA and xz take 1.09 to 1.20 times liblzma's, on both corpora, as the [Zstandard record](../../benchmarks/decoders/zstandard/README.md) and the [LZMA and xz record](../../benchmarks/decoders/lzma-and-xz/README.md) show. On 14 November 2026 the user marked both "not ready yet" rather than "stays inside Zipper", and asked for this milestone.

Scope:

- Profile each decoder on x86-64 Linux against its C library, outside timed runs, before any change. Name the limiter for each, such as the bit reader, the literal and match loops, the Huffman or FSE tables, the match copy or the window. A hosted runner or any x86-64 Linux machine will do, with `perf` where it runs.
- Compare code generation on x86-64 with the M2 Max, where Zipper leads, to find what differs.
- Check how the C side was compiled on each machine. [M34](m34-decoder-cases-on-windows.md) found Zipper level with libzstd and ahead of liblzma on Windows, where MSVC built them, so the Linux gap may belong to GCC's build or to the machine.
- Change only in safe Rust, as AGENTS.md and the pure-Rust rule require, with no assembly and no C. Keep every Kani proof, hostile test and differential test, and the M2 Max lead.
- Measure each kept change with the M31 driver, `scripts/m31_decoders.py`, on both corpora and both machines.

Exit checks:

1. A record for each decoder shows it level with or ahead of its C library on x86-64 Linux, with ranges apart or overlapping, on both corpora, with the M2 Max lead kept.
2. The M31 verdicts are re-judged with the new records. LZMA becoming a crate starts [M23](m23-the-lzma-decoder-as-its-own-crate.md).
3. If a gap cannot be closed in safe Rust, a record names the limiter with a profile, and the verdict stays "not ready yet" with that reason.

## Profile on x86-64

[`m35-profile.yml`](../../.github/workflows/m35-profile.yml) profiles each decoder against its C library on a hosted x86-64 Linux runner. It runs only when dispatched.

```sh
gh workflow run m35-profile.yml --ref main -f corpus=silesia -f codec=all -f target_cpu=baseline -f repeats=10
gh run list --workflow m35-profile.yml --limit 1
gh run download RUN_ID --name m35-profile-linux-x86_64-silesia-all-baseline --dir target/m35/linux-silesia
```

- `corpus` picks `silesia` or `dependencies`, and `codec` picks `zstd`, `xz`, `lzma` or `all`. Zstandard runs against libzstd, and xz and LZMA run against liblzma.
- `target_cpu` stays `baseline` by default, which builds with rustc 1.99.1 and no `RUSTFLAGS`, as the M31 runs did. `x86-64-v3` sets `RUSTFLAGS="-C target-cpu=x86-64-v3"`. It changes only the Rust code, since the `cc` crate builds libzstd and liblzma with its own flags.
- The workflow builds the binaries and the corpus with the same composite action as `m31-decoders.yml`, in [`.github/actions/m31-corpus`](../../.github/actions/m31-corpus/action.yml).
- It installs `perf` from the runner kernel's `linux-tools` package and sets `kernel.perf_event_paranoid` to -1 for the job.
- [`scripts/m35_profile.py`](../../scripts/m35_profile.py) runs `zipper-decode-bench repeat CODEC DECODER STREAM ORIGINAL COUNT` for each decoder. One process decodes the stream `repeats` times into one buffer and checks every byte of the last pass. A count of 0 runs only the setup.
- Each decoder runs under `perf stat` with the repeats and again with the setup alone. The script divides the difference by the repeats, so `summary.md` gives the cycles, instructions, branches, branch misses and L1 data cache load misses of one decode, with Zipper's count over the C library's.
- Each decoder then runs under `perf record -g`.

The artifact `m35-profile-linux-x86_64-CORPUS-CODEC-TARGET_CPU` holds these files, and uploads also when a step fails.

- `summary.md` and `profile.json`, with the counts of one decode.
- `stat-CODEC-DECODER-decode.csv` and `stat-CODEC-DECODER-setup.csv`, the raw `perf stat -x,` output.
- `report-CODEC-DECODER.txt`, from `perf report --no-children --percent-limit 1 --stdio`, for both decoders.
- `annotate-CODEC-zipper-1.txt` to `-3.txt`, from `perf annotate --stdio` on Zipper's three busiest user symbols.
- `machine.txt` with the commit, the inputs, `uname -a`, the `perf` and C compiler versions, plus `lscpu.txt` and `rustc.txt`.
- `corpus-source.json` and the streams' `corpus.json`, with the sha256 of the tar and of each stream.

A hosted runner may count no hardware events. `perf stat` then reads those rows as not counted, and `perf record` falls back to sampling the CPU clock. The profile names where the cycles go. It times nothing, and `scripts/m31_decoders.py` measures time.

### Time a change against its base on one runner

A hosted runner can get another CPU on each run and counts no hardware events. So a change to a decoder counts only when both builds run in one job. Set `base` to time Zipper at that ref, Zipper at the dispatched ref and the C library together.

```sh
gh workflow run m35-profile.yml --ref BRANCH -f corpus=silesia -f codec=all -f target_cpu=baseline -f base=main -f rounds=11 -f profile=false
gh run download RUN_ID --name m35-profile-linux-x86_64-silesia-all-baseline --dir target/m35/ab-silesia
```

- `base` takes a branch, a tag or a commit. The checkout then fetches the full history, and the job builds `zipper-decode-bench` at that commit in its own worktree and target folder. It uses the same rustc 1.99.1 and the same `target_cpu` flags as the dispatched ref.
- `profile=false` skips `perf` and the profile, so an A/B run stays short. It is refused when `base` is empty, and the default `true` runs both.
- `rounds` sets the timed rounds after one warmup, from 11 to 31.
- [`scripts/m35_ab.py`](../../scripts/m35_ab.py) runs the decode command of `scripts/m31_decoders.py` with both bench binaries. For each codec it times `zipper` from the dispatched ref, `zipper@base` from the base and libzstd or liblzma. Each sample is one fresh process, the order in each round comes from a fixed seed, and every decoded byte is checked against the tar.

The artifact then also holds these files.

- `ab.json` with every raw sample, the order of each round, both commits, the sha256 of both bench binaries and the runner's CPU.
- `ab.md` with each row's median and range in seconds, then the head's median time over the base's and over the C library's, and whether the ranges are apart. The job summary shows the same table.

Overlapping ranges mean no established difference, as the `zipper-bench` skill says. Compare rows only within one run.
