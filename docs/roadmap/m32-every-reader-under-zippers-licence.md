# M32. Every reader under Zipper's licence

Every reader and decoder in Zipper is written from specifications, compatible code and observed behaviour, so all of it can ship under MIT OR Apache-2.0.

Zipper took the MIT OR Apache-2.0 licence on 12 November 2026. An audit that day compared each reader with 7-Zip 26.05's source. It found the FAT, MSI and DMG readers and the LZMS decoder derived from 7-Zip's LGPL code, and three test oracles that were ports of it. It found the APFS, NTFS, HFS and CHM readers, the LZX decoder and the Base64 sniffer likely derived, and small copied checks in the WIM, CAB, Quantum, MSLZ and LZH code. An unshipped NSIS reader was a port. The other readers read as independent, and the LZMA, xz and BCJ2 decoders follow the public-domain LZMA SDK.

Scope:

- Replace each derived or likely derived module with one written in a clean room. Its author never reads 7-Zip's source, the audit or the module it replaces, and works from specifications, BSD, MIT, Apache, zlib or public-domain code, and tools run as black boxes. The existing tests are the acceptance tests.
- Replace each ported test oracle with a run of `7zz` itself or an independent reference written from a specification.
- Rewrite the small copied checks in the modules that are otherwise independent.
- Write NSIS support fresh in the same way, before NSIS reads in process.

Exit checks:

1. Each replaced module passes its corpus, hostile and formal checks, and a differential run against `7zz` agrees as before.
2. A second audit, by an auditor who did not write the replacements, finds no module derived from GPL or LGPL code.
3. A new tag is cut only after the second audit.
