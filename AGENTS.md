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
- Expect the gate to format, lint, test, and build a release binary. It also checks a byte-exact round trip of `.zipper` and ZIP extraction, selected-path extraction, and truncated input cleanup.
- Use `scripts/compare_revisions.py` for direct byte-verified revision comparisons.
- Run benchmarks on Unix. The benchmarks are Unix-oriented.

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
