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

Done so far, by 13 November 2026:

- RAR 5 archives read in process with Zipper's own decoder since `437d581`, and RAR 4 archives that are not solid since `1aaed99`, as the [RAR reader reference](../reference/rar-reader.md) describes. `679a960` charges each RAR dictionary to the 2 GiB decoder budget. The `Rar` and `Rar5` rows of the [parity matrix](../reference/parity.md) read native.
- `tests/rar_corpus.rs` checks libarchive's test archives against `7zz x` where 7-Zip decodes them and against bsdtar where it does not, since no RAR writer is installed here. `tests/hostile_rar.rs` cuts and flips every corpus archive, builds hostile RAR 5 and RAR 4 headers, and reads the archives the adversarial reviews built in `tests/data/rar-hostile`.

Left, on 13 November 2026:

- Exit check 1 is not met. No corpus written by WinRAR is in the repository, and no test compares with `unrar`.
- Exit check 2 is partly met. The hostile tests cover the container, and the review archives include filter cases. No recorded review names each RAR 4 filter on its own.
- RAR AES encryption, RAR volumes and RAR 4 solid archives are not read. They went to 7-Zip until [M20](m20-zipper-without-7-zip.md) removed the backend, and the RAR reader now refuses each by name.
