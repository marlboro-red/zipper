# M16. Package formats

Software packages extract in process on every operating system.

Scope:

- cpio in its binary, odc and newc forms.
- `ar`, Debian `.deb` and RPM, with their compressed payloads.
- Windows CAB with MSZIP, LZX and Quantum, and MSI through its compound file.
- `xar` and macOS `.pkg`.

Exit checks:

1. The M10 rows for these formats read native, with corpus archives from each format's own tools.
2. Each format has hostile tests for traversal, links, duplicate names and truncation.

On 12 November 2026 the LZX decoder and the MSI and compound file reader were removed for licence reasons, as [M32](m32-every-reader-under-zippers-licence.md) records. Zipper now refuses compound files and CAB folders compressed with LZX. The history below stays as it was.

Done so far, by 8 November 2026:

- cpio in its binary, odc and newc forms, `ar` in its System V, GNU and BSD forms, and Debian packages read in process, as `docs/reference/cpio-reader.md` and `docs/reference/ar-reader.md` describe. Six adversarial reviews found wrong-inode hard links, memory charges below real use and a second header pass that a rewritten archive could fool, each fixed with a test that failed first.
- xar archives and flat macOS packages read in process, as the [xar reader reference](../reference/xar-reader.md) describes. Real archives from `xar`, `pkgbuild` and `productbuild` extract to the trees `7zz x -txar` and `xar -x` write.
- CAB files with stored, MSZIP and LZX folders read in process, with Zipper's own LZX decoder, as `docs/reference/cab-reader.md` describes. Every corpus CAB and 400 generated ones extract as 7-Zip extracts them. Quantum folders decode in process with Zipper's own decoder.
- CAB cabinet sets read in process, with folders and data blocks joined across cabinets. 172 generated sets extract to their source files and as `7zz x -tcab` extracts them, and a missing, cut, misnumbered or inconsistent set is refused with the cabinet named. The CAB row of the parity matrix reads native, with a `gcab` cabinet and a generated three-cabinet set in the parity corpus. No CAB written by Microsoft's own tools is in either corpus.
- MSI, MSP and MSM installers and other compound files, such as `.doc`, `.xls` and `.msg`, read in process, as its reader reference described. An installer whose one CAB stream is a CAB extracts that CAB, as `7zz x` does. Every corpus file, 160 generated ones and 15 public installers and patches extract as 7-Zip extracts them.
Closed on 8 November 2026:

- M16 closed on 8 November 2026. Every M10 row for these formats reads native. The CAB corpus comes from `gcab` and Zipper's own writer, because no permissively licensed CAB from a Microsoft tool was found.
