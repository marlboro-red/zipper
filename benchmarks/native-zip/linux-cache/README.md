# Linux small-ZIP extraction cache

Letting native-fast Linux extraction use the bounded archive cache made 10,000-file extraction 1.95× to 5.87× faster. The change is kept on Linux only.

## Question

The first portable matrix contradicted the local macOS bottleneck model. Linux native extraction of 10,000 tiny files took much longer than creating their bytes from RAM. Does removing repeated shared file seeks and reads for small ZIPs reduce this gap?

## Setup

The experiment changes only cache eligibility. Native-fast Linux extraction can use the same bounded immutable archive cache already used for integrity verification. It includes no output scheduling or decoder change.

[Hosted experiment 40891611260](https://github.com/marlboro-red/zipper/actions/runs/40891611260) built baseline `82d060d` and a candidate differing only in that condition. Both already have the shared entry-buffer budget.

Ubuntu 24.04 x86-64, four available CPUs, a warm and uncontrolled cache, and seven interleaved rounds after warmup. Direct byte and tree verification follows each timed extraction. CPU and filesystem information, binary and archive hashes, samples and run order accompany this report.

GNU time now measures Linux child peak RSS. The workflow verifies this with a 128 MiB Python parent and both tiny and 64 MiB allocating children. The check stops the inherited parent high-water mark seen in the first portable matrix from being presented as archiver memory. The wrapper's launch cost is included for both sides. The reader comparators use a subprocess directly, not a pipeline.

## Results

| Shape / workers | Before ms [range] | After ms [range] | Speedup | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: | ---: |
| 10,000 flat / 1 | 526.43 [523.66–539.75] | 253.02 [251.39–254.43] | 2.08× | 7.08 / 9.94 |
| 10,000 flat / 4 | 443.90 [439.87–472.23] | 227.44 [226.51–227.86] | 1.95× | 8.79 / 10.83 |
| 10,000 grouped / 1 | 483.81 [476.26–497.71] | 201.79 [201.52–202.22] | 2.40× | 7.50 / 10.64 |
| 10,000 grouped / 4 | 445.67 [441.25–448.38] | 75.96 [74.96–77.49] | 5.87× | 9.14 / 11.52 |

The result exceeds 3× for this grouped Linux fixture. It does **not** exceed 3× for flat Linux files or the original local macOS target.

## Decision

Keep on Linux only. Other platforms retain verification-only caching because extraction benefit has not been demonstrated there.

Archives of at most 8 MiB are loaded once after preflight and shared by immutable cursors. Larger inputs use the prior file reader. This cache is in addition to the entry-buffer budget, including `--zip-buffer-mib 0`. Metadata consistency and CRC checks remain.

Independent review found no safety blocker. Candidate release tests passed on Linux. They cover cache boundaries, malformed archives, byte recovery, CRC and size failures, and staging cleanup. The production change has the same cache predicate, with different comments and formatting.

## Limits

Do not compare these absolute times with a different hosted job. Hardware, storage and load can differ. This is not yet a universal lead over other archivers.

Loading and conversion can exceed 8 MiB transiently. Source files must remain unchanged during extraction, because snapshotting does not detect mutations after loading.

## Reproduce

`.github/workflows/linux-cache.yml` retains the original isolated experiment. It intentionally fails its patch assertion if the baseline already contains the change.
