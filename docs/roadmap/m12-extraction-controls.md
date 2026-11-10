# M12. Extraction controls

The command line and the library offer the extraction choices 7-Zip offers.

Scope:

- Overwrite modes that skip existing files, rename new files or rename existing files, beside `Refuse` and `Overwrite`. They follow 7-Zip's `-aos`, `-aou` and `-aot`.
- Include and exclude patterns with wildcards, like 7-Zip's `-i` and `-x`, for every format. Tar selects entries while streaming.
- Extraction without folders, like `7z e`. Two archive files with one name follow the overwrite mode.
- Reading the archive from standard input and writing one file to standard output, like `-si` and `-so`.
- No option keeps files from a damaged archive. Decided on 4 November 2026, because it would end the rule that a damaged archive publishes nothing.

Exit checks:

1. The exhaustive overwrite check covers every overwrite mode.
2. Each control has a test on every native reader, and its output matches 7-Zip's on the M10 corpus.
3. `docs/usage.md` documents each control.
