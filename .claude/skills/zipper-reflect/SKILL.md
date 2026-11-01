---
name: zipper-reflect
description: Turn what a stretch of work taught into enforcement. Use when a milestone closes, after work that needed repeated corrections, or when the same mistake appears twice.
---

# Zipper reflect

A rule with nothing enforcing it is a wish. Reflection finds repeated mistakes and moves each one into the strongest place that prevents it.

1. Gather evidence. Read the commits since the last reflection (`git log`), the experiment log in `benchmarks/README.md`, reverted changes, and the corrections the user gave.
2. Group mistakes into classes. A class needs at least two instances. One mistake is an incident.
3. For each class, choose the strongest encoding that works, in this order:
   1. A design that makes the mistake impossible.
   2. A type or an API that rejects it.
   3. A check in `cargo xtask gate`, a script guard, or a lint in `cargo xtask lint-docs`.
   4. A test.
   5. A skill or `AGENTS.md` edit, only for judgment that no check can make.
4. Prove each new check. Plant one instance of the mistake and show the check fails. Remove it and show the check passes.
5. List each skill under `.claude/skills/` and name when it was last used. Delete or merge a skill nobody used, or name the work that will use it next.
6. Commit the encodings with the evidence in the message.

## Lessons already encoded

| Mistake | Enforced by |
| --- | --- |
| Pushing code without the checks | `.githooks/pre-push` runs `cargo xtask gate fast` |
| A broken byte round trip | the release round trip in `cargo xtask gate fast` |
| Unreadable or unreachable docs | `cargo xtask lint-docs` |
| Experiment records in different shapes | record headings in `cargo xtask lint-docs` |
| Four Zipper workers against single-process tools | `scripts/extraction_matrix.py` refuses external readers without `--threads 1` |
| Wall time reported without CPU time | the benchmark smoke step in `cargo xtask gate full` |
| Facts lost when rewriting docs | `scripts/factcheck_docs.py` |
| A stricter ZIP check rejecting real archives | `tests/zip_corpus.rs` on archives from 5 writers, including a nested streaming ZIP |
| A hostile ZIP layout accepted | `tests/hostile_zip.rs`, one test per layout, and `tests/zip_fuzz.rs` |
| A planted break that never applied | the `zipper-review` skill, step 5 |
| A reviewer brief rebuilt by hand, missing a rule | the `zipper-adversary` agent in `.claude/agents/` |
| Code that compiles only on the host OS, failing Windows CI | the `windows clippy` step in `cargo xtask gate fast` |
