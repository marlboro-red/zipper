# Formats Zipper refuses

Since 12 November 2026 Zipper refuses the formats and methods on this page. A licence audit found their readers derived, or likely derived, from 7-Zip's LGPL source, so the readers were removed, as [M32](../roadmap/m32-every-reader-under-zippers-licence.md) records. A second audit that day found the disk image reader a port of 7-Zip's LGPL partition and container code. It was removed too, and [M33](../roadmap/m33-clean-rewrites-of-the-copied-checks.md) tracks a clean rewrite. Zipper recognizes each one by its signature only and never decodes it. Zipper never runs 7-Zip, so no such file reaches it.

## How a refusal behaves

- Zipper refuses the file where its reader used to be chosen.
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
| Mach-O universal files with the other signature | `B9 FA F1 0E` at offset 0, after the 7z reader found no 7z | `Zipper does not read Mach-O universal files with the B9 FA F1 0E signature` |
| SquashFS of other versions | A SquashFS superblock of version 1 to 3, big-endian, or with a swapped vendor signature | `Zipper does not read SquashFS images other than version 4.0 in little-endian order` |

The last two rows have no reader under any licence. Zipper sent them to 7-Zip until 13 November 2026, when [M20](../roadmap/m20-zipper-without-7-zip.md) removed every path to 7-Zip.

A file with an ISO 9660 or UDF descriptor at offset `0x8001` reads as a disc when the disc reader finds a whole descriptor set. Hybrid discs carry an Apple partition map, an HFS+ volume or an MBR beside the disc, and they still read as discs.

## Methods

The CAB and WIM readers still read their other methods. They refuse a removed method by its name.

| Archive | Refused | Message |
| --- | --- | --- |
| CAB | A folder compressed with LZX | `CAB folder is compressed with LZX, which Zipper does not decode` |
| WIM | An image compressed with LZX | `WIM image is compressed with LZX, which Zipper does not decode` |
| WIM and ESD | An image or solid resource compressed with LZMS | `WIM image is compressed with LZMS, which Zipper does not decode` |

Every ESD file, and every solid WIM that wimlib writes with its default compression, compresses its metadata with LZMS, so each is refused. A CAB whose other folders are stored, MSZIP or Quantum is refused whole when one folder uses LZX.

## Disk images

Zipper refuses disk images whole, whatever they hold. That covers raw MBR and GPT disks and VHD, VHDX, QCOW, VDI, VMDK and Android sparse images. No partition is read, so no file system inside is decoded, and no image goes to 7-Zip.

- The container signatures at offset 0 are checked before the WIM, 7z, disc and file system readers sniff the file, and before the check for a damaged executable. Only a ZIP, executable, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ or RAR signature wins over them.
- A VHD footer at the end, a FAT boot sector, a GPT header and an MBR table are checked after the 7z, WIM, executable, LP, UEFI, disc, SquashFS, CramFS and ext readers. So an ISO 9660 or UDF disc with a hybrid MBR still reads as a disc. An ext volume keeps its boot block, which may hold an old MBR. So an MBR table is not refused when the ext reader's superblock checks accept the superblock at offset 1024.
- A fixed VHD that holds an ext, SquashFS or CramFS volume with no partition table reads as that volume, since its bytes are the volume followed by a footer. No disk code reads it.
- An archive that holds a disk image, such as a tar, extracts the image as one file.
- A disk that also has a few planted disc descriptor bytes at `0x8001` is refused, since the disc reader finds no whole descriptor set in it.

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

- `every_refused_format_is_refused_with_nothing_written` in `tests/refused.rs` runs every format and method above but the last two formats through the CLI's `extract`, `list` and `test` and `zipper::extract_archive`. It runs each of those formats through `zipper::choose_reader` too.
- `no_refused_format_reaches_a_fake_7zip_on_path` in the same file puts a fake `7zz` first on `PATH` and checks that none of these files starts it.
- `crafted_refused_files_never_open_in_process_or_start_7zip` runs files an adversarial review crafted. They hold a prefix before a refused file, a Mach-O or Mach-O universal header before one, a FAT boot sector with an odd media byte, a planted disc descriptor or a split Base64 file. Some hold a VHD, VHDX, QCOW or GPT disk. None may be read, and none may start 7-Zip.
- `tests/no_7zip.rs` refuses the two rows Zipper once sent to 7-Zip. It also checks that a fake 7-Zip never starts for any file in `tests/data`.
- `archives_that_hold_a_refused_signature_still_extract` reads a CAB that ends with a DMG, tars that hold a VHD or a QCOW image, and tars whose first name starts with `koly`, `ITSF` or `KDMV`.
- `lzx_folders_are_refused_by_name_with_nothing_written` in `tests/hostile_cab.rs` refuses a lone LZX folder and an LZX folder after an MSZIP one.
- `tests/wim_corpus.rs` captures LZX, LZMS and solid images with `wimlib-imagex` and requires each to be refused by its method's name.
- `a_disk_image_split_into_numbered_volumes_is_refused` refuses a VHD named `disk.vhd.001` with its `.002`, whose joined bytes end with the footer.
- `disks_with_a_planted_disc_descriptor_are_refused` refuses GPT, MBR and fixed VHD disks that carry a planted disc descriptor.
- `a_first_volume_read_alone_never_reaches_7zip` splits a fixed VHD that holds an ext volume into a small `.001` and a larger `.002`, and checks that 7-Zip never starts.
- `discs_and_ext_volumes_that_start_with_a_partition_table_still_read` reads an ISO 9660 disc and an ext4 volume that each hold an MBR table in their first sector.
- `tests/parity.rs` runs every corpus archive on a row marked `refused` in the [parity matrix](parity.md) and requires each to be refused.
- Kani proves under `[refused-signatures]` that the signature, boot sector and partition table checks never panic on any short input.
- `tests/data/refused` holds the CHM, compound file, LZX CAB, LZX and LZMS WIM and disk image samples. Its `NOTICE.txt` names their sources.
