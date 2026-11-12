use anyhow::Result;
use std::path::Path;

const HEAD: usize = 1 << 12;
const SECTOR: usize = 512;
const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const COMPOUND: &[u8; 8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
const CHM: &[u8; 8] = b"ITSF\x03\0\0\0";
const HELP2: &[u8; 12] = b"ITOLITLS\x01\0\0\0";
const KOLY: &[u8; 12] = b"koly\0\0\0\x04\0\0\x02\0";
const HFS_AT: usize = 1024;
const HFS_EMBEDDED_AT: usize = HFS_AT + 0x7C;
const APM_MAP_AT: usize = SECTOR;
const APFS_MAGIC_AT: usize = 32;
const APFS_TYPE_AT: usize = 24;
const APFS_BLOCK_AT: usize = 36;
const APFS_SUPERBLOCK_TYPE: u32 = 1;
const APFS_SMALLEST_BLOCK: u32 = 1 << 12;
const APFS_LARGEST_BLOCK: u32 = 1 << 16;
const MOST_FAT_COPIES: u8 = 4;
const NTFS_OEM: &[u8; 8] = b"NTFS    ";
const ISO_DESCRIPTOR_AT: usize = 0x8001;
const ISO_IDENTIFIERS: [&[u8; 5]; 4] = [b"CD001", b"BEA01", b"NSR02", b"NSR03"];
const BASE64_EXTENSION: &str = "b64";
const CAB: &[u8; 4] = b"MSCF";
const CAB_HEADER: usize = 36;
const CAB_FOLDERS_AT: usize = 26;
const CAB_FLAGS_AT: usize = 30;
const CAB_LINKS_PREVIOUS: u16 = 1;
const CAB_LINKS_NEXT: u16 = 2;
const CAB_HAS_RESERVE: u16 = 4;
const CAB_FOLDER: usize = 8;
const CAB_TYPE_AT: usize = 6;
const CAB_METHOD_MASK: u8 = 0x0F;
const CAB_LZX: u8 = 3;
const WIM: &[u8; 8] = b"MSWIM\0\0\0";
const WIM_FLAGS_AT: usize = 16;
const WIM_COMPRESSED: u32 = 0x2;
const WIM_LZX: u32 = 1 << 18;
const WIM_LZMS: u32 = 1 << 19;
const VHD_COOKIE: &[u8; 8] = b"conectix";
const VHD_CHECKSUM_AT: usize = 64;
const VHDX_SIGNATURE: &[u8; 8] = b"vhdxfile";
const QCOW_MAGIC: &[u8; 4] = b"QFI\xFB";
const VDI_TEXT: &[u8; 4] = b"<<< ";
const VDI_SIGNATURE_AT: usize = 0x40;
const VDI_SIGNATURE: u32 = 0xBEDA_107F;
const VDI_VERSION_AT: usize = 0x44;
const VDI_MAJOR_VERSION: u32 = 1;
const VMDK_SPARSE_MAGIC: &[u8; 4] = b"KDMV";
const VMDK_DESCRIPTOR: &[u8; 21] = b"# Disk DescriptorFile";
const ANDROID_SPARSE_MAGIC: [u8; 4] = 0xED26_FF3A_u32.to_le_bytes();
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const GPT_HEADER_AT: [u64; 2] = [512, 4096];
const MBR_TABLE_AT: usize = 446;
const MBR_ENTRY: usize = 16;
const MBR_STATUSES: [u8; 2] = [0x00, 0x80];
const MBR_TYPE_AT: usize = 4;
const MBR_SECTORS_AT: usize = 12;
const EXT_MAGIC_AT: usize = 1080;
const EXT_MAGIC: [u8; 2] = [0x53, 0xEF];

pub(crate) const COMPOUND_FILES: &str = "compound files such as MSI, DOC and XLS";
pub(crate) const CHM_FILES: &str = "CHM and Help 2 files";
pub(crate) const DMG_IMAGES: &str = "DMG images";
pub(crate) const HFS_VOLUMES: &str = "HFS+ and HFSX volumes";
pub(crate) const APPLE_PARTITION_MAPS: &str = "Apple partition maps";
pub(crate) const APFS_CONTAINERS: &str = "APFS containers";
pub(crate) const NTFS_VOLUMES: &str = "NTFS volumes";
pub(crate) const FAT_VOLUMES: &str = "FAT volumes";
pub(crate) const BASE64_FILES: &str = "Base64 files";
pub(crate) const CAB_LZX_FOLDERS: &str = "CAB folders compressed with LZX";
pub(crate) const WIM_LZX_IMAGES: &str = "WIM images compressed with LZX";
pub(crate) const WIM_LZMS_IMAGES: &str = "WIM and ESD images compressed with LZMS";
pub(crate) const VHD_IMAGES: &str = "VHD images";
pub(crate) const VHDX_IMAGES: &str = "VHDX images";
pub(crate) const QCOW_IMAGES: &str = "QCOW images";
pub(crate) const VDI_IMAGES: &str = "VDI images";
pub(crate) const VMDK_IMAGES: &str = "VMDK images";
pub(crate) const ANDROID_SPARSE_IMAGES: &str = "Android sparse images";
pub(crate) const GPT_DISKS: &str = "GPT disk images";
pub(crate) const MBR_DISKS: &str = "MBR disk images";
pub(crate) const UNNAMED_FOR_7ZIP: &str =
    "a file through 7-Zip without naming its format as one Zipper hands to 7-Zip";

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

// SPEC(Microsoft Virtual Hard Disk Image Format Specification, Hard Disk Footer Format): the
// checksum at 64 is the one's complement of the sum of the footer's other bytes.
fn is_vhd_footer(footer: &[u8]) -> bool {
    if footer.len() < SECTOR || !footer.starts_with(VHD_COOKIE) {
        return false;
    }
    let checksum = VHD_CHECKSUM_AT..VHD_CHECKSUM_AT + 4;
    let sum = footer[..SECTOR]
        .iter()
        .enumerate()
        .filter(|(at, _)| !checksum.contains(at))
        .fold(0u32, |sum, (_, byte)| sum.wrapping_add(u32::from(*byte)));
    be32(footer, VHD_CHECKSUM_AT) == !sum
}

fn is_vdi_header(head: &[u8]) -> bool {
    head.starts_with(VDI_TEXT)
        && head.len() >= VDI_VERSION_AT + 4
        && le32(head, VDI_SIGNATURE_AT) == VDI_SIGNATURE
        && le32(head, VDI_VERSION_AT) >> 16 == VDI_MAJOR_VERSION
}

fn container(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(VHDX_SIGNATURE) {
        Some(VHDX_IMAGES)
    } else if head.starts_with(QCOW_MAGIC) {
        Some(QCOW_IMAGES)
    } else if is_vdi_header(head) {
        Some(VDI_IMAGES)
    } else if head.starts_with(VMDK_SPARSE_MAGIC) || head.starts_with(VMDK_DESCRIPTOR) {
        Some(VMDK_IMAGES)
    } else if head.starts_with(&ANDROID_SPARSE_MAGIC) {
        Some(ANDROID_SPARSE_IMAGES)
    } else {
        None
    }
}

fn is_ext_volume(head: &[u8]) -> bool {
    head.get(EXT_MAGIC_AT..EXT_MAGIC_AT + 2) == Some(EXT_MAGIC.as_slice())
}

// SPEC(UEFI 2.10, 5.2.1 Legacy MBR): four 16-byte partition records at 446, then the signature
// 0x55 0xAA at 510.
fn has_partition_table(head: &[u8]) -> bool {
    if head.len() < SECTOR || head[SECTOR - 2..SECTOR] != BOOT_SIGNATURE {
        return false;
    }
    let mut records = head[MBR_TABLE_AT..SECTOR - 2].chunks_exact(MBR_ENTRY);
    records
        .clone()
        .all(|record| MBR_STATUSES.contains(&record[0]))
        && records.any(|record| record[MBR_TYPE_AT] != 0 && le32(record, MBR_SECTORS_AT) != 0)
}

fn jumps(sector: &[u8]) -> bool {
    match sector[0] {
        0xE9 => true,
        0xEB => sector[2] == 0x90,
        _ => false,
    }
}

// SPEC(Microsoft fatgen103, Boot Sector and BPB): the fields every FAT boot sector holds,
// with media 0xF0 or 0xF8 to 0xFF.
pub(crate) fn is_fat_boot_sector(sector: &[u8]) -> bool {
    if sector.len() < SECTOR || !jumps(sector) || sector[SECTOR - 2..SECTOR] != BOOT_SIGNATURE {
        return false;
    }
    let bytes_per_sector = le16(sector, 11);
    let sectors_per_cluster = sector[13];
    let reserved = le16(sector, 14);
    let fats = sector[16];
    let sectors = u32::from(le16(sector, 19)).max(le32(sector, 32));
    let media = sector[21];
    let fat_sectors = u32::from(le16(sector, 22)).max(le32(sector, 36));
    [512, 1024, 2048, 4096].contains(&bytes_per_sector)
        && sectors_per_cluster.is_power_of_two()
        && reserved != 0
        && (1..=MOST_FAT_COPIES).contains(&fats)
        && sectors != 0
        && (media == 0xF0 || media >= 0xF8)
        && fat_sectors != 0
}

pub(crate) fn is_ntfs_boot_sector(sector: &[u8]) -> bool {
    sector.len() >= SECTOR
        && jumps(sector)
        && &sector[3..11] == NTFS_OEM
        && sector[SECTOR - 2..SECTOR] == BOOT_SIGNATURE
}

pub(crate) fn is_apfs_container(head: &[u8]) -> bool {
    if head.len() < APFS_BLOCK_AT + 4 || &head[APFS_MAGIC_AT..APFS_MAGIC_AT + 4] != b"NXSB" {
        return false;
    }
    let block = le32(head, APFS_BLOCK_AT);
    le32(head, APFS_TYPE_AT) & 0xFFFF == APFS_SUPERBLOCK_TYPE
        && block.is_power_of_two()
        && (APFS_SMALLEST_BLOCK..=APFS_LARGEST_BLOCK).contains(&block)
}

pub(crate) fn is_hfs_volume(head: &[u8]) -> bool {
    let Some(header) = head.get(HFS_AT..HFS_AT + 4) else {
        return false;
    };
    matches!(header, b"H+\0\x04" | b"HX\0\x05")
        || &header[..2] == b"BD"
            && head.get(HFS_EMBEDDED_AT..HFS_EMBEDDED_AT + 2) == Some(b"H+".as_slice())
}

fn is_apple_partition_map(head: &[u8]) -> bool {
    head.starts_with(b"ER") && head.get(APM_MAP_AT..APM_MAP_AT + 2) == Some(b"PM".as_slice())
}

fn is_base64_text(head: &[u8]) -> bool {
    !head.is_empty()
        && head
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=\r\n\t ".contains(byte))
        && head.iter().any(u8::is_ascii_alphanumeric)
}

fn skip_name(head: &[u8], at: usize) -> Option<usize> {
    let length = head.get(at..)?.iter().position(|byte| *byte == 0)?;
    Some(at + length + 1)
}

// SPEC(MS-CAB CFHEADER, CFFOLDER): optional reserve sizes and the
// previous and next cabinet names come before the folders, whose typeCompress is at 6.
fn cab_has_lzx_folder(head: &[u8]) -> bool {
    if !head.starts_with(CAB) || head.len() < CAB_HEADER {
        return false;
    }
    let flags = le16(head, CAB_FLAGS_AT);
    let folders = usize::from(le16(head, CAB_FOLDERS_AT));
    let mut at = CAB_HEADER;
    let mut folder_reserve = 0;
    if flags & CAB_HAS_RESERVE != 0 {
        let Some(sizes) = head.get(at..at + 4) else {
            return false;
        };
        folder_reserve = usize::from(sizes[2]);
        at += 4 + usize::from(le16(sizes, 0));
    }
    for link in [CAB_LINKS_PREVIOUS, CAB_LINKS_NEXT] {
        if flags & link != 0 {
            let Some(after) = skip_name(head, at).and_then(|at| skip_name(head, at)) else {
                return false;
            };
            at = after;
        }
    }
    head.get(at..)
        .unwrap_or_default()
        .chunks(CAB_FOLDER + folder_reserve)
        .take(folders)
        .any(|folder| {
            folder
                .get(CAB_TYPE_AT)
                .is_some_and(|method| method & CAB_METHOD_MASK == CAB_LZX)
        })
}

fn wim_method(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(WIM) || head.len() < WIM_FLAGS_AT + 4 {
        return None;
    }
    let flags = le32(head, WIM_FLAGS_AT);
    if flags & WIM_COMPRESSED == 0 {
        None
    } else if flags & WIM_LZX != 0 {
        Some(WIM_LZX_IMAGES)
    } else if flags & WIM_LZMS != 0 {
        Some(WIM_LZMS_IMAGES)
    } else {
        None
    }
}

fn leading(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(COMPOUND) {
        Some(COMPOUND_FILES)
    } else if head.starts_with(CHM) || head.starts_with(HELP2) {
        Some(CHM_FILES)
    } else {
        None
    }
}

fn volume(head: &[u8]) -> Option<&'static str> {
    if is_hfs_volume(head) {
        Some(HFS_VOLUMES)
    } else if is_apple_partition_map(head) {
        Some(APPLE_PARTITION_MAPS)
    } else if is_apfs_container(head) {
        Some(APFS_CONTAINERS)
    } else if is_ntfs_boot_sector(head) {
        Some(NTFS_VOLUMES)
    } else {
        None
    }
}

