# Formats Zipper refuses

Since 12 November 2026 Zipper refuses the formats and methods on this page in every engine. A licence audit found their readers derived, or likely derived, from 7-Zip's LGPL source, so the readers were removed, as [M32](../roadmap/m32-every-reader-under-zippers-licence.md) records. Zipper recognizes each one by its signature only and never decodes it. No engine hands such a file to 7-Zip.

## How a refusal behaves

- The default and native engines refuse the file where its reader used to be chosen.
- `--zip-engine backend` checks the file before 7-Zip starts. So does `zipper::compat::run`, the library's way to start 7-Zip.
- 7-Zip only ever starts with `-t` naming one format, so it never searches a file for another one. `--zip-engine backend` names the format from a signature at a fixed place, such as `7z`, `rar`, `zip`, `gzip`, `tar`, `iso` or `squashfs`. A file with none of these signatures is refused with `Zipper does not read a file through 7-Zip without naming its format as one Zipper hands to 7-Zip`. CAB, WIM, DMG and disk images are never among them.
- The default engine sends to 7-Zip only what its own reader checked first, such as a 7z with BCJ2, a Mach-O universal file as `-tmub`, or a SquashFS image of another version as `-tsquashfs`. A split or pre-1.13 WIM, and a disk image whose file system Zipper does not read, such as exFAT or a differencing image, are refused instead.
- The error is `zipper::Refused`. Its message is `Zipper does not read`, followed by the format's name.
- Nothing is written. The destination is never created.
- `extract`, `list` and `test` all refuse the same way.

## Formats

| Format | Recognized by | Message |
| --- | --- | --- |
| Compound files, such as `.msi`, `.msp`, `.doc` and `.xls` | `D0 CF 11 E0 A1 B1 1A E1` at offset 0 | `Zipper does not read compound files such as MSI, DOC and XLS` |
| CHM and Help 2 | `ITSF` or `ITOLITLS` at offset 0 | `Zipper does not read CHM and Help 2 files` |
| DMG | `koly` at offset 0 or 512 bytes before the end | `Zipper does not read DMG images` |
| HFS+ and HFSX | `H+` version 4 or `HX` version 5 at offset 1024, or an HFS wrapper `BD` at 1024 with `H+` at 1148 | `Zipper does not read HFS+ and HFSX volumes` |
| Apple partition maps | `ER` at offset 0 and `PM` at offset 512 | `Zipper does not read Apple partition maps` |
| APFS | `NXSB` at offset 32, a container superblock type at 24 and a block size of 4 KiB to 64 KiB at 36 | `Zipper does not read APFS containers` |
| NTFS | A boot sector with a jump, `NTFS    ` at offset 3 and `55 AA` at 510 | `Zipper does not read NTFS volumes` |
| FAT | A boot sector with a jump, `55 AA` at 510 and BPB fields every FAT volume holds | `Zipper does not read FAT volumes` |
| Base64 | A file named `*.b64` whose first 4 KiB hold only Base64 letters, `=` and spaces | `Zipper does not read Base64 files` |

A file with an ISO 9660 or UDF descriptor at offset `0x8001` is not refused by the backend check. Hybrid discs carry an Apple partition map or an HFS+ volume beside the disc. The default and native engines read them as discs, and `--zip-engine backend` hands them to 7-Zip as `-tiso` or `-tudf`, so 7-Zip reads only the disc.

## Methods

The CAB and WIM readers still read their other methods. They refuse a removed method by its name.

| Archive | Refused | Native message | Backend message |
| --- | --- | --- | --- |
| CAB | A folder compressed with LZX | `CAB folder is compressed with LZX, which Zipper does not decode` | `Zipper does not read CAB folders compressed with LZX` |
| WIM | An image compressed with LZX | `WIM image is compressed with LZX, which Zipper does not decode` | `Zipper does not read WIM images compressed with LZX` |
| WIM and ESD | An image or solid resource compressed with LZMS | `WIM image is compressed with LZMS, which Zipper does not decode` | `Zipper does not read WIM and ESD images compressed with LZMS` |

Every ESD file, and every solid WIM that wimlib writes with its default compression, compresses its metadata with LZMS, so each is refused. A CAB whose other folders are stored, MSZIP or Quantum is refused whole when one folder uses LZX. `--zip-engine backend` hands no CAB or WIM to 7-Zip. So no cabinet of a set, folder behind a large header reserve or solid resource of an unnamed method can reach it.

## Disk images

A disk image is refused whole when its lone partition or whole volume holds FAT, NTFS, HFS+, HFSX or APFS. That covers raw, MBR and GPT disks and VHD, VHDX, QCOW, VDI, VMDK and Android sparse images. A lone SquashFS, CramFS or ext partition still extracts.

A disk with several partitions still extracts each partition, and each gap between them, as one raw file, whatever the partition holds. No file system is decoded there, so nothing is extracted in part. A partition file such as `0.ntfs` is then refused when Zipper is asked to extract it.

`--zip-engine backend` hands no disk image to 7-Zip. The backend check names a refused file system in a disk image only in a build with the `disk` feature, and refuses the image either way.

## Proof

- `every_refused_format_is_refused_in_every_engine_with_nothing_written` in `tests/refused.rs` runs every format and method above through each engine. It uses the CLI's `extract`, `list` and `test`, `zipper::extract_archive`, `zipper::choose_reader` and `zipper::compat::run`.
- `no_refused_format_reaches_a_fake_7zip_on_path` in the same file puts a fake `7zz` first on `PATH` and checks that none of these files starts it, while another format does.
- `every_7zip_start_names_the_format_and_crafted_refused_files_never_open_in_process` runs files an adversarial review crafted through every engine. They hold a prefix before a refused file, a Mach-O universal header before one, a FAT boot sector with an odd media byte, a planted disc descriptor or a split Base64 file. Each 7-Zip start must name its format, and nothing may be read in process.
- `compat_starts_7zip_only_for_a_format_it_names_and_hands_to_7zip` refuses `compat::run` without a format, or with `-tcab`, `-twim` or `-tchm`.
- `archives_that_hold_a_refused_signature_still_extract` reads a CAB that ends with a DMG and tars whose first name starts with `koly` or `ITSF`.
- `lzx_folders_are_refused_by_name_with_nothing_written` in `tests/hostile_cab.rs` refuses a lone LZX folder and an LZX folder after an MSZIP one.
- `tests/wim_corpus.rs` captures LZX, LZMS and solid images with `wimlib-imagex` and requires each to be refused by its method's name.
- `a_lone_fat_ntfs_hfs_or_apfs_volume_is_refused_by_name_in_every_container` in `tests/hostile_disk.rs` refuses each file system in every container.
- `tests/parity.rs` runs every corpus archive on a row marked `refused` in the [parity matrix](parity.md) through every engine.
- Kani proves under `[refused-signatures]` that the signature, boot sector, CAB folder and WIM header checks never panic on any short input. A CAB counts as LZX only from a folder inside the bytes read.
- `tests/data/refused` holds the CHM, compound file, LZX CAB and LZX and LZMS WIM samples. Its `NOTICE.txt` names their sources.
