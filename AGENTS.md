# Working on Zipper

## Goal and workflow

- Work autonomously. The user wants autonomous development.
- Make small, verified commits and ship them to `origin/main` with `cargo xtask ship`.
- Preserve remote history.
- Aim for the fastest safe extraction of ZIP, tar and 7z. Do not gain throughput by discarding a safety check or correctness.
- Add no dependency that compiles or links C or another native library. A C library may be a dev-dependency used as a test oracle. The gate's `pure rust` step enforces this.
- Write every reader and decoder from format specifications, code under a compatible licence (BSD, MIT, Apache, zlib or public domain) and other tools run as black boxes. Never port or adapt 7-Zip's source under `CPP/`, which is LGPL, or any GPL or LGPL code. The LZMA SDK files under 7-Zip's `C/` are public domain and may be followed. Whoever reads such source to audit Zipper never writes the code that replaces it.
- Keep Zipper building on the `rust-version` in `Cargo.toml` with every dependency at the minimum `Cargo.toml` declares. Raise a minimum only when the code needs it. The full gate's `dependency minimums` step checks both.

## Engineering approach

Follow [pstack's philosophy](https://github.com/cursor/plugins/tree/main/pstack):

- Understand the data shape and bottleneck before adding machinery. Prefer a smaller design that makes ownership and invariants explicit.
- Build rerunnable tools for verification. A successful build is not evidence of byte recovery. Test public behavior, malformed input, and failure cleanup.
- Pass direct original-versus-extracted byte checks on every reader change, including selected-path extraction.
- Before you accept an optimization, state its hypothesis and metric.
- Measure release binaries with interleaved repeated runs and retain raw samples. Report median, range, archive size, memory, workload, and limitations.
- Profile outside timed runs. Treat overlapping timing ranges as inconclusive.
- Reject unproven performance complexity. Record kept and reverted experiments.
- Keep one change per experiment. Commit each verified stage and push it.
- Do not claim cross-machine or state-of-the-art leadership from one local corpus.
- Seek independent review for format parsing and concurrency changes. Ask reviewers to prove concrete failures where possible and to avoid editing the owner's files.

## Checks

- Treat a change as done only when `cargo xtask gate fast` passes. CI runs `cargo xtask gate full` as five parallel jobs, each started with `cargo xtask gate full --shard NAME`, for the shards `lints` and `builds` and for `tests-1`, `tests-2` and `tests-3`. Those three split the `tests` shard with nextest's `--partition hash:K/3`, so each test runs in one of them. `GATE_SHARDS` in `xtask/src/main.rs` puts each gate step in exactly one shard, and an xtask test fails when a step is in no shard or in two. `cargo xtask gate full` with no shard still runs every step. `cargo xtask gate fast --remote` runs the same gate on the second machine at the committed `HEAD`, set by the `git config` keys below, so this machine stays free.
- Run `cargo xtask install-hooks` once so pre-push runs the fast gate.
- Ship with `cargo xtask ship`. It refuses a dirty tree, runs `cargo xtask lint-commits`, and pushes `HEAD` to a `ci/` branch without the pre-push gate, since CI runs the stricter full gate on that commit. It then waits for CI on that exact commit and moves `main` only when the job of every shard passes, so `main` never receives a commit that CI failed. When a shard's job never gets a runner, `ship` runs only that shard on this host. Every other push still runs the pre-push gate. On the push to `main`, CI runs only `cargo xtask warm-cache`, which builds what the gate builds and saves the build cache that each `ci/` run restores. CI runs on Ubuntu only. The gate lints the Windows build on every push. Before shipping a change that can behave differently on Windows, invoke the `Windows check` workflow, which lints and runs every test with cargo-nextest under the `ci` profile. Before a release, invoke the `Windows release candidate` workflow, which tests the release build and packages it.
- CI installs no 7-Zip, so the tests that compare Zipper with `7zz` skip there. Every gate run outside CI sets `ZIPPER_PARITY_REQUIRE_7Z`, so on this machine and the second one those tests fail instead of skipping when no 7-Zip of version 26.05 or newer is installed. A difference that shows only with 7-Zip on Linux is no longer checked.
- `cargo xtask ci-wait` waits for the CI run on a pushed `HEAD` and prints each job's result.
- Before a release, run `cargo xtask release-checks`, or `cargo xtask release-checks --remote` to run it on the second machine. It runs the full checks listed in `verification/coverage.txt`, then the Kani list check, `cargo xtask format-tests` and `cargo xtask gate full`. The long exhaustive checks, such as link escape and overwrite, run only there and in `cargo xtask verify-sync`, never on a push.
- `release-checks` skips a section whose full checks already passed on the same code and the same check inputs. Its key covers the section's fingerprint, its full-check lines and the files they name, such as a model folder or a `--test` file. `verify-sync` and `release-checks` record that key in `verification/release-checks.txt`. Every other section runs, including each owed one, and its fingerprint is recorded when it passes. A changed section without a full check fails the run until its skill is followed and synced.
- Pass `--all` to rerun every section. The run refuses to skip anything, and asks for `--all`, when the channel in `rust-toolchain.toml`, `rust-version` in `Cargo.toml` or `cargo kani --version` differs from what the last passing `--all` run recorded. Commit `verification/coverage.txt` and `verification/release-checks.txt` after a run.
- `release-checks` runs sections at the same time, 3 by default or `--jobs N`. Each output line starts with its section's name in brackets, and a failure names its section and check. Kani checks on each worker after the first build in their own target folder, since Kani reads the newest files it wrote there. The `cargo xtask kani-reach` check of every section that runs goes first. They take minutes in all, and when one fails the run stops before any longer check. A `cargo test` or `cargo nextest` check that ran no test fails its section, here as in `verify-sync`.
- A remote run holds the second machine's one lock, the folder `target/remote-run.lock`, from start to end, so remote runs go one at a time. `release-checks --remote` holds it for the whole run, and its `--jobs` workers share that run's build folder, with Kani lanes in their own folders inside it. A waiting run removes the lock folder only when it is more than a day old, since a release run can hold it for hours.
- Expect the gate to format, lint, test, and build a release binary. It runs the tests with cargo-nextest, which runs every test in its own process and runs test binaries at the same time. Install it once with `cargo install cargo-nextest --locked`. On macOS and Linux it also lints the Windows build, which needs `rustup target add x86_64-pc-windows-gnu` once. It also checks byte-exact ZIP extraction, selected-path extraction, and truncated input cleanup.
- Use `scripts/compare_zip_readers.py` and `scripts/compare_workloads.py` for byte-verified comparisons of two binaries.
- Prove a check can fail with `scripts/plant.py`. It restores the file and rebuilds the release binary, so a planted break never reaches a measurement.
- Keep formal checks in step with their code. The gate's `verify` step fails when code listed in `verification/coverage.txt` changed since its check last passed. Follow the named `zipper-verify-*` skill, then run `cargo xtask verify-sync NAME`. With `--remote`, the check runs on a second machine at the committed `HEAD`. That machine is set by the local `git config` keys `zipper.remoteHost`, `zipper.remoteGit` and `zipper.remoteDir`, which are never committed. The fingerprint is recorded only when that machine checked the same covered code.
- A section marked `speed = slow` runs for more than about 5 minutes, such as a long Kani proof or the exhaustive overwrite check. When it goes stale, the gate prints it as owed instead of failing, so a change can ship on its tests, fast proofs and differential checks. Run the owed syncs in one batch before closing a milestone. `cargo xtask verify --owed` lists them, and `cargo xtask release-checks` runs and records each one.
- Prefer an exhaustive test to a slow Kani proof when the input space is small enough to enumerate.
- Copy every commit hash you cite from `git log --oneline`. The `commit-msg` hook and `cargo xtask lint-commits` in pre-push and in ship reject a hash that names no commit, or one that is not an ancestor of the commit citing it. After a rebase, copy the new hashes again.
- Delete only inside the session scratch directory or `target/`. The hook in `.claude/hooks/guard_destructive.py` blocks `rm -r` or `rm -f` anywhere else, an unguarded variable in an `rm` path, `git clean -f` and `git worktree remove --force`. Use a fresh `mktemp -d` folder per run and `${NAME:?}` in any path built from a variable.
- Run benchmarks on Unix. The benchmarks are Unix-oriented. `scripts/compare_zip_readers.py`, `scripts/codec_scoreboard.py` and `scripts/scoreboard.py` refuse to time while another process, such as a build, Kani or TLC, uses 25% CPU or more.
- The shell is zsh. It does not split an unquoted `$var` into words, so write `${=var}`, and it expands an unquoted `*` itself, so quote globs such as `--include='*.rs'`. macOS has no `timeout` command. A loop over a variable that fails this way runs nothing and can look like it passed.
- Never edit `docs/results.md`. `cargo xtask bench` measures the fixed workloads and regenerates it, and the gate fails when it differs from `benchmarks/scoreboard/`.

## Comments

- Write no comment the code can carry. Rename, extract a function, or name a constant instead.
- A line comment starts with a keep-list tag. The tags are `SAFETY:`, `SPEC(<section>):`, `EXTERNAL(<system>):`, `RECORD(benchmarks/<record>):` and `LINK: <url>`. It runs at most 3 lines. `cargo xtask lint-comments` runs in the gate and checks the tag, the length and that a `RECORD` path exists.
- Doc comments are for public contracts, which are `pub` items of the library and clap help text.
- Before committing Rust changes, have the read-only [`zipper-comments`](.claude/agents/zipper-comments.md) agent judge the comments in the diff. It checks that each tag is true, which the lint cannot.

## Documentation

- Keep each reader's contract in its page under [`docs/reference/`](docs/reference/zip-reader.md).
- Make every Markdown page reachable by links from `README.md`.
- Record each experiment in `benchmarks/` with the fixed headings, and add a row to the [experiment log](benchmarks/README.md).
- Write docs by the pstack `technical-writing` and `unslop` rules. `cargo xtask lint-docs` runs in the gate and checks dashes, colons, word lists, sentence length, links, record headings and reachability.

## Skills

Use these skills from `.claude/skills/` for the judgment that checks cannot make:

| Skill | Use it when |
| --- | --- |
| [`zipper-bench`](.claude/skills/zipper-bench/SKILL.md) | You measure, compare or publish a performance number. |
| [`zipper-review`](.claude/skills/zipper-review/SKILL.md) | You change archive parsing, extraction safety or concurrency. |
| [`zipper-docs`](.claude/skills/zipper-docs/SKILL.md) | You add, move or rewrite a Markdown page or record. |
| [`zipper-reflect`](.claude/skills/zipper-reflect/SKILL.md) | A milestone closes or the same mistake happens twice. |
| [`zipper-verify-tar-pool`](.claude/skills/zipper-verify-tar-pool/SKILL.md) | You change the writer pool, its budget or its queues in `src/writer_pool.rs`, or how `src/native_tar.rs` uses it. |
| [`zipper-verify-zip-write-behind`](.claude/skills/zipper-verify-zip-write-behind/SKILL.md) | You change `write_behind` or its buffer sizes in `src/native_zip.rs`. |
| [`zipper-verify-7z-workers`](.claude/skills/zipper-verify-7z-workers/SKILL.md) | You change the 7z folder workers, the decoder memory budget, the cancel checks or the hand-off of small files to the writer pool in `src/native_7z.rs`. |
| [`zipper-verify-overwrite`](.claude/skills/zipper-verify-overwrite/SKILL.md) | You change extraction into an existing folder in `src/staging.rs` or the publish path. |
| [`zipper-verify-zip-budget`](.claude/skills/zipper-verify-zip-budget/SKILL.md) | You change how ZIP workers reserve decoder memory, `Memory` in `src/memory.rs`, or the write-behind hand-off. |
| [`zipper-verify-links`](.claude/skills/zipper-verify-links/SKILL.md) | You change the link check in `src/staging.rs`, or how links are created or checked in `src/lib.rs`. |
| [`zipper-verify-progress`](.claude/skills/zipper-verify-progress/SKILL.md) | You change `Tracker` in `src/progress.rs` or how a Rayon pool reports progress. |
| [`zipper-verify-strip`](.claude/skills/zipper-verify-strip/SKILL.md) | You change how leading folders are stripped in `src/placement.rs`, or where a reader renames its entries for a strip. |
| [`zipper-verify-parsers`](.claude/skills/zipper-verify-parsers/SKILL.md) | You change the 7z header readers, the LZMA2 chunk scan, the ZIP time fields, end record or data descriptor, the tar numeric fields, or the Zstandard decoder in `src/zstandard.rs`. |

The `zipper-review` skill spawns the read-only [`zipper-adversary`](.claude/agents/zipper-adversary.md) agent.

To run work in parallel, spawn the [`zipper-builder`](.claude/agents/zipper-builder.md) agent once per unit, each in its own worktree, with a brief in the fields that file names. Builders never benchmark, push or ship. The coordinator reviews each branch, runs the measurements on a quiet machine and ships one branch at a time.