struct Start {
    file: crate::volumes::ArchiveFile,
    head: Vec<u8>,
}

impl Start {
    fn read(path: &Path) -> Result<Start> {
        let file = crate::volumes::open(path)?;
        let mut head = vec![0u8; HEAD];
        let mut filled = 0;
        while filled < HEAD {
            let read = file.read_at(&mut head[filled..], filled as u64)?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        head.truncate(filled);
        Ok(Start { file, head })
    }

    fn bytes_at<const N: usize>(&self, at: u64) -> Result<Option<[u8; N]>> {
        let mut bytes = [0u8; N];
        let mut filled = 0;
        while filled < N {
            let read = self
                .file
                .read_at(&mut bytes[filled..], at + filled as u64)?;
            if read == 0 {
                return Ok(None);
            }
            filled += read;
        }
        Ok(Some(bytes))
    }

    fn is_dmg(&self) -> Result<bool> {
        let length = self.file.len()?;
        Ok(self.head.starts_with(KOLY)
            || length >= SECTOR as u64
                && self.bytes_at::<12>(length - SECTOR as u64)?.as_ref() == Some(KOLY))
    }

    fn is_disc(&self) -> Result<bool> {
        Ok(self
            .bytes_at::<5>(ISO_DESCRIPTOR_AT as u64)?
            .is_some_and(|identifier| ISO_IDENTIFIERS.contains(&&identifier)))
    }

    fn disk_container(&self) -> Option<&'static str> {
        if is_vhd_footer(&self.head) {
            Some(VHD_IMAGES)
        } else {
            container(&self.head)
        }
    }

