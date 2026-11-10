# M0. Verified workflow

The repository enforces its own rules.

- `cargo xtask gate fast` runs format, docs and comment lints, clippy, a Windows lint, tests, a release build and a byte-exact round trip. The pre-push hook runs it. Tests build with the `ci` profile, optimized without LTO and with overflow checks and debug assertions on. The release build keeps LTO.
- `cargo xtask gate full` adds the counters build and a benchmark smoke run. CI runs it on Linux.
- `cargo xtask lint-docs` checks writing rules, links, record headings, reachability and every path a page names.
- Benchmark scripts record CPU time and refuse comparisons without a 1-thread Zipper row.
- The skills in `.claude/skills/` cover benchmarking, review, documentation and reflection.
