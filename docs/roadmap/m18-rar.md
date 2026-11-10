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