    fn ends_with_vhd_footer(&self) -> Result<bool> {
        let length = self.file.len()?;
        Ok(length >= SECTOR as u64
            && self
                .bytes_at::<SECTOR>(length - SECTOR as u64)?
                .is_some_and(|footer| is_vhd_footer(&footer)))
    }

    // SPEC(UEFI 2.10, 5.3.2 GPT Header): the header sits at LBA 1, so at 512 or 4096 bytes for
    // 512-byte or 4 KiB sectors.
    fn has_gpt_header(&self) -> Result<bool> {
        for at in GPT_HEADER_AT {
            if self.bytes_at::<8>(at)?.as_ref() == Some(GPT_SIGNATURE) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn disk(&self) -> Result<Option<&'static str>> {
        Ok(if self.ends_with_vhd_footer()? {
            Some(VHD_IMAGES)
        } else if is_fat_boot_sector(&self.head) {
            Some(FAT_VOLUMES)
        } else if self.has_gpt_header()? {
            Some(GPT_DISKS)
        } else if has_partition_table(&self.head) && !is_ext_volume(&self.head) {
            Some(MBR_DISKS)
        } else {
            None
        })
    }
}

fn named_base64(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(BASE64_EXTENSION))
}

pub(crate) fn leading_signature(path: &Path) -> Result<Option<&'static str>> {
    Ok(leading(&Start::read(path)?.head))
}

