# ZIP decoder selection experiment

Hypothesis: whole-entry allocation, decoding, checksum and output can be slower
than bounded streaming for large, very compressible entries. Avoid choosing the
whole-entry path purely because it fits memory. On the local M5, five repeat
patterns at 16 MiB favor streaming, while two incompressible patterns favor
whole-entry decoding. The selection candidate streams extraction entries of at
least 16 MiB with compressed size at most 1/128 of expanded size. This is a
conservative measured region, not a claimed universal optimal threshold.

Only extraction policy changes. Integrity-only reads, metadata/CRC validation,
shared budget and incompressible entry selection stay unchanged. The patch is
isolated from scratch allocation and output-wrapper experiments.

The shape generator varies zeros, text, long runs and random repeat distances,
plus incompressible input. Seven interleaved release runs after warmup, four
workers and default budgets, identical input ZIP for each before/after pair;
direct original/extracted bytes and full tree checks after every run. Timings
include process startup and output; caches are warm and durability is not
requested. Cross-platform results must justify any retained platform policy.
