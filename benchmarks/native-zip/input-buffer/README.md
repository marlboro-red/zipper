# Smaller ZIP input read-ahead

Cutting ZIP file-reader read-ahead from 256 KiB to 8 KiB made Linux streaming extraction 2.06× faster for flat files and 4.63× faster for grouped files, with lower memory. The change is kept, and Mac and Windows timings show no established gain.

## Question

The [File Activity trace](../file-activity/README.md) found roughly 4.72 GB of logical archive reads per extraction of a 3.4 MB tiny-file ZIP. The input reader used 256 KiB read-ahead buffers that entry and header seeks repeatedly discarded. Does smaller input read-ahead reduce redundant copying and memory without penalizing large streaming reads, which can bypass the buffer?

## Setup

The experiment changes only both file-reader buffer capacities, from 256 KiB to 8 KiB. Decoder and output buffers, checksums, scheduling and archive bytes are unchanged.

The Linux comparison uses `native`, so the retained native-fast archive cache does not mask this change. Each comparison runs seven interleaved measured rounds after warmup, with direct byte and tree checks after each timed extraction. These are warm and uncontrolled-cache results. The Linux runner has four CPUs, and the local Mac is the M5.

[Linux experiment 40893245837](https://github.com/marlboro-red/zipper/actions/runs/40893245837) uses baseline `f5b86b4` plus an isolated two-line candidate patch. The Mac baseline is `cc8aced` production code, identical in the affected path. Reports retain binary and archive hashes, commands, samples, memory and run order.

The validation [run 40897687606](https://github.com/marlboro-red/zipper/actions/runs/40897687606) built source `3f2c406` with the retained 8 KiB buffer. Its reference differs only by restoring both capacities to 256 KiB. It uses seven interleaved measured rounds and four workers, with byte and tree checks throughout. Linux fixtures contain 30,000 files, and the run checks that their ZIP sizes exceed the 8 MiB cache limit. Windows uses 1,000 files to keep repeated output creation practical.

## Results

### Small archives below the cache limit

| Workload / workers | Before ms [range] | After ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| Linux 10k flat / 1 | 714.65 [709.21–734.68] | 366.83 [363.76–367.96] | 7.01 / 6.48 |
| Linux 10k flat / 4 | 614.96 [612.60–618.26] | 298.44 [295.72–299.69] | 8.60 / 7.40 |
| Linux 10k grouped / 1 | 653.02 [647.14–661.05] | 299.34 [296.08–306.50] | 7.50 / 7.00 |
| Linux 10k grouped / 4 | 608.24 [601.60–619.42] | 131.29 [128.49–136.64] | 9.09 / 7.75 |
| M5 10k flat / 4 | 334.15 [326.25–357.38] | 334.58 [323.61–359.49] | 9.67 / 8.77 |
| M5 Silesia / 4 | 119.53 [118.46–167.74] | 123.45 [105.71–134.69] | 5.09 / 3.58 |

At four workers, Linux streaming extraction is 2.06× faster for flat files and 4.63× faster for grouped files, with lower memory. Local Mac extraction ranges overlap.

### Windows and archives above the cache limit

| Platform / shape / reader | 256 KiB ms [range] | 8 KiB ms [range] | Peak RSS before / after MiB |
| --- | ---: | ---: | ---: |
| Linux 30k flat / native | 1776.95 [1751.51–1861.12] | 907.99 [903.10–915.57] | 15.20 / 14.95 |
| Linux 30k flat / fast | 1798.30 [1740.83–1829.60] | 897.70 [890.43–909.79] | 15.23 / 14.95 |
| Linux 30k grouped / native | 1730.79 [1704.37–1760.16] | 404.17 [395.41–411.83] | 16.68 / 16.39 |
| Linux 30k grouped / fast | 1726.72 [1708.41–1760.34] | 371.70 [366.05–381.29] | 16.58 / 16.26 |
| Windows 1k flat / native | 477.08 [460.69–726.46] | 481.95 [472.74–565.33] | 6.48 / 5.35 |
| Windows 1k flat / fast | 556.19 [473.55–672.53] | 519.79 [478.30–821.03] | 6.48 / 5.24 |
| Windows 1k grouped / native | 307.65 [296.55–485.16] | 291.94 [276.39–389.67] | 6.65 / 5.46 |
| Windows 1k grouped / fast | 386.30 [287.63–598.51] | 291.11 [280.93–461.56] | 6.66 / 5.40 |

The Linux ZIPs are 8,753,795 bytes (flat) and 9,627,995 bytes (grouped), so neither uses the whole-archive cache. Above the cache limit, native-fast extraction improves **2.00× flat and 4.65× grouped**. The streaming reader improves 1.96× flat and 4.28× grouped. Windows ranges overlap.

## Decision

Keep the 8 KiB read-ahead. Claim no Mac timing gain. On Windows, retain the memory reduction but claim no established timing gain.

Archive cache and reduced read-ahead are alternatives on a given read path. **Do not multiply their measured speedups**.

Independent review confirmed both constructors, clone position accounting and boundary behavior. Reader tests and the full test suite pass.

## Limits

The original File Activity trace is baseline evidence. A second Instruments capture of the smaller-buffer prototype hit its time limit. It is not used as proof of changed I/O volume. The extraction timings above are uninstrumented.

Windows correctness is covered by CI, but no Windows performance gain is established. The small-archive test alone does not prove gains above the cache limit. The validation run measures that case separately. It tests the read-ahead change, not window-preserving seeks.

## Reproduce

Full commands, hashes, environment and samples for the validation run are in `validation/`. No other reproduction command was recorded.
