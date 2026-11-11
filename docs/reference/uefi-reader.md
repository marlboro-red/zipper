# In-process UEFI reader

Zipper reads UEFI firmware volumes and UEFI capsules with its own Rust reader, in the tree `7zz x` writes. The default engine sends a file here when it starts with a firmware volume header or with a capsule header of the GUID `539182B9-ABB5-4391-B69A-E3A943F72FCC`. A file that no other reader claims also comes here when a volume header lies in its first MiB, as in firmware images that start with code. `--zip-engine native` also reads these files in process, and `--zip-engine backend` hands them to 7-Zip.

## Where the volume is

7-Zip reads one volume, and so does Zipper:

- A capsule of that GUID gives its header size and image size. The image lies inside the file and holds the header and the first volume whole. Zipper reads that first volume, as 7-Zip does, and leaves any volume after it. A capsule whose payload is not a volume writes its payload as one `[VOL]` file, named as below.
- Otherwise Zipper reads the first volume header in the file, from the start on, whose volume opens. 7-Zip searches a file it cannot open for an archive inside, and that search finds such a volume. So a capsule of another GUID, a capsule whose header does not hold, and an image with code before its volume all read the volume inside.
- When a volume fails one of the checks below, Zipper tries the next volume header in the file, as 7-Zip's search does, up to 16. A damaged section list in a file or a GUID-defined section ends the search instead, because 7-Zip opens such a volume and reports a header error.

## What it writes

A volume holds files, and a file holds sections. Zipper names them as 7-Zip does:

- A file is named by the first 8 hex digits of its GUID, such as `9E21FD93`. When the 8 digits repeat those of a file before it in the same volume, the file takes its whole GUID instead. A user interface section names the file instead, the last one when there are several.
- A raw file writes its body under its name. A pad file writes its body only when the body holds a byte other than `FF`, and is named as any file is.
- Every other file is a folder of its sections. A version section, a dependency expression and a user interface section write nothing when they read as one.
- A leaf section is named `N.KIND`, where `N` counts the leaf sections of its folder from 0. `KIND` is `efi` for PE32, `te`, `PIC`, `raw`, `COMPATIBILITY16`, `DXE_DEPEX`, `PEI_DEPEX`, `VERSION`, `USER_INTERFACE` or `FREEFORM_SUBTYPE_GUID`, or the section type in decimal for any other type, such as `28` for an MM dependency expression. A free-form subtype section of 16 bytes or more takes the 8 hex digits of its GUID and writes its data after the GUID.
- A GUID-defined section is a folder named by the 8 hex digits of its GUID, or by a user interface section inside it. An LZMA section is decoded first. A section of any other GUID, Tiano's included, is read as plain sections, as in 7-Zip. When its data starts past its header, the bytes between write `XXXXXXXX.prop` beside it.
- A CRC32 section and an uncompressed compression section are not folders. Their sections join the folder that holds them.
- A volume section writes its files. A volume of one file writes that file in place. A volume that is the only section of its folder writes its files into that folder. Any other volume is a folder named `VOLUME`.
- A volume section whose file system GUID is neither FFS1 nor FFS2, or whose signature is not `_FVH`, writes its bytes as `XXXXXXXX[VOL]`. The 8 hex digits come from its file system GUID, and a GUID of all `FF` writes `[VOL]` alone.
- A file or GUID-defined section with exactly one thing inside takes it into its own name, joined with a dot. So a driver with one PE32 section writes `Driver.efi`, and a file holding a volume of one file `F` writes `NAME.F`. A GUID-defined section joins its parent's name in no case.
- Bytes other than `FF` in a volume's free space write `[junk]`, from the first such byte to the end of the volume.

Two outputs of the same path write one, the last, as `7zz x -y` does. A user interface name becomes the UTF-8 bytes of its text with each byte read as one character, as 7-Zip writes it, so `é` becomes `Ã©`. A lone surrogate becomes U+FFFD first.

## Checks

Zipper refuses a file that 7-Zip refuses:

