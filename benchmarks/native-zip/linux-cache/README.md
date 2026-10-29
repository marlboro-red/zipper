# Linux small-ZIP extraction cache

The first portable matrix contradicted the local macOS bottleneck model. Linux
native extraction of 10,000 tiny files took much longer than creating their bytes
from RAM. Hypothesis: eliminating repeated shared file seeks/reads for small ZIPs
would reduce this gap. The experiment changes only cache eligibility: native-fast
Linux extraction can use the same bounded immutable archive cache already used
for integrity verification. No output scheduling or decoder change is included.

[Hosted experiment 40891611260](https://github.com/marlboro-red/zipper/actions/runs/40891611260)
built baseline `82d060d` and a candidate differing only in that condition. Both
already have the shared entry-buffer budget. Ubuntu 24.04 x86-64, four available
CPUs, warm/uncontrolled cache, seven interleaved rounds after warmup. Each timed
extraction is followed by direct byte/tree verification. CPU and filesystem
information, binary/archive hashes, samples and run order accompany this report.

| Shape / workers | Before ms [range] | After ms [range] | Speedup | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: | ---: |
| 10,000 flat / 1 | 526.43 [523.66–539.75] | 253.02 [251.39–254.43] | 2.08× | 7.08 / 9.94 |
| 10,000 flat / 4 | 443.90 [439.87–472.23] | 227.44 [226.51–227.86] | 1.95× | 8.79 / 10.83 |
| 10,000 grouped / 1 | 483.81 [476.26–497.71] | 201.79 [201.52–202.22] | 2.40× | 7.50 / 10.64 |
| 10,000 grouped / 4 | 445.67 [441.25–448.38] | 75.96 [74.96–77.49] | 5.87× | 9.14 / 11.52 |

Keep on Linux only. The result exceeds 3× for this grouped Linux fixture, **not**
for flat Linux files or the original local macOS target. Do not compare absolute
times with a different hosted job: hardware, storage and load can differ.
This is not yet a universal lead over other archivers.

Archives at most 8 MiB are loaded once after preflight and shared by immutable
cursors. Larger inputs use the prior file reader. This cache is additional to the
entry-buffer budget, including `--zip-buffer-mib 0`; loading/conversion can exceed
8 MiB transiently. Metadata consistency/CRC checks remain, but source files must
remain unchanged during extraction. Snapshotting does not detect mutations after
loading. Other platforms retain verification-only caching because extraction
benefit has not been demonstrated there.

GNU time now measures Linux child peak RSS. The workflow verifies it with a
128 MiB Python parent and both tiny and 64 MiB allocating children, preventing
the inherited parent high-water mark seen in the first portable matrix from
being presented as archiver memory. The wrapper's launch cost is included for
both sides. The reader comparators use a subprocess directly, not a pipeline.

Independent review found no safety blocker. Candidate release tests passed on
Linux, including cache boundaries, malformed archives, byte recovery, CRC/size
failures and staging cleanup. The production change has the same cache predicate;
comments and formatting differ. `.github/workflows/linux-cache.yml` retains the
original isolated experiment and intentionally fails its patch assertion if the
baseline already contains the change.
