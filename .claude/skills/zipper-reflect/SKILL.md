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
| A commit message citing the hash of a commit a rebase rewrote, four times on 10 and 11 November, which `lint-commits` passed because the old object still existed locally | `cargo xtask lint-commits` and the `commit-msg` hook accept only a hash that is an ancestor of the citing commit, with `a_hash_off_the_citing_commits_history_is_reported` in `xtask/src/commits.rs` |
| Published numbers that their revision cannot reproduce | the `results` gate step rejects scoreboard JSON measured on a dirty tree |
| A hand-edited results page | the `results` gate step renders `docs/results.md` from the JSON and compares |
| A formal model that drifts from the code it describes | the `verify` gate step fingerprints the covered code in `verification/coverage.txt` |
| Two builders ran `pkill -f cbmc` and `pkill -f kani-driver` on 6 November, which killed another builder's proof run mid-harness | the `.claude/hooks/guard_destructive.py` hook refuses `killall`, and `pkill` unless `-f` names a path, with its cases in `.claude/hooks/test_guard_destructive.py` |
| Timings taken while builds, Kani or TLC from other sessions ran, twice on 5 November, and the scoreboard's own guard ignored `cargo` and `rustc` | one guard in `scripts/bench_common.py` that `compare_zip_readers.py`, `codec_scoreboard.py` and `scoreboard.py` share; it counts other sessions' builds and proof runs as load and refuses unless `--allow-busy`, which the JSON records |
| Two M22 parser changes that raised peak memory on hostile metadata, each found only by review | the Memory against the old code item in the `zipper-review` checklist, and rule 2 of `zipper-builder`, which asks builders for `/usr/bin/time -l` peaks against the old code |
| zsh pitfalls, an unsplit `$var`, an expanded glob and a missing `timeout`, three times in one session, once hiding a check that never ran | the shell note in `AGENTS.md`, since no check can see a loop that ran nothing; the TLA+ case is also caught by `verify-sync` running models itself |
| A planted break left in the code when a session restart killed `scripts/plant.py` mid-check, found only by the next builder | `plant.py` restores on SIGTERM and SIGHUP, journals the original bytes for a kill it cannot catch, refuses to plant again until `--restore`, and the gate's `verify` step fails while a journal entry exists |
| A TLA+ check recorded as passed when TLC never ran, after a shell left its arguments unsplit | each TLA+ section's `full-check` runs `scripts/check_model.py`, so `cargo xtask verify-sync` runs TLC itself and syncs nothing when a rule breaks |
| A planted-break helper that hid the failure or never applied the edit, and a planted break left in the release binary that was then measured | `scripts/plant.py` counts only a stated verdict, restores the file and rebuilds the binary, with its cases in `scripts/test_plant.py` run by the `hooks` gate step |
| A memory limit that missed an archive-controlled allocation: implied directories, link targets, queued writer chunks | one test per budget in `tests/hostile_tar.rs` and `src/native_tar.rs`, and the Budgets item in the `zipper-review` checklist |
| Windows behaviour first exercised by CI after a push, leaving `main` red until a fix commit: CRLF in a test archive, an older 7-Zip on Linux, a POSIX-only link size, a 7-Zip refusal | `cargo xtask ship` moves `main` only after CI passes on the exact commit |
| A `plant.py` verdict lost to its caller: a pipe closed by `head`, a check passed as one word with spaces | `scripts/plant.py` exits quietly on a closed pipe and refuses a check that cannot start, with both cases in `scripts/test_plant.py` |
| A coverage section whose code calls the shared field check without listing `field_check`, `BadField` and `Field`, found only by `kani-reach` after a Kani coverage run, four times by 8 November | the gate's `verify` step names the missing items before any proof runs, with its case in `xtask/src/verify.rs`; its first run found `[lzx]` and `[lzms]` |
| Rebases that conflicted on `verification/coverage.txt` every time, and once moved `[apfs-fields]` items into `[swf-fields]` because both sections end with the same lines | `scripts/merge_coverage.py`, a git merge driver set by `cargo xtask install-hooks`, merges one section at a time, with its cases and a real rebase in `scripts/test_merge_coverage.py` run by the `hooks` gate step |
| Builders' gate tests slowed two to three times by other builders sharing this machine | `cargo xtask gate fast --remote`, which rule 10 of `zipper-builder` asks builders to run |
| Scoreboard runs measured while another program loaded the machine | `scripts/scoreboard.py run` refuses to start while another process uses 25% CPU or more, unless `--allow-busy`, and records the list |
| A `cargo xtask` check piped into `tail` and followed by `&&`, which continued on the filter's status: a ship after a failed remote gate, and a record committed after failed lint, on 11 November | the `.claude/hooks/guard_destructive.py` hook refuses a piped `cargo xtask` check followed by `&&` unless `pipefail` is set, with its cases in `.claude/hooks/test_guard_destructive.py` |
| Tests that ran git in a scratch repository but inherited the hook's or the remote run's `GIT_DIR` and `GIT_WORK_TREE`, twice by 11 November | the gate's `cargo_command` removes `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE` from every cargo run, with `gate_steps_never_pass_a_hooks_git_location_to_the_tests` |
| A second machine without `7zz`, `unsquashfs` or the TLA+ runner, and two remote runs sharing one build folder at once, which hid or broke checks on 10 and 11 November | `remote_preflight` stops a `--remote` run when a tool is missing, with `a_remote_run_stops_when_the_second_machine_lacks_a_tool_the_gate_needs`, and `remote_lock` lets one remote run use the second machine at a time |
| 7-Zip comparison tests that skipped quietly under `ZIPPER_PARITY_REQUIRE_7Z`, since 17 test files looked up `7zz` themselves, so a second machine without 7-Zip passed the gate for days without running them | one locator, `sevenzip()` in `tests/common/sevenzip.rs`, panics when the flag is set and no 7-Zip of version 26.05 or newer is found, and the gate's `7-zip lookups` step, `cargo xtask lint-7zip-lookups`, fails on a lookup by name anywhere else in `tests/` |
| Raw timing JSON from the second machine committed with its home folder and checkout path in 87 files of 14 records, on 10 and 11 November | the gate's `private paths` step, `cargo xtask lint-private-paths`, fails on a tracked file that contains a `zipper.remote*` setting or the home folder inside one |
| Builders that waited for their own checks with `until`/`while` loops that sleep, one more each time they waited, 66 idle loops on 11 November | the `.claude/hooks/guard_destructive.py` hook refuses a command whose loop sleeps, with its cases in `.claude/hooks/test_guard_destructive.py`, and rule 11 of `zipper-builder` |
| A `cargo xtask` check followed by `;` and a commit, so a record was committed after `lint-docs` failed, on 11 November, the second form of a check whose failure did not stop the next step | the `.claude/hooks/guard_destructive.py` hook refuses a check followed by `;` and then a commit, push or ship, unless it is the condition of an `if` |
| `cargo xtask ship` stopping on a 502 from the GitHub jobs API, a DNS failure or a dropped connection, four times on 11 November, each retried by a hand-written loop | `ship` retries those errors with backoff and reads CI success from the run's recorded conclusion, not from `gh run watch`'s exit status, with `the_network_errors_seen_on_11_november_are_retried_and_a_failed_run_is_not` in `xtask/src/main.rs` |
