# M32. Readers removed for licence reasons

Zipper holds no reader or decoder derived from 7-Zip's LGPL source, and it refuses the formats those readers read in every engine.

A licence audit found these readers and decoders derived, or likely derived, from 7-Zip's LGPL source. On 12 November 2026 the user decided to remove them rather than rewrite them, and to refuse their formats with no hand-off to 7-Zip. 7-Zip parity is no longer a goal until the user sets it again.

Removed:

- The FAT reader.
- The MSI and compound file reader.
- The DMG reader, with its ADC, LZFSE and LZVN decoders, its XML property list and resource fork code.
- The HFS+ and HFSX reader and the Apple partition map reader.
- The APFS reader.
- The NTFS reader, with its LZNT1 decoder.
- The CHM and Help 2 reader.
- The LZX decoder, used by CAB, WIM and CHM.
- The LZMS decoder, used by WIM and ESD.
- The Base64 reader.
- The macOS extended attribute restore that only the APFS and HFS+ readers used.

Their Cargo features `apfs`, `chm`, `dmg`, `msi` and `ntfs` are gone, with their tests, corpora, coverage sections and reader references. The test oracles ported from these readers went with them.

Exit checks:

1. Every engine refuses each format and method that the [refused formats page](../reference/refused.md) lists, with nothing written, as `tests/refused.rs` checks.
2. No refused file starts 7-Zip, named by `ZIPPER_7Z` or found on `PATH`, as `no_refused_format_reaches_a_fake_7zip_on_path` checks.
3. CAB still reads stored, MSZIP and Quantum folders, and WIM still reads stored and XPRESS resources. Each refuses LZX and LZMS by the method's name.
4. A disk image whose lone partition holds FAT, NTFS, HFS+, HFSX or APFS is refused in every container, as `tests/hostile_disk.rs` checks. SquashFS, CramFS and ext partitions still extract.
5. The parity matrix marks each removed row `refused` with a note, and `tests/parity.rs` refuses its corpus archives in every engine.

Done on 12 November 2026.
