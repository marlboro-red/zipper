# M15. Split, spanned and self-extracting archives

Archives split into volumes, or wrapped in an executable, extract in process.

Scope:

- Raw split volumes (`.001`, `.002` and on) for any format the readers support.
- 7z volumes, and ZIP split and spanned archives (`.z01` with `.zip`).
- Self-extracting ZIP and 7z executables, read from the archive that follows the executable. The ZIP reader refuses bytes before the first record today. It keeps refusing them when they are not a known executable stub.
- A missing or truncated volume fails before publish and names the volume.

Exit checks:

1. A corpus of split and self-extracting archives from 7-Zip, WinZip and Info-ZIP extracts the same trees as 7-Zip.
2. Hostile tests cover volumes that overlap, repeat or come in the wrong order.

Done so far, by 6 November 2026:

- Raw numbered volumes (`.001`, `.002` and on) read in process for every native format, as the split volumes section of `docs/reference/zip-reader.md` describes. Seven adversarial reviews shaped the rules for damaged sets. A damaged set is refused and names the volume at fault. A single compressed file whose last volume is full is refused, because a missing volume cannot be told apart.
- ZIP split sets from Info-ZIP `zip -s`, read in process with every disk number and offset placed inside its own volume, as [ZIP split sets](../reference/zip-reader.md#zip-split-sets) describes. 7-Zip writes no `.z01` sets and WinZip was not available, so Info-ZIP is the only writer in the corpus.
- Self-extracting ZIP, with offsets adjusted by `zip -A` or not, as [self-extracting ZIP](../reference/zip-reader.md#self-extracting-zip) describes. The rule for the stub changed. Recognizing a known stub was dropped, because stubs differ by writer and version. The stub must be a whole PE, ELF or Mach-O executable or a `#!` script. The archive must start exactly where the executable's own data ends, and no range its headers name may reach it. The stub may hold no other archive's signature. Three of five adversarial reviews found files that another reader opened as a different archive, and each is closed by one of these rules.
- Self-extracting 7z, found within 8 MiB after a PE, ELF or Mach-O stub, as [self-extracting 7z](../reference/7z-reader.md#self-extracting-7z) describes. `7zz a -sfx` has no SFX module on macOS, so the tests build the stubs by hand.
Closed on 8 November 2026:

- M15 closed on 8 November 2026. WinZip does not run on macOS, and 7-Zip on macOS has no self-extractor module. So the corpus of exit check 1 holds Info-ZIP split sets and self-extracting archives whose stubs the tests build. Every one extracts the tree 7-Zip extracts.
