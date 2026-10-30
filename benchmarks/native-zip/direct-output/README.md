# Direct ZIP output experiment

Hypothesis: ZIP decoding already uses whole-entry or 256 KiB streaming output
buffers. Removing the additional 8 KiB BufWriter per file may avoid allocations
and copies without increasing the usual write-call count. A decoder returning
many short chunks could invalidate that assumption, so test both tiny-file and
large incompressible/compressible workloads, native and native-fast.

`candidate.patch` changes only the output wrapper. All CRC, decoded-size,
create-new, staging and error propagation remain required. The manual workflow
builds baseline and candidate from the same checkout, tests the candidate and
runs seven interleaved release rounds after warmup, with exact byte/tree checks.
Four workers, unchanged default budgets, identical ZIPs. It also captures Linux
syscall summaries separately from timed runs. The change is experimental until
end-to-end ranges establish a benefit without workload regressions.
