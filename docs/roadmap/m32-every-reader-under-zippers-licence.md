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
3. A second audit, by an auditor who did not write the change, finds no module derived from GPL or LGPL code outside M33's list. The second audit on 12 November 2026 confirmed the removal and found the disk-image reader derived, which is removed in turn. Its smaller findings joined M33.
4. A new tag is cut only after the second audit.

Done so far, by 12 November 2026:

- The FAT reader is gone. So are the MSI and compound file reader, the DMG reader with its ADC, LZFSE and LZVN decoders, XML property list and resource fork code, and the HFS+, HFSX and Apple partition map reader.
- The APFS reader, the NTFS reader with its LZNT1 decoder, the CHM and Help 2 reader, the LZX and LZMS decoders and the Base64 reader are gone too. So is the macOS extended attribute restore that only APFS and HFS+ used.
- The Cargo features `apfs`, `chm`, `dmg`, `msi` and `ntfs` are gone, with their tests, corpora, coverage sections, reader references and the test writers only they used.
- Every engine refuses each format and method that the [refused formats page](../reference/refused.md) lists, with an error that names it and nothing written, as `tests/refused.rs` checks. There, `no_refused_format_reaches_a_fake_7zip_on_path` checks that no refused file starts 7-Zip, whether `ZIPPER_7Z` names it or `PATH` finds it.
- 7-Zip now starts only with a `-t` switch that names a format Zipper hands to it on purpose. `--zip-engine backend` names the format from a signature at a fixed place, or Intel HEX from its records, and refuses any other file and every CAB, WIM, DMG or disk image. A split or pre-1.13 WIM and a disk image Zipper cannot read now fail instead of going to 7-Zip. RAR volumes, RAR 4 solid archives and RAR archives with encrypted headers still go to 7-Zip, named `-trar` or `-trar5`.
- CAB still reads stored, MSZIP and Quantum folders, and WIM still reads stored and XPRESS resources. Each refuses LZX and LZMS by the method's name.
- A disk image whose lone partition held FAT, NTFS, HFS+, HFSX or APFS was refused in every container until the disk image reader itself was removed, as the next item says.
- A second audit on 12 November 2026 found the disk image reader, with its MBR, GPT, VHD and VHDX code, a port of 7-Zip's LGPL handlers. The user decided that day to remove it, with its QCOW, VDI, VMDK and Android sparse containers, the `disk` feature, its tests, corpus, coverage sections and reference page. Every engine now refuses raw MBR and GPT disks and VHD, VHDX, QCOW, VDI, VMDK and Android sparse images by signature, with nothing written and no hand-off to 7-Zip, as `tests/refused.rs` checks. The ext, SquashFS and CramFS readers still read raw volumes. [M33](m33-clean-rewrites-of-the-copied-checks.md) tracks a clean rewrite.
- The parity matrix marks each removed row `refused` with a note, and `tests/parity.rs` refuses its corpus archives in every engine.

Exit checks 1 to 3 pass. Check 1 rests on `tests/refused.rs`, from `09bc152`, `e681602`, `de29b57` and `64f3dd1`. Check 2 rests on the `refused` rows those commits set. Check 3 is the audit recorded in `bc39092`, whose disk image finding `de29b57` removed. No tag has been cut since `v0.1.0-alpha.2`, which predates the audit. The NSIS drop and the tag remain. The NSIS reader never reached main, and its `Nsis` row still reads backend.
