use crate::{BadField, Field, field_check, native_cpio::tree::Tree};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
};

pub(crate) const BOOT_SECTOR: usize = 512;
const RECORD: usize = 32;
const SHORT_NAME: usize = 11;
const LONG_NAME_RECORDS: usize = 20;
const LONG_NAME_UNITS: usize = 13;
const LONG_NAME_BUFFER: usize = LONG_NAME_RECORDS * LONG_NAME_UNITS + 1;
const LARGEST_CLUSTER_LOG: u32 = 18;
const FAT32_ENTRY_MASK: u32 = 0x0FFF_FFFF;
const FAT32_BAD_CLUSTER: u32 = 0x0FFF_FFF7;
const FAT16_CLUSTER_LIMIT: u32 = 0xFFF5;
const FAT12_CLUSTER_LIMIT: u32 = 0xFF5;
const FAT32_FAT_SECTOR_LIMIT: u32 = 1 << 24;
const FAT32_READ_ENTRIES: u64 = 1 << 13;
const FIRST_CLUSTER: u32 = 2;
const FOLDER_BYTES_LIMIT: u32 = 1 << 21;
const DEPTH_LIMIT: u32 = 256;
const ITEM_LIMIT: usize = 1 << 30;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const ATTRIBUTE_READ_ONLY: u8 = 0x01;
const ATTRIBUTE_TYPE: u8 = 0x18;
const ATTRIBUTE_VOLUME: u8 = 0x08;
const ATTRIBUTE_DIRECTORY: u8 = 0x10;
const LONG_NAME_ATTRIBUTES: u8 = 0x0F;
const LONG_NAME_LAST: u8 = 0x40;
const RECORD_END: u8 = 0x00;
const RECORD_DELETED: u8 = 0xE5;
const KANJI_LEAD_E5: u8 = 0x05;
const NAME_IS_LOWER: u8 = 0x08;
const EXTENSION_IS_LOWER: u8 = 0x10;
const MEDIA_LOWEST: u32 = 0xF0;
const DOT: &[u8; SHORT_NAME] = b".          ";
const DOT_DOT: &[u8; SHORT_NAME] = b"..         ";
const BLANK: &[u8; SHORT_NAME] = b"           ";
const FAT_BLOCK: u64 = 1 << 16;
const FAT_BLOCKS_CACHED: usize = 4;
const RUN_LIMIT: u64 = 1 << 20;
const PAGE_LOG: u32 = 15;
const PAGE_WORDS: usize = 1 << (PAGE_LOG - 6);
const NO_PARENT: u32 = u32::MAX;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn power_of_two_log(value: u32, smallest: u32, largest: u32) -> Option<u32> {
    (smallest..=largest).find(|log| 1u32 << log == value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BootSector {
    pub(crate) fat_bits: u32,
    pub(crate) sector_log: u32,
    pub(crate) cluster_sectors_log: u32,
    pub(crate) cluster_log: u32,
    pub(crate) fats: u32,
    pub(crate) media: u8,
    pub(crate) reserved_sectors: u32,
    pub(crate) sectors: u32,
    pub(crate) fat_sectors: u32,
    pub(crate) root_sector: u32,
    pub(crate) root_sectors: u32,
    pub(crate) data_sector: u32,
    pub(crate) fat_size: u32,
    pub(crate) bad_cluster: u32,
    pub(crate) flags: u16,
    pub(crate) root_cluster: u32,
}

fn sectors_for(bytes: u32, sector_log: u32) -> u32 {
    (bytes + (1 << sector_log) - 1) >> sector_log
}

impl BootSector {
    fn fat32(&self) -> bool {
        self.fat_bits == 32
    }

    fn table_sectors(&self) -> u32 {
        sectors_for(
            (self.fat_size * (self.fat_bits / 4)).div_ceil(2),
            self.sector_log,
        )
    }

    // EXTERNAL(7-Zip): the active FAT of a FAT32 volume without mirroring may be
    // the one just past the last FAT, as 7-Zip's `GetFatSector` allows.
    pub(crate) fn fat_sector(&self) -> u32 {
        let active = u32::from(self.flags & 0xF);
        let index = if self.fat32() && self.flags & 0x80 != 0 && active <= self.fats {
            active
        } else {
            0
        };
        self.reserved_sectors + index * self.fat_sectors
    }

    pub(crate) fn fat_bytes_read(&self) -> u64 {
        let sector = 1u64 << self.sector_log;
        if self.fat32() {
            let entries = u64::from(self.fat_size);
            let whole = entries / FAT32_READ_ENTRIES;
            let rest = entries % FAT32_READ_ENTRIES;
            whole * FAT32_READ_ENTRIES * 4 + (rest * 4).div_ceil(sector) * sector
        } else {
            u64::from(self.table_sectors()) << self.sector_log
        }
    }

    pub(crate) fn valid_cluster(&self, cluster: u32) -> bool {
        (FIRST_CLUSTER..self.fat_size).contains(&cluster)
    }

    pub(crate) fn chain_end(&self, value: u32) -> bool {
        value > self.bad_cluster
    }

    pub(crate) fn cluster_offset(&self, cluster: u32) -> u64 {
        (u64::from(self.data_sector)
            + (u64::from(cluster - FIRST_CLUSTER) << self.cluster_sectors_log))
            << self.sector_log
    }
}

pub(crate) fn boot_sector(p: &[u8; BOOT_SECTOR]) -> Field<BootSector> {
    field_check(
        le16(p, 0x1FE) == 0xAA55,
        "the boot sector lacks its signature",
    )?;
    let code_offset = match p[0] {
        0xE9 => 3 + i32::from(le16(p, 1) as i16),
        0xEB => {
            field_check(p[2] == 0x90, "the boot sector's jump is not a FAT jump")?;
            2 + i32::from(p[1] as i8)
        }
        _ => return Err(BadField("the boot sector does not start with a jump")),
    };
    let sector_log = power_of_two_log(u32::from(le16(p, 11)), 9, 12)
        .ok_or(BadField("the sector size is not 512 to 4096 bytes"))?;
    let cluster_sectors_log = power_of_two_log(u32::from(p[13]), 0, 7)
        .ok_or(BadField("the sectors per cluster are not a power of two"))?;
    let cluster_log = sector_log + cluster_sectors_log;
    field_check(
        cluster_log <= LARGEST_CLUSTER_LOG,
        "a cluster is larger than 256 KiB",
    )?;
    let reserved_sectors = u32::from(le16(p, 14));
    field_check(reserved_sectors != 0, "the volume reserves no sectors")?;
    let fats = u32::from(p[16]);
    field_check(
        (1..=4).contains(&fats),
        "the volume holds no FAT or more than 4",
    )?;
    let plain_offset = code_offset == 0 || code_offset == if p[0] == 0xEB { 2 } else { 3 };
    let root_entries = u32::from(le16(p, 17));
    let (mut fat_bits, root_sectors) = if root_entries == 0 {
        field_check(
            code_offset >= 90 || plain_offset,
            "the boot code overlaps FAT32 fields",
        )?;
        (32, 0)
    } else {
        field_check(
            code_offset >= 38 || plain_offset,
            "the boot code overlaps FAT fields",
        )?;
        let shift = sector_log - 5;
        field_check(
            root_entries & ((1 << shift) - 1) == 0,
            "the root folder does not fill whole sectors",
        )?;
        (0, root_entries >> shift)
    };
    let mut sectors = u32::from(le16(p, 19));
    if sectors == 0 {
        sectors = le32(p, 32);
    }
    let media = p[21];
    let mut fat_sectors = u32::from(le16(p, 22));
    let mut fields_end = 36;
    let (mut flags, mut root_cluster) = (0, 0);
    if fat_bits == 32 {
        field_check(fat_sectors == 0, "a FAT32 volume gives a 16-bit FAT size")?;
        fat_sectors = le32(p, 36);
        field_check(fat_sectors < FAT32_FAT_SECTOR_LIMIT, "the FAT is too large")?;
        flags = le16(p, 40);
        field_check(le16(p, 42) == 0, "the FAT32 version is not 0")?;
        root_cluster = le32(p, 44);
        field_check(
            p[52..64].iter().all(|byte| *byte == 0),
            "the FAT32 reserved bytes are not zero",
        )?;
        fields_end += 28;
    }
    if code_offset >= fields_end + 3 && p[fields_end as usize + 2] == 0x29 {
        field_check(
            code_offset >= fields_end + 26,
            "the boot code overlaps the volume fields",
        )?;
    }
    field_check(fat_sectors != 0, "the FAT has no sectors")?;
    let root_sector = reserved_sectors + fat_sectors * fats;
    let data_sector = root_sector + root_sectors;
    field_check(sectors >= data_sector, "the volume ends before its data")?;
    let clusters = (sectors - data_sector) >> cluster_sectors_log;
    let mut bad_cluster = FAT32_BAD_CLUSTER;
    if fat_bits != 32 {
        field_check(
            clusters < FAT16_CLUSTER_LIMIT,
            "a FAT16 volume has too many clusters",
        )?;
        fat_bits = if clusters < FAT12_CLUSTER_LIMIT {
            12
        } else {
            16
        };
        bad_cluster &= (1 << fat_bits) - 1;
    }
    let fat_size = clusters
        .checked_add(FIRST_CLUSTER)
        .ok_or(BadField("the volume has too many clusters"))?;
    field_check(fat_size <= bad_cluster, "the volume has too many clusters")?;
    let mut boot = BootSector {
        fat_bits,
        sector_log,
        cluster_sectors_log,
        cluster_log,
        fats,
        media,
        reserved_sectors,
        sectors,
        fat_sectors,
        root_sector,
        root_sectors,
        data_sector,
        fat_size,
        bad_cluster,
        flags,
        root_cluster,
    };
    if boot.table_sectors() > fat_sectors {
        let held = (u64::from(fat_sectors) << (3 + sector_log)) / u64::from(fat_bits);
        boot.fat_size =
            u32::try_from(held).map_err(|_| BadField("the FAT holds too many entries"))?;
    }
    Ok(boot)
}

pub(crate) fn fat_entry_offset(fat_bits: u32, index: u32) -> u64 {
    u64::from(index) * u64::from(fat_bits) / 8
}

pub(crate) fn fat_entry_value(fat_bits: u32, index: u32, bytes: [u8; 4]) -> u32 {
    match fat_bits {
        12 => (u32::from(u16::from_le_bytes([bytes[0], bytes[1]])) >> ((index & 1) << 2)) & 0xFFF,
        16 => u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
        _ => u32::from_le_bytes(bytes) & FAT32_ENTRY_MASK,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Short {
    pub(crate) name: [u8; SHORT_NAME],
    pub(crate) attributes: u8,
    pub(crate) case: u8,
    pub(crate) size: u32,
    pub(crate) cluster: u32,
    pub(crate) modified: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LongPart {
    pub(crate) index: usize,
    pub(crate) last: bool,
    pub(crate) checksum: u8,
    pub(crate) holds_name: bool,
    pub(crate) units: [u16; LONG_NAME_UNITS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    End,
    Deleted,
    Long(LongPart),
    Volume,
    Dots { directory: bool },
    Item(Short),
}

pub(crate) fn record(p: &[u8; RECORD], fat32: bool) -> Field<Record> {
    match p[0] {
        RECORD_END => return Ok(Record::End),
        RECORD_DELETED => return Ok(Record::Deleted),
        _ => {}
    }
    let attributes = p[11];
    if attributes & 0x3F == LONG_NAME_ATTRIBUTES {
        let index = usize::from(p[0] & 0x3F);
        field_check(
            (1..=LONG_NAME_RECORDS).contains(&index) && p[0] <= 0x7F && le16(p, 26) == 0,
            "a long name record is malformed",
        )?;
        let mut units = [0u16; LONG_NAME_UNITS];
        for (unit, at) in units.iter_mut().zip(
            (1..11)
                .step_by(2)
                .chain((14..26).step_by(2))
                .chain((28..32).step_by(2)),
        ) {
            *unit = le16(p, at);
        }
        return Ok(Record::Long(LongPart {
            index,
            last: p[0] & LONG_NAME_LAST != 0,
            checksum: p[13],
            holds_name: p[12] == 0,
            units,
        }));
    }
    let kind = attributes & ATTRIBUTE_TYPE;
    field_check(
        kind != ATTRIBUTE_TYPE,
        "a record is marked both a folder and a volume label",
    )?;
    let name: [u8; SHORT_NAME] = p[..SHORT_NAME].try_into().unwrap();
    if kind == ATTRIBUTE_VOLUME {
        return Ok(Record::Volume);
    }
    if &name == DOT || &name == DOT_DOT {
        return Ok(Record::Dots {
            directory: kind == ATTRIBUTE_DIRECTORY,
        });
    }
    let mut stored = name;
    if stored[0] == KANJI_LEAD_E5 {
        stored[0] = RECORD_DELETED;
    }
    let high = if fat32 {
        u32::from(le16(p, 20)) << 16
    } else {
        0
    };
    Ok(Record::Item(Short {
        name: stored,
        attributes,
        case: p[12],
        size: le32(p, 28),
        cluster: u32::from(le16(p, 26)) | high,
        modified: le32(p, 22),
    }))
}

pub(crate) fn short_name_checksum(name: &[u8; SHORT_NAME]) -> u8 {
    name.iter().fold(0u8, |sum, byte| {
        (sum << 7).wrapping_add(sum >> 1).wrapping_add(*byte)
    })
}

pub(crate) fn long_name_length(units: &mut [u16], words: usize) -> usize {
    let mut end = 0;
    while end < words {
        match units[end] {
            0 => break,
            0xFFFF => return 0,
            _ => end += 1,
        }
    }
    let padding = units.get(end + 1..words).unwrap_or_default();
    if end == 0 || padding.iter().any(|unit| *unit != 0xFFFF) {
        return 0;
    }
    units[end] = 0;
    end + 1
}

fn trimmed(bytes: &[u8], lower: bool, into: &mut [u8; SHORT_NAME + 1], length: &mut usize) {
    let mut kept = bytes.len();
    while kept > 0 && bytes[kept - 1] == b' ' {
        kept -= 1;
    }
    for byte in &bytes[..kept] {
        into[*length] = if lower {
            byte.to_ascii_lowercase()
        } else {
            *byte
        };
        *length += 1;
    }
}

pub(crate) fn short_name_bytes(
    name: &[u8; SHORT_NAME],
    case: u8,
) -> Field<([u8; SHORT_NAME + 1], usize)> {
    let mut spelled = [0u8; SHORT_NAME + 1];
    let mut length = 0;
    trimmed(
        &name[..8],
        case & NAME_IS_LOWER != 0,
        &mut spelled,
        &mut length,
    );
    let base = length;
    spelled[length] = b'.';
    length += 1;
    trimmed(
        &name[8..],
        case & EXTENSION_IS_LOWER != 0,
        &mut spelled,
        &mut length,
    );
    if length == base + 1 {
        length = base;
    }
    for byte in &spelled[..length] {
        field_check(
            (1..0x80).contains(byte),
            "a short name holds a byte outside ASCII, which 7-Zip reads in the OEM code page",
        )?;
    }
    Ok((spelled, length))
}

fn short_name(name: &[u8; SHORT_NAME], case: u8) -> Field<String> {
    let (spelled, length) = short_name_bytes(name, case)?;
    Ok(spelled[..length].iter().copied().map(char::from).collect())
}

struct Node {
    parent: u32,
    name: Box<str>,
    directory: bool,
    read_only: bool,
    size: u32,
    cluster: u32,
    modified: u32,
}

struct FatTable {
    start: u64,
    bits: u32,
    blocks: Vec<(u64, Vec<u8>)>,
    victim: usize,
}

#[derive(Default)]
struct ClusterMap {
    pages: HashMap<u32, Box<[u64; PAGE_WORDS]>>,
}

impl ClusterMap {
    fn claim(&mut self, cluster: u32) -> bool {
        let page = self
            .pages
            .entry(cluster >> PAGE_LOG)
            .or_insert_with(|| Box::new([0; PAGE_WORDS]));
        let bit = cluster & ((1 << PAGE_LOG) - 1);
        let word = &mut page[(bit >> 6) as usize];
        let mask = 1u64 << (bit & 63);
        let fresh = *word & mask == 0;
        *word |= mask;
        fresh
    }
}

pub(crate) struct Volume<R> {
    data: R,
    boot: BootSector,
    table: FatTable,
    folder_clusters: HashSet<u32>,
    claimed: ClusterMap,
    nodes: Vec<Node>,
    name_bytes: usize,
}

fn read_exact_at<R: Read + Seek>(
    data: &mut R,
    offset: u64,
    buffer: &mut [u8],
    what: &str,
) -> Result<()> {
    data.seek(SeekFrom::Start(offset))
        .and_then(|_| data.read_exact(buffer))
        .with_context(|| format!("the FAT volume ends inside its {what}"))
}

impl<R: Read + Seek> Volume<R> {
    pub(crate) fn open(mut data: R) -> Result<Volume<R>> {
        let length = data.seek(SeekFrom::End(0))?;
        let mut sector = [0u8; BOOT_SECTOR];
        read_exact_at(&mut data, 0, &mut sector, "boot sector")?;
        let boot = boot_sector(&sector).map_err(anyhow::Error::from)?;
        let start = u64::from(boot.fat_sector()) << boot.sector_log;
        let end = start + boot.fat_bytes_read();
        ensure!(end <= length, "the FAT volume ends inside its FAT");
        let mut probe = vec![0u8; FAT_BLOCK as usize];
        let mut at = start;
        while at < end {
            let piece = (end - at).min(FAT_BLOCK) as usize;
            read_exact_at(&mut data, at, &mut probe[..piece], "FAT")?;
            at += piece as u64;
        }
        let mut volume = Volume {
            data,
            boot,
            table: FatTable {
                start,
                bits: boot.fat_bits,
                blocks: Vec::new(),
                victim: 0,
            },
            folder_clusters: HashSet::new(),
            claimed: ClusterMap::default(),
            nodes: Vec::new(),
            name_bytes: 0,
        };
        let media = volume.entry(0)? & 0xFF;
        ensure!(
            media == u32::from(boot.media) || media >= MEDIA_LOWEST,
            "the FAT's first entry does not name the volume's media"
        );
        volume.read_folder(NO_PARENT, boot.root_cluster, 0)?;
        Ok(volume)
    }

    fn table_byte(&mut self, offset: u64) -> Result<u8> {
        let block = offset / FAT_BLOCK;
        let within = (offset % FAT_BLOCK) as usize;
        if let Some((_, bytes)) = self.table.blocks.iter().find(|(index, _)| *index == block) {
            return Ok(bytes[within]);
        }
        let from = self.table.start + block * FAT_BLOCK;
        let mut bytes = vec![0u8; FAT_BLOCK as usize];
        self.data.seek(SeekFrom::Start(from))?;
        let mut filled = 0;
        while filled < bytes.len() {
            match self.data.read(&mut bytes[filled..])? {
                0 => break,
                read => filled += read,
            }
        }
        ensure!(within < filled, "the FAT volume ends inside its FAT");
        let byte = bytes[within];
        if self.table.blocks.len() < FAT_BLOCKS_CACHED {
            self.table.blocks.push((block, bytes));
        } else {
            self.table.blocks[self.table.victim] = (block, bytes);
            self.table.victim = (self.table.victim + 1) % FAT_BLOCKS_CACHED;
        }
        Ok(byte)
    }

    fn entry(&mut self, index: u32) -> Result<u32> {
        let offset = fat_entry_offset(self.table.bits, index);
        let width = if self.table.bits == 32 { 4 } else { 2 };
        let mut bytes = [0u8; 4];
        for (place, byte) in bytes.iter_mut().take(width).enumerate() {
            *byte = self.table_byte(offset + place as u64)?;
        }
        Ok(fat_entry_value(self.table.bits, index, bytes))
    }

    fn charge_name(&mut self, name: &str) -> Result<()> {
        self.name_bytes += name.len();
        ensure!(
            self.name_bytes <= MAX_NAME_BYTES,
            "the FAT volume's names exceed 64 MiB"
        );
        ensure!(
            self.nodes.len() < crate::native_cpio::tree::MAX_ENTRIES,
            "the FAT volume holds more than {} entries",
            crate::native_cpio::tree::MAX_ENTRIES
        );
        Ok(())
    }

    fn item_name(short: &Short, long: Option<&[u16]>, unnamed: usize) -> Result<String> {
        let mut name = match long {
            Some(units) => {
                String::from_utf16(units).context("a FAT long name is not valid UTF-16")?
            }
            None if &short.name == BLANK => format!("[NONAME]-{unnamed}"),
            None => short_name(&short.name, short.case).map_err(anyhow::Error::from)?,
        };
        if name.is_empty() {
            name.push('_');
        }
        let name = name.replace('/', "_");
        ensure!(
            !name.contains(['\\', ':', '\0']) && name != "." && name != "..",
            "FAT name {name:?} is not a safe file name; 7-Zip writes it, and Zipper refuses it"
        );
        Ok(name)
    }

    fn read_folder(&mut self, parent: u32, first: u32, level: u32) -> Result<()> {
        let start = self.nodes.len();
        ensure!(
            start < ITEM_LIMIT && level <= DEPTH_LIMIT,
            "the FAT volume nests folders more than {DEPTH_LIMIT} deep"
        );
        let boot = self.boot;
        let by_cluster = boot.fat32() || parent != NO_PARENT;
        let block = if by_cluster {
            1usize << boot.cluster_log
        } else {
            1usize << boot.sector_log
        };
        let mut buffer = vec![0u8; block];
        let mut long = [0u16; LONG_NAME_BUFFER];
        let mut folder_bytes = 0u32;
        let mut root_sector = 0;
        let mut cluster = first;
        let mut long_records = 0usize;
        let mut long_index = 0usize;
        let mut long_checksum: Option<u8> = None;
        let mut record_error = false;
        let mut volume_seen = false;
        'blocks: loop {
            let offset = if by_cluster {
                if boot.chain_end(cluster) {
                    break;
                }
                ensure!(
                    boot.valid_cluster(cluster),
                    "a FAT folder names cluster {cluster}, outside the volume"
                );
                let next = self.entry(cluster)?;
                ensure!(
                    self.folder_clusters.insert(cluster),
                    "FAT folders claim cluster {cluster} twice, so their chains loop or cross"
                );
                let offset = boot.cluster_offset(cluster);
                cluster = next;
                offset
            } else {
                if root_sector >= boot.root_sectors {
                    break;
                }
                root_sector += 1;
                u64::from(boot.root_sector + root_sector - 1) << boot.sector_log
            };
            ensure!(
                folder_bytes < FOLDER_BYTES_LIMIT,
                "a FAT folder is longer than 2 MiB"
            );
            read_exact_at(&mut self.data, offset, &mut buffer, "folders")?;
            folder_bytes += block as u32;
            for chunk in buffer.chunks_exact(RECORD) {
                if record_error {
                    long_records = 0;
                    long_index = 0;
                    long_checksum = None;
                }
                let found =
                    record(chunk.try_into().unwrap(), boot.fat32()).map_err(anyhow::Error::from)?;
                record_error = true;
                match found {
                    Record::End => break 'blocks,
                    Record::Deleted => {
                        if long_records == 0 {
                            record_error = false;
                        }
                        continue;
                    }
                    Record::Long(part) => {
                        if long_records == 0 {
                            if !part.last {
                                continue;
                            }
                            long_records = part.index;
                        } else if part.last || part.index != long_index {
                            bail!("a FAT long name's records are out of order");
                        }
                        long_index = part.index - 1;
                        if part.holds_name {
                            let at = long_index * LONG_NAME_UNITS;
                            long[at..at + LONG_NAME_UNITS].copy_from_slice(&part.units);
                            if part.last {
                                long_checksum = Some(part.checksum);
                            }
                            if long_checksum == Some(part.checksum) {
                                record_error = false;
                            }
                        } else {
                            long_checksum = None;
                        }
                        continue;
                    }
                    Record::Volume => {
                        if !volume_seen && level == 0 {
                            volume_seen = true;
                            record_error = false;
                        }
                    }
                    Record::Dots { directory } => {
                        if long_records == 0 && directory {
                            record_error = false;
                        }
                    }
                    Record::Item(short) => {
                        let mut long_units = None;
                        if long_records == 0 {
                            record_error = false;
                        } else if long_index == 0
                            && long_checksum == Some(short_name_checksum(&short.name))
                        {
                            let units = long_name_length(&mut long, long_records * LONG_NAME_UNITS);
                            if units > 1 {
                                long_units = Some(units - 1);
                                record_error = false;
                            }
                        }
                        let unnamed = self.nodes.len() - start;
                        let name = Self::item_name(
                            &short,
                            long_units.map(|units| &long[..units]),
                            unnamed,
                        )?;
                        self.charge_name(&name)?;
                        self.nodes.push(Node {
                            parent,
                            name: name.into_boxed_str(),
                            directory: short.attributes & ATTRIBUTE_DIRECTORY != 0,
                            read_only: short.attributes & ATTRIBUTE_READ_ONLY != 0,
                            size: short.size,
                            cluster: short.cluster,
                            modified: short.modified,
                        });
                    }
                }
                long_records = 0;
            }
        }
        let end = self.nodes.len();
        let mut names: Vec<&str> = self.nodes[start..end]
            .iter()
            .map(|node| &*node.name)
            .collect();
        names.sort_unstable();
        if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
            bail!("a FAT folder holds two entries named {:?}", pair[0]);
        }
        for index in start..end {
            if self.nodes[index].directory {
                let cluster = self.nodes[index].cluster;
                self.read_folder(index as u32, cluster, level + 1)?;
            }
        }
        Ok(())
    }

    pub(crate) fn extract(&mut self, tree: &mut Tree) -> Result<()> {
        let mut folders: HashMap<u32, String> = HashMap::new();
        for index in 0..self.nodes.len() {
            tree.check()?;
            let node = &self.nodes[index];
            let path = match node.parent {
                NO_PARENT => node.name.to_string(),
                parent => format!("{}/{}", folders[&parent], node.name),
            };
            let modified =
                crate::dos_local_time((node.modified >> 16) as u16, node.modified as u16);
            if node.directory {
                tree.directory(&path, DIRECTORY_MODE, modified)?;
                folders.insert(index as u32, path);
                continue;
            }
            let (size, cluster) = (node.size, node.cluster);
            let mode = if node.read_only {
                READ_ONLY_MODE
            } else {
                FILE_MODE
            };
            if size == 0 {
                ensure!(cluster == 0, "empty FAT file {path:?} names a cluster");
            }
            let clusters = size.div_ceil(1 << self.boot.cluster_log);
            let mut data = FileData {
                volume: self,
                next: cluster,
                clusters_left: clusters,
                run_offset: 0,
                run_left: 0,
                left: u64::from(size),
            };
            tree.file(&path, u64::from(size), mode, modified, &mut data)?;
        }
        Ok(())
    }
}

struct FileData<'v, R> {
    volume: &'v mut Volume<R>,
    next: u32,
    clusters_left: u32,
    run_offset: u64,
    run_left: u64,
    left: u64,
}

impl<R: Read + Seek> FileData<'_, R> {
    fn take(&mut self) -> Result<u32> {
        let boot = self.volume.boot;
        let cluster = self.next;
        ensure!(
            !boot.chain_end(cluster),
            "a FAT file's cluster chain ends before its size does"
        );
        ensure!(
            boot.valid_cluster(cluster),
            "a FAT file names cluster {cluster}, outside the volume"
        );
        ensure!(
            !self.volume.folder_clusters.contains(&cluster),
            "a FAT file uses cluster {cluster}, which a folder holds"
        );
        ensure!(
            self.volume.claimed.claim(cluster),
            "FAT files share cluster {cluster}, so their chains loop or cross"
        );
        let following = self.volume.entry(cluster)?;
        self.clusters_left -= 1;
        if self.clusters_left == 0 {
            ensure!(
                boot.chain_end(following),
                "a FAT file's cluster chain does not end where its size does"
            );
        }
        self.next = following;
        Ok(cluster)
    }

    fn take_run(&mut self) -> Result<()> {
        let boot = self.volume.boot;
        let cluster_bytes = 1u64 << boot.cluster_log;
        let first = self.take()?;
        let mut last = first;
        let mut bytes = cluster_bytes;
        while self.clusters_left > 0 && self.next == last.wrapping_add(1) && bytes < RUN_LIMIT {
            last = self.take()?;
            bytes += cluster_bytes;
        }
        self.run_offset = boot.cluster_offset(first);
        self.run_left = bytes.min(self.left);
        Ok(())
    }
}

impl<R: Read + Seek> Read for FileData<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 || buffer.is_empty() {
            return Ok(0);
        }
        if self.run_left == 0 {
            self.take_run().map_err(std::io::Error::other)?;
        }
        let want = (buffer.len() as u64).min(self.run_left) as usize;
        let data = &mut self.volume.data;
        data.seek(SeekFrom::Start(self.run_offset))?;
        data.read_exact(&mut buffer[..want])?;
        self.run_offset += want as u64;
        self.run_left -= want as u64;
        self.left -= want as u64;
        Ok(want)
    }
}

