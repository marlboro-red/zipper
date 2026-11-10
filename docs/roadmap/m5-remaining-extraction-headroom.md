# M5. Remaining extraction headroom

Every scoreboard workload that is not at its measured limit gets faster, without unbounded memory.

Scope:

- Profile each slow path outside timed runs and name its limiter before changing code.
- The copy path of a large entry. Native takes 57 ms for the 256 MiB random entry, where bsdtar takes 43 ms.
- One-thread Deflate decoding. Native takes 239 ms on Silesia at one thread, where bsdtar takes 184 ms.
- The read amplification of tiny entries, which read 21 times the ZIP size on flat-tiny.
- Parallel decoding of a single Deflate entry above a size threshold, with streaming as the fallback.
- A memory-mapped input experiment, with a fallback when the archive changes during extraction.

Exit checks:

1. Each kept change beats the session drift on its scoreboard rows, with CPU time and peak memory reported.
2. The memory bound is documented and tested, and CRC checks still cover every byte.
3. Hostile Deflate streams pass the `zipper-review` skill.
