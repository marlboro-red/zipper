# Formats Zipper refuses

Since 12 November 2026 Zipper refuses the formats and methods on this page in every engine. A licence audit found their readers derived, or likely derived, from 7-Zip's LGPL source, so the readers were removed, as [M32](../roadmap/m32-every-reader-under-zippers-licence.md) records. A second audit that day found the disk image reader a port of 7-Zip's LGPL partition and container code. It was removed too, and [M33](../roadmap/m33-clean-rewrites-of-the-copied-checks.md) tracks a clean rewrite. Zipper recognizes each one by its signature only and never decodes it. No engine hands such a file to 7-Zip.

## How a refusal behaves

- The default and native engines refuse the file where its reader used to be chosen.
- `--zip-engine backend` checks the file before 7-Zip starts. So does `zipper::compat::run`, the library's way to start 7-Zip.
- 7-Zip only ever starts with `-t` naming one format, so it never searches a file for another one. `--zip-engine backend` names the format from a signature at a fixed place, such as `7z`, `rar`, `zip`, `gzip`, `tar`, `iso` or `squashfs`. It names Intel HEX `ihex` from its records. A file with none of these signatures is refused with `Zipper does not read a file through 7-Zip without naming its format as one Zipper hands to 7-Zip`. CAB, WIM, DMG and disk images are never among them.
- The default engine sends to 7-Zip only what its own reader checked first, such as a 7z with BCJ2, a Mach-O universal file as `-tmub`, or a SquashFS image of another version as `-tsquashfs`. A split or pre-1.13 WIM is refused instead.
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
| VHD | `conectix` at offset 0 or 512 bytes before the end, in a footer whose checksum holds | `Zipper does not read VHD images` |
| VHDX | `vhdxfile` at offset 0 | `Zipper does not read VHDX images` |
| QCOW | `QFI FB` at offset 0 | `Zipper does not read QCOW images` |
| VDI | `<<< ` at offset 0, `7F 10 DA BE` at `0x40` and major version 1 at `0x44` | `Zipper does not read VDI images` |
| VMDK | `KDMV` at offset 0, or a descriptor that starts with `# Disk DescriptorFile` | `Zipper does not read VMDK images` |
| Android sparse images | `3A FF 26 ED` at offset 0 | `Zipper does not read Android sparse images` |
| GPT disks | `EFI PART` at offset 512 or 4096, the header of LBA 1 for 512-byte or 4 KiB sectors | `Zipper does not read GPT disk images` |
| MBR disks | `55 AA` at 510 and a partition table at 446 whose four status bytes are `00` or `80`, with at least one entry of a nonzero type and size | `Zipper does not read MBR disk images` |

A file with an ISO 9660 or UDF descriptor at offset `0x8001` is not refused by the volume, partition or VHD footer checks of the backend. Hybrid discs carry an Apple partition map, an HFS+ volume or an MBR beside the disc. The default and native engines read them as discs, and `--zip-engine backend` hands them to 7-Zip as `-tiso` or `-tudf`, so 7-Zip reads only the disc.

## Methods

The CAB and WIM readers still read their other methods. They refuse a removed method by its name.

| Archive | Refused | Native message | Backend message |
| --- | --- | --- | --- |
| CAB | A folder compressed with LZX | `CAB folder is compressed with LZX, which Zipper does not decode` | `Zipper does not read CAB folders compressed with LZX` |
| WIM | An image compressed with LZX | `WIM image is compressed with LZX, which Zipper does not decode` | `Zipper does not read WIM images compressed with LZX` |
| WIM and ESD | An image or solid resource compressed with LZMS | `WIM image is compressed with LZMS, which Zipper does not decode` | `Zipper does not read WIM and ESD images compressed with LZMS` |

Every ESD file, and every solid WIM that wimlib writes with its default compression, compresses its metadata with LZMS, so each is refused. A CAB whose other folders are stored, MSZIP or Quantum is refused whole when one folder uses LZX. `--zip-engine backend` hands no CAB or WIM to 7-Zip. So no cabinet of a set, folder behind a large header reserve or solid resource of an unnamed method can reach it.

## Disk images

Every engine refuses disk images whole, whatever they hold. That covers raw MBR and GPT disks and VHD, VHDX, QCOW, VDI, VMDK and Android sparse images. No partition is read, so no file system inside is decoded, and no image goes to 7-Zip.