- A volume has the `_FVH` signature, the FFS1 or FFS2 file system and erased bytes of one. Its block map fills its header up to the terminator, its header checksum holds, and it lies inside the data that holds it. 7-Zip refuses FFS3 volumes and volumes that erase to zero.
- A file header sums to zero, and the file lies inside its volume. Its state sets the data valid bit and none of the bits for marked for update, deleted and header invalid. A file that asks for a data checksum matches it.
- A section lies inside its file and does not use the extended size form, which 7-Zip refuses. After a section that does not end its list, the padding to 4 bytes and a whole section header fit before the end. A GUID-defined section's data lies inside it, and a CRC32 section matches its data.
- An LZMA section gives its size, decodes to exactly that size and ends there, with an end marker or a clean range coder and no more input. A compression section that is compressed is refused, because 7-Zip does not decode EFI compression.
- A volume section holds at least 56 bytes.
- A user interface or version text ends with one zero character and holds no other. A dependency expression holds only opcodes 0 to 9, and each `BEFORE`, `AFTER` and `PUSH` has its 16-byte GUID. 7-Zip reads `END` as one more opcode. A section that fails these checks is written as a leaf.

## Safety checks

- The output goes to a hidden staging folder and is published only when the whole file reads, as with every reader.
- Every check holds before a byte is written. Each output file is charged against free space, or `--max-extracted-bytes`, before it is written.
- The image is read into memory once, at most 512 MiB. Decoded LZMA sections are held in memory too, at most 256 MiB in all. A section that declares more is refused before it is decoded, as one more failed volume. Output files are written from those buffers.
- Section lists nest at most 64 deep, as in 7-Zip. A file's sections sit two levels below its volume, and each GUID-defined, CRC32, compression and volume section adds one level.
- `--path`, `--include`, `--exclude`, `--strip-components` and `--strip-prefix` work as with every reader.

## Differences from 7-Zip

- A name that is not a plain relative path goes to 7-Zip under the default engine, and `--zip-engine native` refuses it. That covers a user interface name that holds `/..`, `\` or `:`, and an empty name.
- 7-Zip writes some of a file that fails a check after it finds the failure, and reports an error. Zipper writes nothing.
- An image larger than 512 MiB and decoded sections larger than 256 MiB in all are refused. 7-Zip reads them.
- 7-Zip searches the whole file for a volume and tries every one it finds. Zipper sniffs the first MiB of a file no other reader claims, and tries the first 16 volume headers.
- When a damaged section list lies inside an uncompressed compression or CRC32 section, 7-Zip sometimes reports a header error where Zipper searches on and reads a volume further in. The random sweeps found this in about 1 in 300 flipped volumes.
- A RISC-V QEMU firmware image starts with a jump that 7-Zip takes for an LZMA header, so 7-Zip opens it as an LZMA file and fails. Zipper reads its firmware volume.

## Limits

| Limit | Value |
| --- | --- |
| Image | at most 512 MiB |
| Decoded sections | at most 256 MiB in all |
| Nesting | at most 64 levels, as in 7-Zip |
| Volume headers tried | at most 16 |
| Files and folders | at most 1,000,000, the limit every tree reader shares |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Tests

`tests/uefi_corpus.rs` extracts the QEMU firmware images in `/opt/homebrew/share/qemu` where Homebrew's QEMU installed them. They hold PEI and DXE drivers, TE images, LZMA sections and nested volumes, and the ARM ones start with code before their volume. Each extracts byte for byte as `7zz x` extracts it, and 7-Zip and Zipper both refuse the variable stores. No EDK2 tool runs here, so `tests/common/uefi_writer.rs` writes volumes, files, sections and capsules from the UEFI PI format. The generated volumes cover every leaf section type, names, versions and dependency expressions, and every file kind, with pads and repeated names. They also cover volumes in files, GUID, CRC32, Tiano and compression sections, capsules, code before a volume and free space junk. Each extracts as `7zz x` extracts it.

`tests/hostile_uefi.rs` builds random volumes of files, nested volumes and compressed sections, flips bits in their bodies and cuts them. It cuts a small volume and a capsule at every length, and changes every volume and capsule header byte. Each result is refused when 7-Zip refuses it, and otherwise extracts as 7-Zip extracts it or is refused for a reason listed above. The full sweep of 10,200 cases runs under `--ignored`. Other tests refuse a section that declares more than the decode budget, and nesting past the limit. Kani proofs under `[uefi-fields]` in `verification/coverage.txt` cover the volume, file, section, GUID section, dependency expression and capsule header readers.
