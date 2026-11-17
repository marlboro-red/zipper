# M18. RAR

Zipper extracts RAR 4 and RAR 5 archives, including encrypted and multi-volume ones.

Scope:

- Zipper's own decoder, decided on 4 November 2026. The `unrar` licence forbids using its source to build a RAR compatible archiver. 7-Zip's RAR code derives from it and carries the same restriction.
- Its authors read neither the `unrar` source nor 7-Zip's RAR code. They work from RARLAB's RAR 5 technote, which covers the container, and from libarchive's BSD licensed RAR readers. Code that follows libarchive keeps its BSD notice.
- RAR 5 and RAR 4 containers, solid archives, and the RAR filters.
- RAR AES encryption and RAR volumes, with the M14 and M15 contracts.

Exit checks:

1. A corpus written by WinRAR extracts the same trees as 7-Zip and `unrar`.
2. Hostile tests and the adversarial review cover the container and each filter.

Done so far, by 14 November 2026:

- RAR 5 archives read in process with Zipper's own decoder since `437d581`, and RAR 4 archives that are not solid since `1aaed99`, as the [RAR reader reference](../reference/rar-reader.md) describes. `679a960` charges each RAR dictionary to the 2 GiB decoder budget. The `Rar` and `Rar5` rows of the [parity matrix](../reference/parity.md) read native.
- `tests/rar_corpus.rs` checks libarchive's test archives against `7zz x` where 7-Zip decodes them and against bsdtar where it does not. `tests/hostile_rar.rs` cuts and flips every corpus archive, builds hostile RAR 5 and RAR 4 headers, and reads the archives the adversarial reviews built in `tests/data/rar-hostile`.
- Exit check 1 holds for every archive RAR wrote but one, which the list below names. `scripts/make_rar_corpus.py` wrote 67 archives with RARLAB's `rar` 7.23, and with `rar` 6.24 for RAR 4, as the [corpus written by RAR](../reference/rar-reader.md#corpus-written-by-rar) describes. The user approved `rar` and `unrar` as black-box test tools on 14 November 2026. `rar` for macOS is the RAR engine WinRAR uses, but it writes the Unix host, so libarchive's corpus still covers archives written on Windows. `tests/rar_written.rs` checks 62 archives against the tree recorded from their source, in CI, with each volume set counted as one archive. On the second machine Zipper extracted all 62 to the tree `unrar` 7.23 extracts, and the stored ones to the tree 7-Zip extracts. The other 5 are refused by name, and `unrar` extracts each.
- Exit check 2 is met. The [review of the RAR 4 filters](../reference/rar-reader.md#review-of-the-rar-4-filters) attacked the container and each RAR 4 filter on its own, with E8, E8E9, delta, RGB, audio and Itanium. `tests/hostile_rar.rs` builds an archive for each filter, checks the ones that decode against bsdtar and refuses the ones out of bounds. The review's one failure was PPMd restarts spread over many files, and the restart allowance now covers the whole archive.

- RAR 5 and RAR 4 volume sets extract in process by 17 November 2026, under the M15 contract, as [Volumes](../reference/rar-reader.md#volumes) describes. Zipper reads `name.partN.rar` sets and old RAR 4 sets named `name.rar`, `name.r00` and on, from the first volume. Entries split across volumes may be stored, compressed, solid or RAR 5 encrypted, and each part's checksum and the whole file's are checked. A missing, cut, reordered or foreign volume and a volume flag that disagrees with its file are refused, the error names the volume, and nothing is published. A set holds at most 65,536 volumes, and at most three are open at once. The corpus written by RAR has 9 volume sets, and `rar5-volumes` and `rar4-volumes` moved from refused to extracted. Every set extracts the tree `unrar` 7.23 extracts.
- RAR 5 AES-256 decrypts in process by 17 November 2026, for data written with `-p` and headers written with `-hp`, in stored, compressed and solid archives, under the M14 contract. [Encryption](../reference/rar-reader.md#encryption) gives what the technote leaves out, which runs of `rar` and `unrar` as black boxes settled. The 10 encrypted RAR 5 archives in the corpus extract with their password, and a wrong, empty or missing password is refused and leaves nothing. `tests/hostile_rar.rs` refuses cut encryption records, KDF counts above 24, bad check values and data that is not whole AES blocks.

Left, on 17 November 2026:

- RAR writes an archive the shared PPMd allowance refuses. RAR 6.24 with `-mcT+` gave each of 120 text files of 200 to 900 bytes its own 25 MiB model of order 8. That is 3 GiB of models for 66 KB of output, and Zipper refuses `rar4-text-ppmd-small-files.rar` while `unrar` extracts it. `rar` chose LZ for the same files without `-mcT+`. The allowance exists because `ppmd-rust` fills the whole memory of each new model with zeros, so a model costs its size however little it decodes. A fix needs a model start whose cost follows the bytes it decodes, such as one that keeps the memory of the model before it. Until then exit check 1 has this one difference from `unrar`.
- RAR 4 encryption and RAR 4 solid archives are not read. They went to 7-Zip until [M20](m20-zipper-without-7-zip.md) removed the backend, and the RAR reader now refuses each by name. The scope above lists RAR 4 encryption as a goal, so M18 stays open.