pub(crate) fn dmg(path: &Path) -> Result<Option<&'static str>> {
    Ok(Start::read(path)?.is_dmg()?.then_some(DMG_IMAGES))
}

pub(crate) fn raw_volume(path: &Path) -> Result<Option<&'static str>> {
    Ok(volume(&Start::read(path)?.head))
}

pub(crate) fn disk_container(path: &Path) -> Result<Option<&'static str>> {
    Ok(Start::read(path)?.disk_container())
}

pub(crate) fn disk(path: &Path) -> Result<Option<&'static str>> {
    Start::read(path)?.disk()
}

pub(crate) fn base64(path: &Path) -> Result<Option<&'static str>> {
    Ok((named_base64(path) && is_base64_text(&Start::read(path)?.head)).then_some(BASE64_FILES))
}

pub(crate) fn format(path: &Path) -> Result<Option<&'static str>> {
    let start = Start::read(path)?;
    let head = &start.head;
    if let Some(format) = leading(head) {
        return Ok(Some(format));
    }
    if start.is_dmg()? {
        return Ok(Some(DMG_IMAGES));
    }
    if named_base64(path) && is_base64_text(head) {
        return Ok(Some(BASE64_FILES));
    }
    if cab_has_lzx_folder(head) {
        return Ok(Some(CAB_LZX_FOLDERS));
    }
    if let Some(format) = wim_method(head) {
        return Ok(Some(format));
    }
    if let Some(format) = start.disk_container() {
        return Ok(Some(format));
    }
    // EXTERNAL(hdiutil makehybrid, xorriso): hybrid discs carry an Apple partition map or an
    // HFS+ volume beside ISO 9660 or UDF.
    if start.is_disc()? {
        return Ok(None);
    }
    if let Some(format) = volume(head) {
        return Ok(Some(format));
    }
    start.disk()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fat_sector() -> [u8; SECTOR] {
        let mut sector = [0u8; SECTOR];
        sector[..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        sector[11..13].copy_from_slice(&512u16.to_le_bytes());
        sector[13] = 4;
        sector[14..16].copy_from_slice(&1u16.to_le_bytes());
        sector[16] = 2;
        sector[19..21].copy_from_slice(&40_000u16.to_le_bytes());
        sector[21] = 0xF8;
        sector[22..24].copy_from_slice(&40u16.to_le_bytes());
        sector[SECTOR - 2..].copy_from_slice(&BOOT_SIGNATURE);
        sector
    }

    #[test]
    fn a_fat_boot_sector_needs_every_bpb_field_it_checks() {
        assert!(is_fat_boot_sector(&fat_sector()));
        let breaks: [(&[usize], u8); 11] = [
            (&[0], 0x33),
            (&[2], 0),
            (&[11, 12], 0),
            (&[13], 3),
            (&[14, 15], 0),
            (&[16], 0),
            (&[16], MOST_FAT_COPIES + 1),
            (&[19, 20], 0),
            (&[21], 0xF1),
            (&[22, 23], 0),
            (&[SECTOR - 1], 0),
        ];
        for (bytes, value) in breaks {
            let mut sector = fat_sector();
            for at in bytes {
                sector[*at] = value;
            }
            assert!(!is_fat_boot_sector(&sector), "bytes {bytes:?} = {value:#x}");
        }
    }

    #[test]
    fn an_ntfs_boot_sector_is_not_a_fat_one() {
        let mut sector = [0u8; SECTOR];
        sector[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
        sector[3..11].copy_from_slice(NTFS_OEM);
        sector[11..13].copy_from_slice(&512u16.to_le_bytes());
        sector[13] = 8;
        sector[21] = 0xF8;
        sector[SECTOR - 2..].copy_from_slice(&BOOT_SIGNATURE);
        assert!(is_ntfs_boot_sector(&sector));
        assert!(!is_fat_boot_sector(&sector));
        sector[SECTOR - 1] = 0;
        assert!(!is_ntfs_boot_sector(&sector));
    }

    fn cab(flags: u16, reserve: Option<(u16, u8)>, names: &[&[u8]], methods: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; CAB_HEADER];
        bytes[..4].copy_from_slice(CAB);
        bytes[CAB_FOLDERS_AT..CAB_FOLDERS_AT + 2]
            .copy_from_slice(&(methods.len() as u16).to_le_bytes());
        bytes[CAB_FLAGS_AT..CAB_FLAGS_AT + 2].copy_from_slice(&flags.to_le_bytes());
        let mut folder_reserve = 0;
        if let Some((header, folder)) = reserve {
            bytes.extend(header.to_le_bytes());
            bytes.extend([folder, 0]);
            bytes.extend(vec![0xEE; usize::from(header)]);
            folder_reserve = usize::from(folder);
        }
        for name in names {
            bytes.extend(*name);
            bytes.push(0);
        }
        for method in methods {
            let mut folder = vec![0x33; CAB_FOLDER + folder_reserve];
            folder[CAB_TYPE_AT] = *method;
            folder[CAB_TYPE_AT + 1] = 16;
            bytes.extend(folder);
        }
        bytes
    }

    #[test]
    fn an_lzx_folder_is_found_after_every_optional_header_field() {
        let links: [&[u8]; 4] = [b"prev.cab", b"disk 1", b"next.cab", b"disk 2"];
        for flags in 0..8u16 {
            let names = match flags & 3 {
                0 => &links[..0],
                1 => &links[..2],
                2 => &links[2..],
                _ => &links[..],
            };
            let reserve = (flags & CAB_HAS_RESERVE != 0).then_some((20, 5));
            for methods in [&[0u8, 1, 2][..], &[1, 3], &[3], &[0x13, 0], &[]] {
                let bytes = cab(flags, reserve, names, methods);
                let wanted = methods
                    .iter()
                    .any(|method| method & CAB_METHOD_MASK == CAB_LZX);
                assert_eq!(cab_has_lzx_folder(&bytes), wanted, "{flags} {methods:?}");
                for cut in 0..bytes.len() {
                    let found = cab_has_lzx_folder(&bytes[..cut]);
                    assert!(!found || wanted, "{flags} {methods:?} cut to {cut}");
                }
            }
        }
    }

    #[test]
    fn wim_headers_name_lzx_and_lzms_only_when_compressed() {
        let mut header = [0u8; 208];
        header[..8].copy_from_slice(WIM);
        for (flags, wanted) in [
            (0, None),
            (WIM_LZX, None),
            (WIM_COMPRESSED | (1 << 17), None),
            (WIM_COMPRESSED | WIM_LZX, Some(WIM_LZX_IMAGES)),
            (WIM_COMPRESSED | WIM_LZMS, Some(WIM_LZMS_IMAGES)),
        ] {
            header[WIM_FLAGS_AT..WIM_FLAGS_AT + 4].copy_from_slice(&flags.to_le_bytes());
            assert_eq!(wim_method(&header), wanted, "{flags:#x}");
            assert_eq!(wim_method(&header[..WIM_FLAGS_AT + 3]), None);
        }
    }

    #[test]
    fn no_check_panics_on_any_cut_or_flip_of_pseudo_random_heads() {
        let mut state = 0x5eed_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let length = (next() % 1600) as usize;
            let mut head: Vec<u8> = (0..length).map(|_| next() as u8).collect();
            if let Some(first) = head.get_mut(..4) {
                let signatures: [&[u8]; 5] = [CAB, &WIM[..4], b"ER\0\0", b"NXSB", COMPOUND];
                first.copy_from_slice(&signatures[(next() % 5) as usize][..4]);
            }
            let _ = (leading(&head), volume(&head), wim_method(&head));
            let _ = (
                cab_has_lzx_folder(&head),
                is_fat_boot_sector(&head),
                is_base64_text(&head),
            );
            let _ = (container(&head), has_partition_table(&head));
            let _ = (is_vhd_footer(&head), is_ext_volume(&head));
        }
    }

    fn vhd_footer() -> [u8; SECTOR] {
        let mut footer = [0u8; SECTOR];
        footer[..8].copy_from_slice(VHD_COOKIE);
        footer[60..64].copy_from_slice(&2u32.to_be_bytes());
        let sum: u32 = footer.iter().map(|byte| u32::from(*byte)).sum();
        footer[VHD_CHECKSUM_AT..VHD_CHECKSUM_AT + 4].copy_from_slice(&(!sum).to_be_bytes());
        footer
    }

    #[test]
    fn a_vhd_footer_needs_its_cookie_and_a_checksum_over_every_other_byte() {
        let footer = vhd_footer();
        assert!(is_vhd_footer(&footer));
        assert!(!is_vhd_footer(&footer[..SECTOR - 1]));
        for at in 0..SECTOR {
            let mut changed = footer;
            changed[at] ^= 0x01;
            assert!(!is_vhd_footer(&changed), "byte {at}");
        }
    }

    fn mbr_sector(records: &[(u8, u8, u32)]) -> [u8; SECTOR] {
        let mut sector = [0u8; SECTOR];
        for (index, (status, kind, sectors)) in records.iter().enumerate() {
            let record = &mut sector[MBR_TABLE_AT + index * MBR_ENTRY..][..MBR_ENTRY];
            record[0] = *status;
            record[MBR_TYPE_AT] = *kind;
            record[8..12].copy_from_slice(&2048u32.to_le_bytes());
            record[MBR_SECTORS_AT..MBR_SECTORS_AT + 4].copy_from_slice(&sectors.to_le_bytes());
        }
        sector[SECTOR - 2..].copy_from_slice(&BOOT_SIGNATURE);
        sector
    }

    #[test]
    fn a_partition_table_needs_the_signature_known_statuses_and_one_real_record() {
        assert!(has_partition_table(&mbr_sector(&[(0x80, 0x83, 64)])));
        assert!(has_partition_table(&mbr_sector(&[
            (0, 0, 0),
            (0, 0x07, 64)
        ])));
        let refused: [&[(u8, u8, u32)]; 4] = [
            &[],
            &[(0, 0x83, 0)],
            &[(0, 0, 64)],
            &[(0x80, 0x83, 64), (0x7F, 0, 0)],
        ];
        for records in refused {
            assert!(!has_partition_table(&mbr_sector(records)), "{records:?}");
        }
        let mut unsigned = mbr_sector(&[(0, 0x83, 64)]);
        unsigned[SECTOR - 1] = 0;
        assert!(!has_partition_table(&unsigned));
        assert!(!has_partition_table(&unsigned[..SECTOR - 1]));
    }

    #[test]
    fn each_container_is_named_by_its_own_signature() {
        let mut vdi = vec![0u8; 0x200];
        vdi[..4].copy_from_slice(VDI_TEXT);
        vdi[VDI_SIGNATURE_AT..VDI_SIGNATURE_AT + 4].copy_from_slice(&VDI_SIGNATURE.to_le_bytes());
        vdi[VDI_VERSION_AT..VDI_VERSION_AT + 4].copy_from_slice(&0x0001_0001u32.to_le_bytes());
        let mut old_vdi = vdi.clone();
        old_vdi[VDI_VERSION_AT + 2] = 0;
        let cases: [(&[u8], Option<&str>); 9] = [
            (b"vhdxfile", Some(VHDX_IMAGES)),
            (b"QFI\xFB\0\0\0\x03", Some(QCOW_IMAGES)),
            (&vdi, Some(VDI_IMAGES)),
            (&old_vdi, None),
            (&vdi[1..], None),
            (b"KDMV\x01\0\0\0", Some(VMDK_IMAGES)),
            (b"# Disk DescriptorFile\nversion=1\n", Some(VMDK_IMAGES)),
            (b"\x3A\xFF\x26\xED\x01\0", Some(ANDROID_SPARSE_IMAGES)),
            (b"# Disk Descriptor", None),
        ];
        for (head, wanted) in cases {
            assert_eq!(container(head), wanted, "{:?}", &head[..head.len().min(8)]);
        }
    }

    #[test]
    fn base64_text_is_only_its_alphabet_and_space() {
        assert!(is_base64_text(b"aGVsbG8K\r\n"));
        assert!(!is_base64_text(b""));
        assert!(!is_base64_text(b"==\n"));
        assert!(!is_base64_text(b"aGVs*bG8K"));
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(50)]
    fn cab_and_wim_headers_never_panic_and_name_lzx_only_from_a_folder() {
        let bytes: [u8; 48] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let head = &bytes[..length];
        if cab_has_lzx_folder(head) {
            assert!(head.starts_with(CAB) && head.len() > CAB_HEADER + CAB_TYPE_AT);
        }
        if wim_method(head).is_some() {
            assert!(le32(head, WIM_FLAGS_AT) & WIM_COMPRESSED != 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn volume_and_boot_sector_checks_never_panic_on_any_short_head() {
        let bytes: [u8; SECTOR + 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let head = &bytes[..length];
        let _ = (
            leading(head),
            is_fat_boot_sector(head),
            is_ntfs_boot_sector(head),
        );
        let _ = (is_apfs_container(head), is_apple_partition_map(head));
    }

    #[kani::proof]
    #[kani::unwind(23)]
    fn disk_signature_and_partition_table_checks_never_panic_on_any_short_head() {
        let bytes: [u8; SECTOR + 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let head = &bytes[..length];
        if container(head) == Some(VDI_IMAGES) {
            assert!(head.len() >= VDI_VERSION_AT + 4);
        }
        if has_partition_table(head) {
            assert!(head.len() >= SECTOR && head[SECTOR - 2..SECTOR] == BOOT_SIGNATURE);
        }
        let _ = is_ext_volume(head);
    }
}
