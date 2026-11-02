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
| Comments that excuse code instead of reshaping it | `cargo xtask lint-comments` in the gate, and the `zipper-comments` agent |
| A hand-written CI watcher reading an empty run ID | `cargo xtask ci-wait`, which finds the run by full SHA and refuses an unpushed HEAD |
| `rm -rf` on paths built from unguarded variables, and deletions inside the repository | the `.claude/hooks/guard_destructive.py` PreToolUse hook |
| A relative `rm` after a `cd` to an empty variable, which runs in the home folder | the same hook, with its cases in `.claude/hooks/test_guard_destructive.py` run by the `hooks` gate step |
| Archive routing that trusted 7-Zip's format detection or parsed its output | the signature allowlist in `src/compat.rs` passes `-t`, never parses output and never falls back, with one test per bypass in `tests/cli.rs` |
| Verdict lines rewritten because the sentence lint ignored a period inside `**` | `sentences_end_inside_emphasis_and_brackets` in `xtask/src/docs.rs` |
| A commit message citing a hash typed from memory | `.githooks/commit-msg` and `cargo xtask lint-commits` in pre-push |
| Published numbers that their revision cannot reproduce | the `results` gate step rejects scoreboard JSON measured on a dirty tree |
| A hand-edited results page | the `results` gate step renders `docs/results.md` from the JSON and compares |
| A memory limit that missed an archive-controlled allocation: implied directories, link targets, queued writer chunks | one test per budget in `tests/hostile_tar.rs` and `src/native_tar.rs`, and the Budgets item in the `zipper-review` checklist |
| Scoreboard runs measured while another program loaded the machine | `scripts/scoreboard.py run` refuses to start while another process uses 25% CPU or more, unless `--allow-busy`, and records the list |
