# Working on Zipper

## Goal and workflow

- Work autonomously. The user wants autonomous development.
- Make small, verified commits and push them to `origin/main`.
- Preserve remote history.
- Aim for competitive compression and extraction at comparable archive sizes. Do not gain throughput by discarding compression or correctness.

## Engineering approach

Follow [pstack's philosophy](https://github.com/cursor/plugins/tree/main/pstack):

- Understand the data shape and bottleneck before adding machinery. Prefer a smaller design that makes ownership and invariants explicit.
- Build rerunnable tools for verification. A successful build is not evidence of byte recovery. Test public behavior, malformed input, and failure cleanup.
- Pass direct original-versus-extracted byte checks on every archive change. Preserve v1 read compatibility and validate v2 selected-file extraction.
- Before you accept an optimization, state its hypothesis and metric.
- Measure release binaries with interleaved repeated runs and retain raw samples. Report median, range, archive size, memory, workload, and limitations.
- Profile outside timed runs. Treat overlapping timing ranges as inconclusive.
- Reject unproven performance complexity. Record kept and reverted experiments.
- Keep one change per experiment. Commit each verified stage and push it.
- Do not claim cross-machine or state-of-the-art leadership from one local corpus.
- Seek independent review for format parsing and concurrency changes. Ask reviewers to prove concrete failures where possible and to avoid editing the owner's files.

## Checks

- Treat a change as done only when `cargo xtask gate fast` passes. CI runs `cargo xtask gate full`.
- Run `cargo xtask install-hooks` once so pre-push runs the fast gate.
- After pushing, run `cargo xtask ci-wait`. It waits for the CI run on `HEAD` and prints each job's result.
- Expect the gate to format, lint, test, and build a release binary. On macOS and Linux it also lints the Windows build, which needs `rustup target add x86_64-pc-windows-gnu` once. It also checks a byte-exact round trip of `.zipper` and ZIP extraction, selected-path extraction, and truncated input cleanup.
- Use `scripts/compare_revisions.py` for direct byte-verified revision comparisons.
- Copy every commit hash you cite from `git log --oneline`. The `commit-msg` hook and `cargo xtask lint-commits` in pre-push reject a hash that names no commit.
- Delete only inside the session scratch directory or `target/`. The hook in `.claude/hooks/guard_destructive.py` blocks `rm -r` or `rm -f` anywhere else, an unguarded variable in an `rm` path, `git clean -f` and `git worktree remove --force`. Use a fresh `mktemp -d` folder per run and `${NAME:?}` in any path built from a variable.
- Run benchmarks on Unix. The benchmarks are Unix-oriented.
- Never edit `docs/results.md`. `cargo xtask bench` measures the fixed workloads and regenerates it, and the gate fails when it differs from `benchmarks/scoreboard/`.

## Comments

- Write no comment the code can carry. Rename, extract a function, or name a constant instead.
- A line comment starts with a keep-list tag. The tags are `SAFETY:`, `SPEC(<section>):`, `EXTERNAL(<system>):`, `RECORD(benchmarks/<record>):` and `LINK: <url>`. It runs at most 3 lines. `cargo xtask lint-comments` runs in the gate and checks the tag, the length and that a `RECORD` path exists.
- Doc comments are for public contracts, which are `pub` items of the library and clap help text.
- Before committing Rust changes, have the read-only [`zipper-comments`](.claude/agents/zipper-comments.md) agent judge the comments in the diff. It checks that each tag is true, which the lint cannot.

## Documentation

- Keep format contracts in [`docs/reference/format.md`](docs/reference/format.md).
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
| [`zipper-verify-tar-pool`](.claude/skills/zipper-verify-tar-pool/SKILL.md) | You change the tar writer pool, its budget or its queues in `src/native_tar.rs`. |
| [`zipper-verify-zip-write-behind`](.claude/skills/zipper-verify-zip-write-behind/SKILL.md) | You change `write_behind` or its buffer sizes in `src/native_zip.rs`. |
| [`zipper-verify-7z-workers`](.claude/skills/zipper-verify-7z-workers/SKILL.md) | You change the 7z folder workers, the decoder memory budget or the cancel checks in `src/native_7z.rs`. |
| [`zipper-verify-overwrite`](.claude/skills/zipper-verify-overwrite/SKILL.md) | You change extraction into an existing folder in `src/staging.rs` or the publish path. |
| [`zipper-verify-progress`](.claude/skills/zipper-verify-progress/SKILL.md) | You change `Tracker` in `src/progress.rs` or how a Rayon pool reports progress. |

The `zipper-review` skill spawns the read-only [`zipper-adversary`](.claude/agents/zipper-adversary.md) agent.
