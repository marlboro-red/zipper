# M32. Every reader under Zipper's licence

Zipper ships only readers and decoders written from specifications, compatible code and observed behaviour, so all of it can ship under MIT OR Apache-2.0.

Zipper took the MIT OR Apache-2.0 licence on 12 November 2026. An audit that day compared each reader with 7-Zip 26.05's source. It found the FAT, MSI and DMG readers and the LZMS decoder derived from 7-Zip's LGPL code, and three test oracles that were ports of it. It found the APFS, NTFS, HFS and CHM readers, the LZX decoder and the Base64 sniffer likely derived. An unshipped NSIS reader was a port. Small copied checks in the WIM, CAB, Quantum, MSLZ and LZH code are left to [M33](m33-clean-rewrites-of-the-copied-checks.md). The other readers read as independent, and the LZMA, xz and BCJ2 decoders follow the public-domain LZMA SDK.

The user decided on 12 November 2026 to remove the derived and likely derived code rather than rewrite it. Parity with 7-Zip is not a goal while M32 is open.

Scope:

- Remove the FAT, MSI, DMG, APFS, NTFS, HFS and CHM readers, the LZMS and LZX decoders, the Base64 reader and the three ported test oracles, with their tests, features, checks and pages.
- Refuse those formats in every engine, with an error that names the format, and never hand them to 7-Zip.
- Refuse the methods and file systems that depended on them inside readers that stay, such as LZX in CAB and WIM, LZMS in WIM, and FAT, NTFS and HFS+ partitions in disk images.
- Drop the unshipped NSIS reader.

Exit checks:

1. A test refuses each removed format and method in every engine, writes nothing, and reaches no 7-Zip.
2. The parity matrix marks each removed row as refused for licence reasons.
3. A second audit, by an auditor who did not write the change, finds no module derived from GPL or LGPL code outside M33's list.
4. A new tag is cut only after the second audit.
