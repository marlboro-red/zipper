# M3. Fixed scoreboard

One command measures every tracked workload the same way and publishes the numbers.

Scope:

- A new `bench` subcommand of `cargo xtask` runs fixed workloads. They are 10,000 flat tiny files, 4,000 grouped files, the dependency corpus, a Silesia ZIP, one large compressible entry, one large random entry and a mixed tree.
- Every run includes the parity rows, CPU time and peak memory.
- The command generates `docs/results.md` from the committed JSON.
- Instrumentation reports decoder CPU, bytes read and written, and output operations, outside timed runs. Done with the `counters` feature. Decoder CPU is reported as wall time inside decoder calls.

Exit checks:

1. `docs/results.md` is generated, and the gate fails when it does not match the committed JSON. Done in the `results` gate step.
2. Two sessions on one host record the session-to-session drift for each workload. Done on 2026-11-02 in the drift column of [Results](../results.md). Most rows moved less than 5%, and rows near 25 ms moved up to 11%.
3. One command reproduces every published number. `cargo xtask bench` regenerates every number in [Results](../results.md).
