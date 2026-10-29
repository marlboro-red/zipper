# Reduce ZIP input read-ahead amplification

The [File Activity trace](../file-activity/README.md) found roughly 4.72 GB of
logical archive reads per extraction of a 3.4 MB tiny-file ZIP. The input reader
used 256 KiB read-ahead buffers that entry/header seeks repeatedly discarded.
Hypothesis: smaller input read-ahead reduces redundant copying and memory without
penalizing large streaming reads, which can bypass the buffer.

Experiment: change only both file-reader buffer capacities from 256 KiB to 8 KiB.
Decoder/output buffers, checksums, scheduling and archive bytes are unchanged.
The Linux comparison uses `native`, so the retained native-fast archive cache
does not mask this change. Seven interleaved measured rounds after warmup, direct
byte/tree checks after each timed extraction. These are warm/uncontrolled-cache
results; the Linux runner has four CPUs and the local Mac is the M5.

| Workload / workers | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| Linux 10k flat / 1 | 714.65 [709.21–734.68] | 366.83 [363.76–367.96] | 7.01 / 6.48 |
| Linux 10k flat / 4 | 614.96 [612.60–618.26] | 298.44 [295.72–299.69] | 8.60 / 7.40 |
| Linux 10k grouped / 1 | 653.02 [647.14–661.05] | 299.34 [296.08–306.50] | 7.50 / 7.00 |
| Linux 10k grouped / 4 | 608.24 [601.60–619.42] | 131.29 [128.49–136.64] | 9.09 / 7.75 |
| M5 10k flat / 4 | 334.15 [326.25–357.38] | 334.58 [323.61–359.49] | 9.67 / 8.77 |
| M5 Silesia / 4 | 119.53 [118.46–167.74] | 123.45 [105.71–134.69] | 5.09 / 3.58 |

Keep: Linux streaming extraction is 2.06× faster for flat files and 4.63× faster
for grouped files at four workers, with lower memory. Local Mac extraction ranges
overlap; claim no Mac timing gain. Windows correctness is covered by CI, but
this experiment does not yet establish a Windows performance gain. Nor does the
small-archive test prove large-archive gains above the cache limit. Archive cache
and reduced read-ahead are alternatives on a given read path: **do not multiply
their measured speedups**.

[Linux experiment 40893245837](https://github.com/marlboro-red/zipper/actions/runs/40893245837)
uses baseline `f5b86b4` plus an isolated two-line candidate patch. The Mac baseline
is `cc8aced` production code, identical in the affected path. Reports retain
binary/archive hashes, commands, samples, memory and run order. Independent review
confirmed both constructors, clone position accounting and boundary behavior;
reader tests and the full test suite pass.

The original File Activity trace is baseline evidence. A follow-up Instruments
capture of the smaller-buffer prototype hit its time limit and is not used as
proof of changed I/O volume. The extraction timings above are uninstrumented.
