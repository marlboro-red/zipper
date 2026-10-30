# Lazy ZIP streaming scratch experiment

Hypothesis: every Rayon task eagerly zeroes 256 KiB of streaming scratch even
when all its entries use whole-entry decoding or contain only tiny payloads.
Allocate scratch only on streaming fallback, growing on demand up to 256 KiB.
Keep at least one byte for empty entries so decoder EOF and CRC checks still run.
The scratch remains reusable within a task; no compressed/decoded budget change.

The patch is isolated from the direct-output experiment. Measure native and
native-fast end-to-end extraction and peak memory on identical immutable ZIPs,
four workers, seven interleaved release runs after warmup, direct original versus
extracted bytes/tree verification after every run. Small and large workloads
check both saved allocation and potential extra growth/copying cost.
