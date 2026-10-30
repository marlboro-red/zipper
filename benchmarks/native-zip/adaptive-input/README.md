# Adaptive ZIP file read-ahead experiment

Hypothesis: the fixed 8 KiB buffer reduced tiny-file amplification, but large
streaming entries now issue many small physical reads. The ZIP crate wraps its
compressed input in a standard BufReader, whose requests can be 8 KiB. Increasing
read-ahead only after sustained sequential reads may recover large-entry speed
without reintroducing the original small-file read amplification.

The isolated candidate starts at 8 KiB, promotes to 256 KiB after 64 KiB of
sequential reads when the next request is at least 8 KiB, and resets to 8 KiB on
actual seek movement. A clone starts small at the logical position. Cached
immutable archives are unchanged. Buffer resizing preserves the unread logical
position; added tests compare bytes across growth, seeks, clones and invalid
seeks. This is a prototype until repeatable extraction improvements justify it.

`.github/workflows/zip-adaptive-input.yml` builds before/after binaries and on
Windows also compares the checksum-pinned actual alpha executable. All readers
use normal memory budgets and four workers. Seven interleaved measured rounds,
warmup excluded, with direct byte/tree verification after every extraction.