- The container signatures at offset 0 are checked before the WIM, 7z, executable, disc and file system readers sniff the file. Only a ZIP, executable, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ or RAR signature wins over them.
- A VHD footer at the end, a FAT boot sector, a GPT header and an MBR table are checked after the 7z, WIM, executable, LP, UEFI, disc, SquashFS, CramFS and ext readers. So an ISO 9660 or UDF disc with a hybrid MBR still reads as a disc. An ext volume keeps its boot block, which may hold an old MBR. So an MBR table is not refused when the ext reader's superblock checks accept the superblock at offset 1024.
- A fixed VHD that holds an ext, SquashFS or CramFS volume with no partition table reads as that volume, since its bytes are the volume followed by a footer. No disk code reads it.
- An archive that holds a disk image, such as a tar, extracts the image as one file in the default and native engines. `--zip-engine backend` refuses a file whose last 512 bytes are a valid VHD footer, or that holds a GPT header at offset 512, before it looks for another format. So it refuses an `ar` archive whose last member is a fixed VHD, a ZIP whose comment is a VHD footer and an ext volume with an old GPT header.
- `--zip-engine backend` hands a disk that also has a disc descriptor at `0x8001` to 7-Zip as a disc only in one case. Zipper's disc reader must find a whole descriptor set with a primary volume, or a UDF anchor. Otherwise it refuses the disk. A few planted descriptor bytes are not enough.
- A first volume `name.001` that Zipper reads alone, because `name.002` is larger, goes to 7-Zip without `.split`. So 7-Zip never joins volumes whose joined bytes Zipper did not check.

The ext, SquashFS and CramFS readers still read raw volumes. Before 12 November 2026 they also read the lone partition of a disk image, which the disk reader opened for them.

## Sources

The disk image signatures come from these specifications. No code was copied from any of them.

- Microsoft, Virtual Hard Disk Image Format Specification, version 1.0, for the VHD footer, its cookie and its checksum.
- Microsoft, [MS-VHDX], section 2.2.1, for the VHDX file type identifier.
- The QCOW2 image format description in the QEMU documentation, for the QCOW magic shared by every version.
- The libyal documentation of the VirtualBox Disk Image format, for the VDI signature and version fields.
- VMware, Virtual Disk Format 1.1, for the sparse extent magic and the descriptor's first line.
- `sparse_format.h` in AOSP `system/core/libsparse`, under the Apache License 2.0, for the Android sparse magic.
- UEFI Specification 2.10, sections 5.2.1 and 5.3.2, for the legacy MBR partition table and the GPT header signature.
- The common description of the MBR partition entry, where the status byte is `80` for an active partition and `00` for an inactive one. UEFI sets no rule for that byte, so the check takes the convention as a heuristic.
- The Linux kernel's ext4 disk layout document, for the superblock magic at offset 1080.

## Proof

- `every_refused_format_is_refused_in_every_engine_with_nothing_written` in `tests/refused.rs` runs every format and method above through each engine. It uses the CLI's `extract`, `list` and `test`, `zipper::extract_archive`, `zipper::choose_reader` and `zipper::compat::run`.
- `no_refused_format_reaches_a_fake_7zip_on_path` in the same file puts a fake `7zz` first on `PATH` and checks that none of these files starts it, while another format does.
- `every_7zip_start_names_the_format_and_crafted_refused_files_never_open_in_process` runs files an adversarial review crafted through every engine. They hold a prefix before a refused file, a Mach-O or Mach-O universal header before one, a FAT boot sector with an odd media byte, a planted disc descriptor or a split Base64 file. Some hold a VHD, VHDX, QCOW or GPT disk. Each 7-Zip start must name its format, and nothing may be read in process.
- `compat_starts_7zip_only_for_a_format_it_names_and_hands_to_7zip` refuses `compat::run` without a format, or with `-tcab`, `-twim` or `-tchm`.
- `archives_that_hold_a_refused_signature_still_extract` reads a CAB that ends with a DMG, tars that hold a VHD or a QCOW image, and tars whose first name starts with `koly`, `ITSF` or `KDMV`.
- `lzx_folders_are_refused_by_name_with_nothing_written` in `tests/hostile_cab.rs` refuses a lone LZX folder and an LZX folder after an MSZIP one.
- `tests/wim_corpus.rs` captures LZX, LZMS and solid images with `wimlib-imagex` and requires each to be refused by its method's name.
- `a_disk_image_split_into_numbered_volumes_is_refused_in_every_engine` refuses a VHD named `disk.vhd.001` with its `.002`, whose joined bytes end with the footer.
- `disks_with_a_planted_disc_descriptor_are_refused_before_7zip` refuses GPT, MBR and fixed VHD disks that carry a planted disc descriptor, through `choose_reader` and `compat::run`.
- `a_first_volume_read_alone_never_reaches_7zip_as_a_split_set` splits a fixed VHD that holds an ext volume into a small `.001` and a larger `.002`, and checks that 7-Zip never starts with `.split`.
- `discs_and_ext_volumes_that_start_with_a_partition_table_still_read` reads an ISO 9660 disc and an ext4 volume that each hold an MBR table in their first sector, in process, and names them `-tiso` and `-text` for 7-Zip.
- `tests/parity.rs` runs every corpus archive on a row marked `refused` in the [parity matrix](parity.md) through every engine.
- Kani proves under `[refused-signatures]` that the signature, boot sector, partition table, CAB folder and WIM header checks never panic on any short input. A CAB counts as LZX only from a folder inside the bytes read.
- `tests/data/refused` holds the CHM, compound file, LZX CAB, LZX and LZMS WIM and disk image samples. Its `NOTICE.txt` names their sources.
