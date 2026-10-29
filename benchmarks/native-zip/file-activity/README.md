# File activity trace: tiny ZIP extraction

Apple M5, macOS 26.7.1, Instruments 27.1. Source `82d060d`, native-fast,
four workers, ten extractions of the 10,000-file flat fixture. The process exits
successfully and deletes its outputs only after all ten extractions. This is an
instrumented diagnostic, not a timed performance comparison. The trace contains
startup and cleanup; these are not extraction time.

| Scope / syscall | Calls | Summed thread duration, seconds | Logical bytes |
| --- | ---: | ---: | ---: |
| Archive read | 180,270 | 1.381 | 47,221,309,650 read |
| Archive seek | 180,200 | 0.071 | — |
| Output open | 100,000 | 8.003 | — |
| Output write | 80,000 | 1.436 | 95,360,000 written |
| Output close | 100,000 | 1.081 | — |
| Cleanup unlink | 100,000 | 3.211 | — |

Durations sum across threads and must not be added to infer elapsed extraction
time. Cleanup unlink is outside the extraction workload. The 20,000 empty output
files explain why write count is lower than open count. The exact output byte
count matches ten copies of the fixture.

The 3,383,629-byte ZIP generates approximately 4.72 GB of logical reads per
extraction, about 1,396 times its archive size. These are filesystem reads,
**not physical disk traffic**. The file reader has 256 KiB read-ahead buffers;
entry/header seeks discard buffered data. The counts identify input read
amplification as an architectural investigation target. They do not prove that
smaller buffers alone improve complete extraction on every OS. The retained
Linux cache removes these reads for eligible inputs; above 8 MiB, the file-reader
path still needs measurement. On this Mac output creation remains the largest
summed extraction syscall cost.

```sh
cargo build --release --example profile_zip
xctrace record --template 'File Activity' --output /tmp/zipper-file-activity.trace \
  --time-limit 15s --no-prompt --launch -- target/release/examples/profile_zip \
  /tmp/zipper-corpora/tiny-flat.zip native-fast 10 4
xctrace export --input /tmp/zipper-file-activity.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="FsSyscall"]' \
  --output /tmp/zipper-file-syscalls.xml
python3 scripts/summarize_file_trace.py /tmp/zipper-file-syscalls.xml \
  --archive-name tiny-flat.zip > summary.json
```

The summary omits filesystem paths and call stacks. The original trace remains
local at `/tmp/zipper-file-activity.trace`; raw tracing artifacts are too large
for the repository. The parser resolves Instruments XML references and groups
archive calls, generated output-file calls and other activity separately.
