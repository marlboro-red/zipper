# M33. Clean rewrites of the copied checks

The few checks that the licence audit found copied from 7-Zip, inside readers that are otherwise Zipper's own, are written again in a clean room.

The audit of 12 November 2026 found these readers independent apart from small pieces. WIM has four validators, for the security data, stream entries, folder entries and chunk table. CAB has its item sort and equality rule, and Quantum its chunk driver's order of checks. MSLZ has its decode step, and LZH one window-size rule.

A second audit on 12 November 2026 found more small pieces that follow 7-Zip's source. They are the LZMA86 header and sniff, the SZDD output names, the XPRESS decode loop, and the El Torito boot-image names. The PPMd `.pmd` header, the FLV codec-name table, the PE string resources, and the Quantum model rescale and length table follow it too. So does a test oracle for the 7z AES properties. The user decided that day that these join M33 and do not hold back the next tag. The same audit found the disk-image reader derived. It was removed instead, and a clean rewrite of it is part of M33.

Scope:

- Rewrite each piece from the format's specification or observed behaviour, by an author who has not read 7-Zip's source or the audit.
- Keep each reader's tests, corpus and hostile checks as the acceptance tests.
- Write a new disk image reader for MBR and GPT disks and VHD, VHDX, QCOW, VDI, VMDK and Android sparse images, in a clean room from their specifications. The old reader was a port of 7-Zip's LGPL handlers and was removed on 12 November 2026. Until the rewrite lands, Zipper refuses these images, as the [refused formats page](../reference/refused.md#disk-images) describes.
- Replace the 7z AES test oracle with a comparison against `7zz` itself.

Exit checks:

1. Each rewritten piece passes the reader's corpus, hostile and formal checks.
2. The second audit of M32 finds none of these pieces derived.
