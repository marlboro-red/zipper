# Working on Zipper

The user wants autonomous development, small verified commits, and pushes to
`origin/main`. Preserve remote history. The target is competitive compression and
extraction at comparable archive sizes, not throughput obtained by discarding
compression or correctness.

Use [pstack's philosophy](https://github.com/cursor/plugins/tree/main/pstack):

- Understand the data shape and bottleneck before adding machinery. Prefer a
  smaller design that makes ownership and invariants explicit.
- Build rerunnable tools for verification. A successful build is not evidence of
  byte recovery. Test public behavior, malformed input, and failure cleanup.
- Every archive change must pass direct original-versus-extracted byte checks.
  Preserve v1 read compatibility and validate v2 selected-file extraction.
- Before accepting an optimization, state its hypothesis and metric. Measure
  release binaries with interleaved repeated runs and retain raw samples. Report
  median, range, archive size, memory, workload, and limitations.
- Profile outside timed runs. Treat overlapping timing ranges as inconclusive.
  Reject unproven performance complexity; record kept and reverted experiments.
- Keep one change per experiment. Commit each verified stage and push it. Do not
  claim cross-machine or state-of-the-art leadership from one local corpus.
- Seek independent review for format parsing and concurrency changes. Reviewers
  must prove concrete failures where possible and avoid editing the owner's files.

Checks: `cargo fmt --check`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`, and a release CLI round trip.
Use `scripts/compare_revisions.py` for direct byte-verified revision comparisons.
Benchmarks are Unix-oriented. Format contracts live in `docs/format.md`.