pub(crate) fn is_boot_sector(sector: &[u8; BOOT_SECTOR]) -> bool {
    boot_sector(sector).is_ok()
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn fat16_boot() -> [u8; BOOT_SECTOR] {
        let mut p = [0u8; BOOT_SECTOR];
        p[0] = 0xEB;
        p[1] = 0x3C;
        p[2] = 0x90;
        p[11..13].copy_from_slice(&512u16.to_le_bytes());
        p[13] = 4;
        p[14..16].copy_from_slice(&1u16.to_le_bytes());
        p[16] = 2;
        p[17..19].copy_from_slice(&512u16.to_le_bytes());
        p[19..21].copy_from_slice(&40_000u16.to_le_bytes());
        p[21] = 0xF8;
        p[22..24].copy_from_slice(&40u16.to_le_bytes());
        p[0x1FE] = 0x55;
        p[0x1FF] = 0xAA;
        p
    }

    #[test]
    fn a_fat16_boot_sector_reads_its_layout() {
        let boot = boot_sector(&fat16_boot()).unwrap();
        assert_eq!(boot.fat_bits, 16);
        assert_eq!(boot.root_sector, 81);
        assert_eq!(boot.data_sector, 81 + 32);
        assert_eq!(boot.fat_size, (40_000 - 113) / 4 + 2);
        assert_eq!(boot.fat_sector(), 1);
    }

    #[test]
    fn a_short_fat_falls_back_to_the_entries_it_holds() {
        let mut p = fat16_boot();
        p[22..24].copy_from_slice(&10u16.to_le_bytes());
        let boot = boot_sector(&p).unwrap();
        assert_eq!(boot.fat_size, 10 * 512 * 8 / 16);
    }

    #[test]
    fn checksums_and_long_names_follow_the_vfat_rules() {
        assert_eq!(short_name_checksum(b"ALONGF~1TXT"), {
            let mut sum = 0u8;
            for byte in b"ALONGF~1TXT" {
                sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*byte);
            }
            sum
        });
        let mut units = [0u16; 27];
        units[..3].copy_from_slice(&[b'a' as u16, b'b' as u16, 0]);
        units[3..26].fill(0xFFFF);
        assert_eq!(long_name_length(&mut units, 26), 3);
        units[5] = 0;
        assert_eq!(long_name_length(&mut units, 26), 0);
        let mut full = [b'x' as u16; 27];
        assert_eq!(long_name_length(&mut full, 26), 27);
        assert_eq!(full[26], 0);
        let mut empty = [0u16; 14];
        assert_eq!(long_name_length(&mut empty, 13), 0);
    }

    #[test]
    fn short_names_trim_and_lower_as_7zip_spells_them() {
        assert_eq!(short_name(b"HELLO   TXT", 0x08).unwrap(), "hello.TXT");
        assert_eq!(short_name(b"HELLO   TXT", 0x18).unwrap(), "hello.txt");
        assert_eq!(short_name(b"README     ", 0).unwrap(), "README");
        assert_eq!(short_name(b"        TXT", 0).unwrap(), ".TXT");
        assert!(short_name(b"\xE5BC     TXT", 0).is_err());
    }

    #[test]
    fn fat12_entries_straddle_bytes_as_the_specification_packs_them() {
        let packed = [0x34, 0x12, 0xAB];
        assert_eq!(fat_entry_value(12, 0, [packed[0], packed[1], 0, 0]), 0x234);
        assert_eq!(fat_entry_value(12, 1, [packed[1], packed[2], 0, 0]), 0xAB1);
        assert_eq!(fat_entry_offset(12, 1), 1);
        assert_eq!(fat_entry_offset(12, 2), 3);
        assert_eq!(
            fat_entry_value(32, 0, [0xFF, 0xFF, 0xFF, 0xFF]),
            FAT32_ENTRY_MASK
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(14)]
    fn boot_sectors_never_panic_and_keep_every_cluster_and_fat_entry_inside_the_volume() {
        let bytes: [u8; BOOT_SECTOR] = kani::any();
        if let Ok(boot) = boot_sector(&bytes) {
            assert!(matches!(boot.fat_bits, 12 | 16 | 32));
            assert!(boot.cluster_log <= LARGEST_CLUSTER_LOG);
            assert!(boot.data_sector <= boot.sectors);
            assert!(boot.fat_size <= boot.bad_cluster);
            let fat_start = u64::from(boot.fat_sector()) << boot.sector_log;
            assert!(fat_start.checked_add(boot.fat_bytes_read()).is_some());
            let cluster: u32 = kani::any();
            kani::assume(boot.valid_cluster(cluster));
            let end = boot.cluster_offset(cluster) + (1 << boot.cluster_log);
            assert!(end <= u64::from(boot.sectors) << boot.sector_log);
            let width = if boot.fat32() { 4 } else { 2 };
            assert!(fat_entry_offset(boot.fat_bits, cluster) + width <= boot.fat_bytes_read());
            let value = fat_entry_value(boot.fat_bits, cluster, kani::any());
            assert!(value < 1 << boot.fat_bits.min(28));
        }
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn folder_records_never_panic_and_keep_their_fields() {
        let bytes: [u8; RECORD] = kani::any();
        let fat32: bool = kani::any();
        match record(&bytes, fat32) {
            Ok(Record::Long(part)) => assert!((1..=LONG_NAME_RECORDS).contains(&part.index)),
            Ok(Record::Item(short)) => {
                assert!(fat32 || short.cluster <= u32::from(u16::MAX));
                assert!(short.name[0] != KANJI_LEAD_E5);
            }
            _ => {}
        }
    }

    #[kani::proof]
    #[kani::unwind(28)]
    fn long_names_never_panic_and_end_at_their_terminator() {
        let mut units: [u16; 2 * LONG_NAME_UNITS + 1] = kani::any();
        let words = if kani::any() {
            LONG_NAME_UNITS
        } else {
            2 * LONG_NAME_UNITS
        };
        let original = units;
        let length = long_name_length(&mut units, words);
        assert!(length <= words + 1);
        if length > 0 {
            assert!(length > 1);
            assert!(units[length - 1] == 0);
            for index in 0..length - 1 {
                assert!(original[index] != 0 && original[index] != 0xFFFF);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(13)]
    fn short_names_never_panic_and_stay_ascii_within_twelve_bytes() {
        let name: [u8; SHORT_NAME] = kani::any();
        let case: u8 = kani::any();
        if let Ok((spelled, length)) = short_name_bytes(&name, case) {
            assert!(length <= SHORT_NAME + 1);
            for byte in &spelled[..length] {
                assert!((1..0x80).contains(byte));
            }
        }
        let _ = short_name_checksum(&name);
    }
}
