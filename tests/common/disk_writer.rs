#![allow(dead_code)]
use std::collections::BTreeMap;

pub const SECTOR: usize = 512;
const DOS_TIME: u32 = (44 << 25) | (5 << 21) | (6 << 16) | (7 << 11) | (8 << 5) | 5;

#[derive(Clone, Debug)]
pub enum Item {
    File {
        name: String,
        data: Vec<u8>,
        read_only: bool,
    },
    Folder {
        name: String,
        items: Vec<Item>,
    },
}

pub fn file(name: &str, data: &[u8]) -> Item {
    Item::File {
        name: name.to_owned(),
        data: data.to_vec(),
        read_only: false,
    }
}

pub fn folder(name: &str, items: Vec<Item>) -> Item {
    Item::Folder {
        name: name.to_owned(),
        items,
    }
}

#[derive(Clone, Debug)]
pub struct Volume {
    pub bits: u32,
    pub cluster_sectors: u32,
    pub sectors: u32,
    pub root_entries: u32,
    pub fragment: bool,
    pub deleted: bool,
    pub label: Option<&'static str>,
}

impl Volume {
    pub fn new(bits: u32, sectors: u32) -> Volume {
        Volume {
            bits,
            cluster_sectors: if bits == 32 { 1 } else { 4 },
            sectors,
            root_entries: if bits == 32 { 0 } else { 512 },
            fragment: false,
            deleted: false,
            label: Some("ZIPPERTEST "),
        }
    }
}

pub struct Built {
    pub bytes: Vec<u8>,
    pub bits: u32,
    pub fat_at: usize,
    pub fat_bytes: usize,
    pub fats: usize,
    pub data_at: usize,
    pub cluster_bytes: usize,
    pub root_at: usize,
    pub root_cluster: u32,
    pub clusters: u32,
    pub records: BTreeMap<String, usize>,
    pub chains: BTreeMap<String, Vec<u32>>,
}

impl Built {
    pub fn end_of_chain(&self) -> u32 {
        match self.bits {
            12 => 0xFFF,
            16 => 0xFFFF,
            _ => 0x0FFF_FFFF,
        }
    }

    pub fn entry(&self, cluster: u32) -> u32 {
        read_entry(&self.bytes[self.fat_at..], self.bits, cluster)
    }

    pub fn set_entry(&mut self, cluster: u32, value: u32) {
        for copy in 0..self.fats {
            let at = self.fat_at + copy * self.fat_bytes;
            write_entry(
                &mut self.bytes[at..at + self.fat_bytes],
                self.bits,
                cluster,
                value,
            );
        }
    }

    pub fn cluster_at(&self, cluster: u32) -> usize {
        self.data_at + (cluster as usize - 2) * self.cluster_bytes
    }

    pub fn record(&self, path: &str) -> usize {
        self.records[path]
    }
}

fn read_entry(fat: &[u8], bits: u32, cluster: u32) -> u32 {
    let index = cluster as usize;
    match bits {
        12 => {
            let at = index * 3 / 2;
            let pair = u16::from_le_bytes([fat[at], fat[at + 1]]);
            u32::from(if index.is_multiple_of(2) {
                pair & 0xFFF
            } else {
                pair >> 4
            })
        }
        16 => u32::from(u16::from_le_bytes([fat[2 * index], fat[2 * index + 1]])),
        _ => u32::from_le_bytes(fat[4 * index..4 * index + 4].try_into().unwrap()) & 0x0FFF_FFFF,
    }
}

fn write_entry(fat: &mut [u8], bits: u32, cluster: u32, value: u32) {
    let index = cluster as usize;
    match bits {
        12 => {
            let at = index * 3 / 2;
            let mut pair = u16::from_le_bytes([fat[at], fat[at + 1]]);
            let value = (value & 0xFFF) as u16;
            pair = if index.is_multiple_of(2) {
                (pair & 0xF000) | value
            } else {
                (pair & 0x000F) | (value << 4)
            };
            fat[at..at + 2].copy_from_slice(&pair.to_le_bytes());
        }
        16 => fat[2 * index..2 * index + 2].copy_from_slice(&(value as u16).to_le_bytes()),
        _ => fat[4 * index..4 * index + 4].copy_from_slice(&value.to_le_bytes()),
    }
}

fn is_plain_short_name(name: &str) -> bool {
    let (base, extension) = match name.rsplit_once('.') {
        Some((base, extension)) => (base, extension),
        None => (name, ""),
    };
    let allowed = |part: &str| {
        part.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_-~!#$%&".contains(&byte)
        })
    };
    !base.is_empty()
        && base.len() <= 8
        && extension.len() <= 3
        && allowed(base)
        && allowed(extension)
        && !(name.contains('.') && extension.is_empty())
}

fn short_name(name: &str, taken: &mut std::collections::HashSet<[u8; 11]>) -> ([u8; 11], bool) {
    let mut short = [b' '; 11];
    if is_plain_short_name(name) {
        let (base, extension) = name.rsplit_once('.').unwrap_or((name, ""));
        short[..base.len()].copy_from_slice(base.as_bytes());
        short[8..8 + extension.len()].copy_from_slice(extension.as_bytes());
        taken.insert(short);
        return (short, false);
    }
    let clean = |part: &str, length: usize| -> Vec<u8> {
        part.bytes()
            .filter(|byte| byte.is_ascii_alphanumeric())
            .map(|byte| byte.to_ascii_uppercase())
            .take(length)
            .collect()
    };
    let (base, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let base = clean(base, 6);
    let extension = clean(extension, 3);
    for number in 1.. {
        let tail = format!("~{number}");
        let keep = base.len().min(8 - tail.len());
        let mut candidate = [b' '; 11];
        candidate[..keep].copy_from_slice(&base[..keep]);
        candidate[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        candidate[8..8 + extension.len()].copy_from_slice(&extension);
        if !taken.contains(&candidate) {
            taken.insert(candidate);
            return (candidate, true);
        }
    }
    unreachable!()
}

pub fn checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, byte| {
        ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*byte)
    })
}

pub fn long_records(name: &str, short: &[u8; 11]) -> Vec<[u8; 32]> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    if !units.len().is_multiple_of(13) {
        units.push(0);
    }
    while !units.len().is_multiple_of(13) {
        units.push(0xFFFF);
    }
    let count = units.len() / 13;
    let sum = checksum(short);
    (0..count)
        .rev()
        .map(|index| {
            let mut record = [0u8; 32];
            record[0] = (index + 1) as u8 | if index + 1 == count { 0x40 } else { 0 };
            record[11] = 0x0F;
            record[13] = sum;
            let piece = &units[index * 13..index * 13 + 13];
            let places = (1..11)
                .step_by(2)
                .chain((14..26).step_by(2))
                .chain((28..32).step_by(2));
            for (unit, at) in piece.iter().zip(places) {
                record[at..at + 2].copy_from_slice(&unit.to_le_bytes());
            }
            record
        })
        .collect()
}

pub fn short_record(name: &[u8; 11], attributes: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut record = [0u8; 32];
    record[..11].copy_from_slice(name);
    record[11] = attributes;
    record[14..18].copy_from_slice(&DOS_TIME.to_le_bytes());
    record[18..20].copy_from_slice(&((DOS_TIME >> 16) as u16).to_le_bytes());
    record[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    record[22..26].copy_from_slice(&DOS_TIME.to_le_bytes());
    record[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    record[28..32].copy_from_slice(&size.to_le_bytes());
    record
}

struct Layout {
    reserved: u32,
    fat_sectors: u32,
    root_sectors: u32,
    clusters: u32,
}

fn layout(volume: &Volume) -> Layout {
    let reserved = if volume.bits == 32 { 32 } else { 1 };
    let root_sectors = volume.root_entries * 32 / SECTOR as u32;
    let mut fat_sectors = 1;
    loop {
        let data = volume.sectors - reserved - 2 * fat_sectors - root_sectors;
        let clusters = data / volume.cluster_sectors;
        let needed =
            ((u64::from(clusters) + 2) * u64::from(volume.bits)).div_ceil(8 * SECTOR as u64) as u32;
        if needed <= fat_sectors {
            let fits = match volume.bits {
                12 => clusters < 0xFF5,
                16 => (0xFF5..0xFFF5).contains(&clusters),
                _ => true,
            };
            assert!(
                fits,
                "{clusters} clusters do not make a FAT{} volume",
                volume.bits
            );
            return Layout {
                reserved,
                fat_sectors,
                root_sectors,
                clusters,
            };
        }
        fat_sectors = needed;
    }
}

struct Writer {
    bytes: Vec<u8>,
    bits: u32,
    fat: Vec<u32>,
    free: Vec<u32>,
    data_at: usize,
    cluster_bytes: usize,
    records: BTreeMap<String, usize>,
    chains: BTreeMap<String, Vec<u32>>,
}

impl Writer {
    fn allocate(&mut self, count: usize) -> Vec<u32> {
        assert!(self.free.len() >= count, "the volume is full");
        let chain: Vec<u32> = self.free.drain(..count).collect();
        let end = match self.bits {
            12 => 0xFFF,
            16 => 0xFFFF,
            _ => 0x0FFF_FFFF,
        };
        for pair in chain.windows(2) {
            self.fat[pair[0] as usize] = pair[1];
        }
        if let Some(last) = chain.last() {
            self.fat[*last as usize] = end;
        }
        chain
    }

    fn write_chain(&mut self, chain: &[u32], data: &[u8]) {
        for (cluster, piece) in chain.iter().zip(data.chunks(self.cluster_bytes)) {
            let at = self.data_at + (*cluster as usize - 2) * self.cluster_bytes;
            self.bytes[at..at + piece.len()].copy_from_slice(piece);
        }
    }

    fn records(
        &mut self,
        items: &[Item],
        folder: &str,
        parent: u32,
        own: Option<u32>,
        deleted: bool,
    ) -> (Vec<u8>, Vec<(String, usize)>) {
        let mut records: Vec<u8> = Vec::new();
        let mut places = Vec::new();
        if let Some(own) = own {
            records.extend(short_record(b".          ", 0x10, own, 0));
            records.extend(short_record(b"..         ", 0x10, parent, 0));
        }
        let mut taken = std::collections::HashSet::new();
        let own_cluster = own.unwrap_or(0);
        let _ = parent;
        for item in items {
            let (name, attributes, cluster, size) = match item {
                Item::File {
                    name,
                    data,
                    read_only,
                } => {
                    let count = data.len().div_ceil(self.cluster_bytes);
                    let chain = self.allocate(count);
                    self.write_chain(&chain, data);
                    let path = format!("{folder}{name}");
                    let first = chain.first().copied().unwrap_or(0);
                    self.chains.insert(path, chain);
                    (name, 0x20 | u8::from(*read_only), first, data.len() as u32)
                }
                Item::Folder { name, items } => {
                    let path = format!("{folder}{name}");
                    let own = self.allocate(1)[0];
                    self.chains.insert(path.clone(), vec![own]);
                    let (child, child_places) =
                        self.records(items, &format!("{path}/"), own_cluster, Some(own), false);
                    let child_chain = self.extend_folder(own, child.len());
                    self.write_chain(&child_chain, &child);
                    self.chains.insert(path.clone(), child_chain.clone());
                    for (child_path, offset) in child_places {
                        let cluster = child_chain[offset / self.cluster_bytes];
                        let at = self.data_at
                            + (cluster as usize - 2) * self.cluster_bytes
                            + offset % self.cluster_bytes;
                        self.records.insert(child_path, at);
                    }
                    (name, 0x10, own, 0)
                }
            };
            if deleted {
                let mut gone = short_record(b"\xE5ONE    TXT", 0x20, 0, 0);
                gone[0] = 0xE5;
                records.extend(gone);
            }
            let (short, long) = short_name(name, &mut taken);
            if long {
                for record in long_records(name, &short) {
                    records.extend(record);
                }
            }
            places.push((format!("{folder}{name}"), records.len()));
            records.extend(short_record(&short, attributes, cluster, size));
        }
        (records, places)
    }

    fn extend_folder(&mut self, first: u32, bytes: usize) -> Vec<u32> {
        let count = bytes.max(1).div_ceil(self.cluster_bytes);
        let mut chain = vec![first];
        if count > 1 {
            let more = self.allocate(count - 1);
            self.fat[first as usize] = more[0];
            chain.extend(more);
        }
        chain
    }
}

pub fn build(volume: &Volume, items: &[Item]) -> Built {
    let shape = layout(volume);
    let sector = SECTOR as u32;
    let mut bytes = vec![0u8; (volume.sectors * sector) as usize];
    let boot = &mut bytes[..SECTOR];
    boot[..3].copy_from_slice(if volume.bits == 32 {
        &[0xEB, 0x58, 0x90]
    } else {
        &[0xEB, 0x3C, 0x90]
    });
    boot[3..11].copy_from_slice(b"ZIPPERTS");
    boot[11..13].copy_from_slice(&(sector as u16).to_le_bytes());
    boot[13] = volume.cluster_sectors as u8;
    boot[14..16].copy_from_slice(&(shape.reserved as u16).to_le_bytes());
    boot[16] = 2;
    boot[17..19].copy_from_slice(&(volume.root_entries as u16).to_le_bytes());
    if volume.sectors < 0x10000 && volume.bits != 32 {
        boot[19..21].copy_from_slice(&(volume.sectors as u16).to_le_bytes());
    } else {
        boot[32..36].copy_from_slice(&volume.sectors.to_le_bytes());
    }
    boot[21] = 0xF8;
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    let extended = if volume.bits == 32 {
        boot[36..40].copy_from_slice(&shape.fat_sectors.to_le_bytes());
        boot[48..50].copy_from_slice(&1u16.to_le_bytes());
        boot[50..52].copy_from_slice(&6u16.to_le_bytes());
        64
    } else {
        boot[22..24].copy_from_slice(&(shape.fat_sectors as u16).to_le_bytes());
        36
    };
    boot[extended] = 0x80;
    boot[extended + 2] = 0x29;
    boot[extended + 3..extended + 7].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    boot[extended + 7..extended + 18]
        .copy_from_slice(volume.label.unwrap_or("NO NAME    ").as_bytes());
    let system = match volume.bits {
        12 => b"FAT12   ",
        16 => b"FAT16   ",
        _ => b"FAT32   ",
    };
    boot[extended + 18..extended + 26].copy_from_slice(system);
    boot[510] = 0x55;
    boot[511] = 0xAA;
    if volume.bits == 32 {
        let info = &mut bytes[SECTOR..2 * SECTOR];
        info[..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        info[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
        info[492..496].copy_from_slice(&u32::MAX.to_le_bytes());
        info[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
        let copy = bytes[..SECTOR].to_vec();
        bytes[6 * SECTOR..7 * SECTOR].copy_from_slice(&copy);
    }
    let fat_at = (shape.reserved * sector) as usize;
    let fat_bytes = (shape.fat_sectors * sector) as usize;
    let root_at = fat_at + 2 * fat_bytes;
    let data_at = root_at + (shape.root_sectors * sector) as usize;
    let cluster_bytes = (volume.cluster_sectors * sector) as usize;
    let mut free: Vec<u32> = (2..shape.clusters + 2).collect();
    if volume.fragment {
        let (even, odd): (Vec<u32>, Vec<u32>) = free.iter().partition(|cluster| *cluster % 2 == 0);
        free = odd.into_iter().rev().chain(even).collect();
    }
    let mut writer = Writer {
        bytes,
        bits: volume.bits,
        fat: vec![0; shape.clusters as usize + 2],
        free,
        data_at,
        cluster_bytes,
        records: BTreeMap::new(),
        chains: BTreeMap::new(),
    };
    writer.fat[0] = match volume.bits {
        12 => 0xFF8,
        16 => 0xFFF8,
        _ => 0x0FFF_FFF8,
    };
    writer.fat[1] = match volume.bits {
        12 => 0xFFF,
        16 => 0xFFFF,
        _ => 0x0FFF_FFFF,
    };
    let root_cluster = if volume.bits == 32 {
        writer.allocate(1)[0]
    } else {
        0
    };
    if volume.bits == 32 {
        writer.bytes[44..48].copy_from_slice(&root_cluster.to_le_bytes());
        writer.bytes[6 * SECTOR + 44..6 * SECTOR + 48].copy_from_slice(&root_cluster.to_le_bytes());
    }
    let mut root: Vec<u8> = Vec::new();
    if let Some(label) = volume.label {
        root.extend(short_record(
            label.as_bytes().try_into().unwrap(),
            0x08,
            0,
            0,
        ));
    }
    let (records, places) = writer.records(items, "", 0, None, volume.deleted);
    let shift = root.len();
    root.extend(records);
    if volume.bits == 32 {
        let chain = writer.extend_folder(root_cluster, root.len());
        writer.write_chain(&chain, &root);
        writer.chains.insert(String::new(), chain.clone());
        for (path, offset) in places {
            let offset = offset + shift;
            let cluster = chain[offset / cluster_bytes];
            writer.records.insert(
                path,
                data_at + (cluster as usize - 2) * cluster_bytes + offset % cluster_bytes,
            );
        }
    } else {
        assert!(
            root.len() <= (shape.root_sectors * sector) as usize,
            "the root folder is full"
        );
        writer.bytes[root_at..root_at + root.len()].copy_from_slice(&root);
        for (path, offset) in places {
            writer.records.insert(path, root_at + offset + shift);
        }
    }
    let mut fat = vec![0u8; fat_bytes];
    for (cluster, value) in writer.fat.iter().enumerate() {
        write_entry(&mut fat, volume.bits, cluster as u32, *value);
    }
    for copy in 0..2 {
        let at = fat_at + copy * fat_bytes;
        writer.bytes[at..at + fat_bytes].copy_from_slice(&fat);
    }
    Built {
        bytes: writer.bytes,
        bits: volume.bits,
        fat_at,
        fat_bytes,
        fats: 2,
        data_at,
        cluster_bytes,
        root_at,
        root_cluster,
        clusters: shape.clusters,
        records: writer.records,
        chains: writer.chains,
    }
}

fn chs_entry(status: u8, kind: u8, lba: u32, sectors: u32) -> [u8; 16] {
    let mut entry = [0u8; 16];
    entry[0] = status;
    entry[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    entry[4] = kind;
    entry[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    entry[8..12].copy_from_slice(&lba.to_le_bytes());
    entry[12..16].copy_from_slice(&sectors.to_le_bytes());
    entry
}

pub fn mbr_sector(entries: &[(u8, u32, u32)]) -> [u8; SECTOR] {
    let mut sector = [0u8; SECTOR];
    for (index, (kind, lba, sectors)) in entries.iter().enumerate() {
        let at = 0x1BE + 16 * index;
        sector[at..at + 16].copy_from_slice(&chs_entry(0, *kind, *lba, *sectors));
    }
    sector[0x1B8..0x1BC].copy_from_slice(&0xC0FF_EE00u32.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

pub fn mbr(partitions: &[(u8, u32, &[u8])], tail: u32) -> Vec<u8> {
    let entries: Vec<(u8, u32, u32)> = partitions
        .iter()
        .map(|(kind, lba, content)| (*kind, *lba, (content.len() / SECTOR) as u32))
        .collect();
    let end = entries
        .iter()
        .map(|(_, lba, sectors)| lba + sectors)
        .max()
        .unwrap_or(1);
    let mut disk = vec![0u8; (end + tail) as usize * SECTOR];
    disk[..SECTOR].copy_from_slice(&mbr_sector(&entries));
    for (_, lba, content) in partitions {
        let at = *lba as usize * SECTOR;
        disk[at..at + content.len()].copy_from_slice(content);
    }
    disk
}

pub const BASIC_DATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];
pub const LINUX_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

pub fn gpt(partitions: &[([u8; 16], &str, u64, &[u8])], tail: u64) -> Vec<u8> {
    let end = partitions
        .iter()
        .map(|(_, _, lba, content)| lba + (content.len() / SECTOR) as u64)
        .max()
        .unwrap_or(34);
    let total = end + tail + 33;
    let mut disk = vec![0u8; total as usize * SECTOR];
    disk[..SECTOR].copy_from_slice(&mbr_sector(&[(
        0xEE,
        1,
        (total - 1).min(u64::from(u32::MAX)) as u32,
    )]));
    let mut table = vec![0u8; 128 * 128];
    for (index, (kind, name, lba, content)) in partitions.iter().enumerate() {
        let entry = &mut table[index * 128..index * 128 + 128];
        entry[..16].copy_from_slice(kind);
        entry[16..32].copy_from_slice(&[index as u8 + 1; 16]);
        entry[32..40].copy_from_slice(&lba.to_le_bytes());
        entry[40..48].copy_from_slice(&(lba + (content.len() / SECTOR) as u64 - 1).to_le_bytes());
        for (unit, at) in name.encode_utf16().zip((56..128).step_by(2)) {
            entry[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        let at = *lba as usize * SECTOR;
        disk[at..at + content.len()].copy_from_slice(content);
    }
    let table_crc = crc32fast::hash(&table);
    let header = |own: u64, other: u64, table_lba: u64| {
        let mut header = [0u8; SECTOR];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&own.to_le_bytes());
        header[32..40].copy_from_slice(&other.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&(total - 34).to_le_bytes());
        header[56..72].copy_from_slice(&[0x5A; 16]);
        header[72..80].copy_from_slice(&table_lba.to_le_bytes());
        header[80..84].copy_from_slice(&128u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        header[88..92].copy_from_slice(&table_crc.to_le_bytes());
        let crc = crc32fast::hash(&header[..92]);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        header
    };
    disk[SECTOR..2 * SECTOR].copy_from_slice(&header(1, total - 1, 2));
    disk[2 * SECTOR..34 * SECTOR].copy_from_slice(&table);
    let backup_table = (total - 33) as usize * SECTOR;
    disk[backup_table..backup_table + table.len()].copy_from_slice(&table);
    let last = (total - 1) as usize * SECTOR;
    disk[last..last + SECTOR].copy_from_slice(&header(total - 1, 1, total - 33));
    disk
}

fn vhd_checksum(bytes: &[u8]) -> u32 {
    !bytes
        .iter()
        .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(*byte)))
}

pub fn vhd_footer(size: u64, kind: u32, data_offset: u64) -> [u8; SECTOR] {
    let mut footer = [0u8; SECTOR];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&data_offset.to_be_bytes());
    footer[28..32].copy_from_slice(b"zipr");
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[56..60].copy_from_slice(&0x0400_1011u32.to_be_bytes());
    footer[60..64].copy_from_slice(&kind.to_be_bytes());
    footer[68..84].copy_from_slice(&[0x11; 16]);
    let sum = vhd_checksum(&footer);
    footer[64..68].copy_from_slice(&sum.to_be_bytes());
    footer
}

pub fn vhd_fixed(disk: &[u8]) -> Vec<u8> {
    let mut image = disk.to_vec();
    image.extend(vhd_footer(disk.len() as u64, 2, u64::MAX));
    image
}

pub fn vhd_dynamic(disk: &[u8], block_log: u32) -> Vec<u8> {
    let block = 1usize << block_log;
    let blocks = disk.len().div_ceil(block);
    let footer = vhd_footer(disk.len() as u64, 3, 512);
    let mut image = footer.to_vec();
    let mut header = [0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].fill(0xFF);
    header[16..24].copy_from_slice(&1536u64.to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&(blocks as u32).to_be_bytes());
    header[32..36].copy_from_slice(&(block as u32).to_be_bytes());
    let sum = vhd_checksum(&header);
    header[36..40].copy_from_slice(&sum.to_be_bytes());
    image.extend(header);
    let table_bytes = (blocks * 4).next_multiple_of(SECTOR);
    let table_at = image.len();
    image.resize(table_at + table_bytes, 0xFF);
    let bitmap = (block / SECTOR / 8).next_multiple_of(SECTOR);
    for index in 0..blocks {
        let piece = &disk[index * block..((index + 1) * block).min(disk.len())];
        if piece.iter().all(|byte| *byte == 0) {
            continue;
        }
        let at = image.len();
        image[table_at + 4 * index..table_at + 4 * index + 4]
            .copy_from_slice(&((at / SECTOR) as u32).to_be_bytes());
        image.extend(std::iter::repeat_n(0xFF, bitmap));
        image.extend(piece);
        image.resize(at + bitmap + block, 0);
    }
    image.extend(footer);
    image
}

const fn crc32c_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0x82F6_3B78
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

pub fn crc32c(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = crc32c_table();
    !bytes.iter().fold(u32::MAX, |crc, byte| {
        TABLE[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

pub fn seal_crc32c(bytes: &mut [u8], at: usize) {
    bytes[at..at + 4].fill(0);
    let crc = crc32c(bytes);
    bytes[at..at + 4].copy_from_slice(&crc.to_le_bytes());
}

pub const VHDX_BAT: [u8; 16] = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
pub const VHDX_METADATA: [u8; 16] = [
    0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B, 0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E,
];
const FILE_PARAMETERS: [u8; 16] = [
    0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D, 0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B,
];
const VIRTUAL_DISK_SIZE: [u8; 16] = [
    0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48, 0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8,
];
const LOGICAL_SECTOR: [u8; 16] = [
    0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47, 0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F,
];
const PHYSICAL_SECTOR: [u8; 16] = [
    0xC7, 0x48, 0xA3, 0xCD, 0x5D, 0x44, 0x71, 0x44, 0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56,
];

pub struct Vhdx {
    pub bytes: Vec<u8>,
    pub bat_at: usize,
    pub metadata_at: usize,
    pub chunk_ratio: u64,
}

pub fn vhdx_sparse(size: u64, block_log: u32, blocks: &[(u64, Vec<u8>)], flags: u32) -> Vhdx {
    const MIB: usize = 1 << 20;
    let block = 1u64 << block_log;
    let chunk_ratio = (1u64 << 23) * 512 / block;
    let data_blocks = size.div_ceil(block);
    let entries = data_blocks + (data_blocks - 1) / chunk_ratio;
    let bat_bytes = ((entries * 8) as usize).next_multiple_of(MIB);
    let (metadata_at, bat_at) = (2 * MIB, 3 * MIB);
    let first_block = (bat_at + bat_bytes) as u64;
    let mut bytes = vec![0u8; first_block as usize + blocks.len() * block as usize];
    bytes[..8].copy_from_slice(b"vhdxfile");
    for (index, at) in [(0u64, 1usize << 16), (1, 2 << 16)] {
        let header = &mut bytes[at..at + 4096];
        header[..4].copy_from_slice(b"head");
        header[8..16].copy_from_slice(&(10 + index).to_le_bytes());
        header[16..32].copy_from_slice(&[0x21; 16]);
        header[32..48].copy_from_slice(&[0x22; 16]);
        header[0x42..0x44].copy_from_slice(&1u16.to_le_bytes());
        header[0x44..0x48].copy_from_slice(&(MIB as u32).to_le_bytes());
        header[0x48..0x50].copy_from_slice(&(MIB as u64).to_le_bytes());
        seal_crc32c(header, 4);
    }
    let mut region = vec![0u8; 1 << 16];
    region[..4].copy_from_slice(b"regi");
    region[8..12].copy_from_slice(&2u32.to_le_bytes());
    for (index, (guid, offset, length)) in [
        (VHDX_BAT, bat_at as u64, bat_bytes as u32),
        (VHDX_METADATA, metadata_at as u64, MIB as u32),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = &mut region[16 + 32 * index..48 + 32 * index];
        entry[..16].copy_from_slice(&guid);
        entry[16..24].copy_from_slice(&offset.to_le_bytes());
        entry[24..28].copy_from_slice(&length.to_le_bytes());
        entry[28..32].copy_from_slice(&1u32.to_le_bytes());
    }
    seal_crc32c(&mut region, 4);
    for at in [3usize << 16, 4 << 16] {
        bytes[at..at + region.len()].copy_from_slice(&region);
    }
    let metadata = &mut bytes[metadata_at..metadata_at + MIB];
    metadata[..8].copy_from_slice(b"metadata");
    let items: [([u8; 16], Vec<u8>); 4] = [
        (
            FILE_PARAMETERS,
            [(block as u32).to_le_bytes(), flags.to_le_bytes()].concat(),
        ),
        (VIRTUAL_DISK_SIZE, size.to_le_bytes().to_vec()),
        (LOGICAL_SECTOR, 512u32.to_le_bytes().to_vec()),
        (PHYSICAL_SECTOR, 4096u32.to_le_bytes().to_vec()),
    ];
    metadata[10..12].copy_from_slice(&(items.len() as u16).to_le_bytes());
    let mut place = 1usize << 16;
    for (index, (guid, value)) in items.iter().enumerate() {
        let entry = &mut metadata[32 + 32 * index..64 + 32 * index];
        entry[..16].copy_from_slice(guid);
        entry[16..20].copy_from_slice(&(place as u32).to_le_bytes());
        entry[20..24].copy_from_slice(&(value.len() as u32).to_le_bytes());
        entry[24..28].copy_from_slice(&(if index == 0 { 4u32 } else { 6 }).to_le_bytes());
        metadata[place..place + value.len()].copy_from_slice(value);
        place += value.len().next_multiple_of(8);
    }
    for (slot, (index, data)) in blocks.iter().enumerate() {
        let offset = first_block + slot as u64 * block;
        let entry = index / chunk_ratio * (chunk_ratio + 1) + index % chunk_ratio;
        let at = bat_at + entry as usize * 8;
        bytes[at..at + 8].copy_from_slice(&(offset | 6).to_le_bytes());
        let start = offset as usize;
        bytes[start..start + data.len()].copy_from_slice(data);
    }
    Vhdx {
        bytes,
        bat_at,
        metadata_at,
        chunk_ratio,
    }
}

pub fn vhdx(disk: &[u8], block_log: u32) -> Vec<u8> {
    let mut disk = disk.to_vec();
    disk.resize(disk.len().next_multiple_of(1 << 20), 0);
    let block = 1usize << block_log;
    let blocks: Vec<(u64, Vec<u8>)> = disk
        .chunks(block)
        .enumerate()
        .filter(|(_, piece)| piece.iter().any(|byte| *byte != 0))
        .map(|(index, piece)| (index as u64, piece.to_vec()))
        .collect();
    vhdx_sparse(disk.len() as u64, block_log, &blocks, 0).bytes
}

fn deflate_raw(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn deflate_zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn nonzero_pieces(disk: &[u8], piece: usize) -> Vec<(u64, Vec<u8>)> {
    disk.chunks(piece)
        .enumerate()
        .filter(|(_, bytes)| bytes.iter().any(|byte| *byte != 0))
        .map(|(index, bytes)| {
            let mut bytes = bytes.to_vec();
            bytes.resize(piece, 0);
            (index as u64, bytes)
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct Qcow {
    pub version: u32,
    pub cluster_bits: u32,
    pub compressed: bool,
    pub zero_flags: bool,
}

pub struct QcowImage {
    pub bytes: Vec<u8>,
    pub l1_at: usize,
    pub l2_at: Vec<usize>,
    pub data_at: Vec<usize>,
}

pub fn qcow_sparse(options: Qcow, size: u64, clusters: &[(u64, Vec<u8>)]) -> QcowImage {
    let cluster = 1usize << options.cluster_bits;
    let l2_entries = cluster / 8;
    let l1_entries = size.div_ceil((cluster * l2_entries) as u64) as usize;
    let l1_clusters = (l1_entries * 8).div_ceil(cluster).max(1);
    let v1 = options.version == 1;
    let l1_at = if v1 { cluster } else { 3 * cluster };
    let mut bytes = vec![0u8; l1_at + l1_clusters * cluster];
    let mut l2_of = BTreeMap::new();
    let mut l2_at = Vec::new();
    let mut data_at = Vec::new();
    let mut packed_at = 0usize;
    let mut packed_end = 0usize;
    let present: BTreeMap<u64, &Vec<u8>> = clusters
        .iter()
        .map(|(index, data)| (*index, data))
        .collect();
    for (&index, data) in &present {
        let zero = data.iter().all(|byte| *byte == 0);
        if zero && !options.zero_flags {
            continue;
        }
        let table = index as usize / l2_entries;
        let table_at = *l2_of.entry(table).or_insert_with(|| {
            let at = bytes.len();
            bytes.resize(at + cluster, 0);
            let entry = if v1 { at as u64 } else { at as u64 | 1 << 63 };
            bytes[l1_at + 8 * table..l1_at + 8 * table + 8].copy_from_slice(&entry.to_be_bytes());
            l2_at.push(at);
            at
        });
        let slot = table_at + 8 * (index as usize % l2_entries);
        if zero {
            bytes[slot..slot + 8].copy_from_slice(&1u64.to_be_bytes());
            continue;
        }
        let entry = if options.compressed {
            let packed = deflate_raw(data);
            if packed_at + packed.len() > packed_end {
                packed_at = bytes.len();
                packed_end = packed_at;
            }
            while packed_at + packed.len() > packed_end {
                bytes.resize(packed_end + cluster, 0);
                packed_end += cluster;
            }
            bytes[packed_at..packed_at + packed.len()].copy_from_slice(&packed);
            data_at.push(packed_at);
            let entry = if v1 {
                1 << 63 | (packed.len() as u64) << (63 - options.cluster_bits) | packed_at as u64
            } else {
                let shift = 62 - (options.cluster_bits - 8);
                let extra = ((packed_at + packed.len() - 1) >> 9) - (packed_at >> 9);
                1 << 62 | (extra as u64) << shift | packed_at as u64
            };
            packed_at += packed.len();
            entry
        } else {
            let at = bytes.len();
            bytes.extend_from_slice(data);
            bytes.resize(at + cluster, 0);
            data_at.push(at);
            if v1 { at as u64 } else { at as u64 | 1 << 63 }
        };
        bytes[slot..slot + 8].copy_from_slice(&entry.to_be_bytes());
    }
    bytes.resize(bytes.len().next_multiple_of(cluster), 0);
    let total = bytes.len() / cluster;
    let header = &mut bytes[..cluster];
    header[..4].copy_from_slice(b"QFI\xfb");
    header[4..8].copy_from_slice(&options.version.to_be_bytes());
    if v1 {
        header[24..32].copy_from_slice(&size.to_be_bytes());
        header[32] = options.cluster_bits as u8;
        header[33] = (options.cluster_bits - 3) as u8;
        header[40..48].copy_from_slice(&(l1_at as u64).to_be_bytes());
    } else {
        header[20..24].copy_from_slice(&options.cluster_bits.to_be_bytes());
        header[24..32].copy_from_slice(&size.to_be_bytes());
        header[36..40].copy_from_slice(&(l1_entries as u32).to_be_bytes());
        header[40..48].copy_from_slice(&(l1_at as u64).to_be_bytes());
        header[48..56].copy_from_slice(&(cluster as u64).to_be_bytes());
        header[56..60].copy_from_slice(&1u32.to_be_bytes());
        if options.version == 3 {
            header[96..100].copy_from_slice(&4u32.to_be_bytes());
            header[100..104].copy_from_slice(&112u32.to_be_bytes());
        }
        bytes[cluster..cluster + 8].copy_from_slice(&(2 * cluster as u64).to_be_bytes());
        for used in 0..total.min(cluster / 2) {
            let at = 2 * cluster + 2 * used;
            bytes[at..at + 2].copy_from_slice(&1u16.to_be_bytes());
        }
    }
    QcowImage {
        bytes,
        l1_at,
        l2_at,
        data_at,
    }
}

pub fn qcow(disk: &[u8], options: Qcow) -> Vec<u8> {
    let clusters = nonzero_pieces(disk, 1 << options.cluster_bits);
    qcow_sparse(options, disk.len() as u64, &clusters).bytes
}

#[derive(Clone, Copy, Debug)]
pub struct Vdi {
    pub block_log: u32,
    pub fixed: bool,
    pub zero_entries: bool,
}

pub struct VdiImage {
    pub bytes: Vec<u8>,
    pub map_at: usize,
    pub data_at: usize,
}

pub fn vdi_sparse(options: Vdi, size: u64, blocks: &[(u64, Vec<u8>)]) -> VdiImage {
    let block = 1usize << options.block_log;
    let count = size.div_ceil(block as u64) as usize;
    let map_at = 0x200;
    let data_at = (map_at + 4 * count).next_multiple_of(SECTOR);
    let mut bytes = vec![0u8; data_at];
    bytes[..0x40].copy_from_slice(format!("{:<63}\n", "<<< Zipper test VDI image >>>").as_bytes());
    let present: BTreeMap<u64, &Vec<u8>> =
        blocks.iter().map(|(index, data)| (*index, data)).collect();
    let mut allocated = 0u32;
    for index in 0..count {
        let data = present.get(&(index as u64));
        let keep = options.fixed || data.is_some_and(|data| data.iter().any(|byte| *byte != 0));
        let entry = if keep {
            if let Some(data) = data {
                bytes.extend_from_slice(data);
            }
            bytes.resize(data_at + (allocated as usize + 1) * block, 0);
            allocated += 1;
            allocated - 1
        } else if data.is_some() && options.zero_entries {
            0xFFFF_FFFE
        } else {
            u32::MAX
        };
        bytes[map_at + 4 * index..map_at + 4 * index + 4].copy_from_slice(&entry.to_le_bytes());
    }
    let kind: u32 = if options.fixed { 2 } else { 1 };
    let fields: [(usize, &[u8]); 12] = [
        (0x40, &0xBEDA_107Fu32.to_le_bytes()),
        (0x44, &0x0001_0001u32.to_le_bytes()),
        (0x48, &0x180u32.to_le_bytes()),
        (0x4C, &kind.to_le_bytes()),
        (0x154, &(map_at as u32).to_le_bytes()),
        (0x158, &(data_at as u32).to_le_bytes()),
        (0x168, &512u32.to_le_bytes()),
        (0x170, &size.to_le_bytes()),
        (0x178, &(block as u32).to_le_bytes()),
        (0x180, &(count as u32).to_le_bytes()),
        (0x184, &allocated.to_le_bytes()),
        (0x188, &[0x5A; 16]),
    ];
    for (at, value) in fields {
        bytes[at..at + value.len()].copy_from_slice(value);
    }
    VdiImage {
        bytes,
        map_at,
        data_at,
    }
}

pub fn vdi(disk: &[u8], options: Vdi) -> Vec<u8> {
    let blocks = nonzero_pieces(disk, 1 << options.block_log);
    vdi_sparse(options, disk.len() as u64, &blocks).bytes
}

#[derive(Clone, Copy, Debug)]
pub struct Vmdk {
    pub grain_sectors: u64,
    pub table_entries: u32,
    pub stream: bool,
}

pub struct VmdkImage {
    pub bytes: Vec<u8>,
    pub gd_at: usize,
    pub gt_at: Vec<usize>,
    pub grain_at: Vec<usize>,
}

pub fn vmdk_descriptor(kind: &str, extents: &[String], parent: Option<u32>) -> String {
    let mut text = format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID={:08x}\ncreateType=\"{kind}\"\n\n# Extent description\n",
        parent.unwrap_or(u32::MAX)
    );
    for extent in extents {
        text.push_str(extent);
        text.push('\n');
    }
    text.push_str("\n# The Disk Data Base\n#DDB\n\nddb.virtualHWVersion = \"4\"\n");
    text
}

fn vmdk_header(
    options: Vmdk,
    capacity: u64,
    gd: u64,
    overhead: u64,
    descriptor: u64,
) -> [u8; SECTOR] {
    let mut header = [0u8; SECTOR];
    header[..4].copy_from_slice(b"KDMV");
    let (version, flags): (u32, u32) = if options.stream {
        (3, 0x3_0003)
    } else {
        (1, 3)
    };
    header[4..8].copy_from_slice(&version.to_le_bytes());
    header[8..12].copy_from_slice(&flags.to_le_bytes());
    header[12..20].copy_from_slice(&capacity.to_le_bytes());
    header[20..28].copy_from_slice(&options.grain_sectors.to_le_bytes());
    header[28..36].copy_from_slice(&1u64.to_le_bytes());
    header[36..44].copy_from_slice(&descriptor.to_le_bytes());
    header[44..48].copy_from_slice(&options.table_entries.to_le_bytes());
    header[56..64].copy_from_slice(&gd.to_le_bytes());
    header[64..72].copy_from_slice(&overhead.to_le_bytes());
    header[0x49..0x4D].copy_from_slice(b"\n \r\n");
    if options.stream {
        header[0x4D..0x4F].copy_from_slice(&1u16.to_le_bytes());
    }
    header
}

fn marker(sectors: u64, kind: u32) -> [u8; SECTOR] {
    let mut marker = [0u8; SECTOR];
    marker[..8].copy_from_slice(&sectors.to_le_bytes());
    marker[12..16].copy_from_slice(&kind.to_le_bytes());
    marker
}

pub fn vmdk_sparse(options: Vmdk, size: u64, grains: &[(u64, Vec<u8>)], name: &str) -> VmdkImage {
    let grain = options.grain_sectors as usize * SECTOR;
    let capacity = size.div_ceil(SECTOR as u64);
    let grain_count = capacity.div_ceil(options.grain_sectors) as usize;
    let per_table = options.table_entries as usize;
    let tables = grain_count.div_ceil(per_table);
    let kind = if options.stream {
        "streamOptimized"
    } else {
        "monolithicSparse"
    };
    let descriptor = vmdk_descriptor(kind, &[format!("RW {capacity} SPARSE \"{name}\"")], None);
    let descriptor_sectors = descriptor.len().div_ceil(SECTOR) as u64;
    let table_sectors = (per_table * 4).div_ceil(SECTOR);
    let gd_sectors = (tables * 4).div_ceil(SECTOR);
    let mut table = vec![0u32; tables * per_table];
    let mut bytes;
    let mut grain_at = Vec::new();
    let mut gt_at = Vec::new();
    let gd_at;
    if options.stream {
        let overhead = (1 + descriptor_sectors).next_multiple_of(options.grain_sectors);
        bytes = vec![0u8; overhead as usize * SECTOR];
        bytes[..SECTOR].copy_from_slice(&vmdk_header(
            options,
            capacity,
            u64::MAX,
            overhead,
            descriptor_sectors,
        ));
        bytes[SECTOR..SECTOR + descriptor.len()].copy_from_slice(descriptor.as_bytes());
        for (index, data) in grains {
            if data.iter().all(|byte| *byte == 0) {
                continue;
            }
            let packed = deflate_zlib(data);
            let at = bytes.len();
            grain_at.push(at);
            table[*index as usize] = (at / SECTOR) as u32;
            bytes.extend_from_slice(&(index * options.grain_sectors).to_le_bytes());
            bytes.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&packed);
            bytes.resize(bytes.len().next_multiple_of(SECTOR), 0);
        }
        let mut directory = Vec::new();
        for index in 0..tables {
            bytes.extend_from_slice(&marker(table_sectors as u64, 1));
            gt_at.push(bytes.len());
            directory.push((bytes.len() / SECTOR) as u32);
            for entry in &table[index * per_table..(index + 1) * per_table] {
                bytes.extend_from_slice(&entry.to_le_bytes());
            }
            bytes.resize(bytes.len().next_multiple_of(SECTOR), 0);
        }
        bytes.extend_from_slice(&marker(gd_sectors as u64, 2));
        gd_at = bytes.len();
        for entry in directory {
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        bytes.resize(bytes.len().next_multiple_of(SECTOR), 0);
        bytes.extend_from_slice(&marker(1, 3));
        bytes.extend_from_slice(&vmdk_header(
            options,
            capacity,
            (gd_at / SECTOR) as u64,
            overhead,
            descriptor_sectors,
        ));
        bytes.extend_from_slice(&marker(0, 0));
    } else {
        let gd_sector = 1 + descriptor_sectors;
        let first_table = gd_sector + gd_sectors as u64;
        let overhead =
            (first_table + (tables * table_sectors) as u64).next_multiple_of(options.grain_sectors);
        bytes = vec![0u8; overhead as usize * SECTOR];
        bytes[..SECTOR].copy_from_slice(&vmdk_header(
            options,
            capacity,
            gd_sector,
            overhead,
            descriptor_sectors,
        ));
        bytes[SECTOR..SECTOR + descriptor.len()].copy_from_slice(descriptor.as_bytes());
        gd_at = gd_sector as usize * SECTOR;
        for index in 0..tables {
            let at = (first_table as usize + index * table_sectors) * SECTOR;
            gt_at.push(at);
            bytes[gd_at + 4 * index..gd_at + 4 * index + 4]
                .copy_from_slice(&((at / SECTOR) as u32).to_le_bytes());
        }
        for (index, data) in grains {
            if data.iter().all(|byte| *byte == 0) {
                continue;
            }
            let at = bytes.len();
            grain_at.push(at);
            table[*index as usize] = (at / SECTOR) as u32;
            bytes.extend_from_slice(data);
            bytes.resize(at + grain, 0);
        }
        for (index, entry) in table.iter().enumerate() {
            let at = gt_at[index / per_table] + 4 * (index % per_table);
            bytes[at..at + 4].copy_from_slice(&entry.to_le_bytes());
        }
    }
    VmdkImage {
        bytes,
        gd_at,
        gt_at,
        grain_at,
    }
}

pub fn vmdk(disk: &[u8], options: Vmdk, name: &str) -> Vec<u8> {
    let grains = nonzero_pieces(disk, options.grain_sectors as usize * SECTOR);
    vmdk_sparse(options, disk.len() as u64, &grains, name).bytes
}

pub fn vmdk_flat(disk: &[u8], name: &str) -> (Vec<u8>, Vec<u8>) {
    let mut flat = disk.to_vec();
    flat.resize(disk.len().next_multiple_of(SECTOR), 0);
    let extent = format!("RW {} FLAT \"{name}\" 0", flat.len() / SECTOR);
    (
        vmdk_descriptor("monolithicFlat", &[extent], None).into_bytes(),
        flat,
    )
}

pub const SPARSE_RAW: u16 = 0xCAC1;
pub const SPARSE_FILL: u16 = 0xCAC2;
pub const SPARSE_SKIP: u16 = 0xCAC3;
pub const SPARSE_CRC: u16 = 0xCAC4;

pub struct SparseImage {
    pub bytes: Vec<u8>,
    pub chunk_at: Vec<usize>,
}

pub fn android_sparse_chunks(
    block: u32,
    blocks: u32,
    chunks: &[(u16, u32, Vec<u8>)],
) -> SparseImage {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xED26_FF3Au32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&28u16.to_le_bytes());
    bytes.extend_from_slice(&12u16.to_le_bytes());
    bytes.extend_from_slice(&block.to_le_bytes());
    bytes.extend_from_slice(&blocks.to_le_bytes());
    bytes.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let mut chunk_at = Vec::new();
    for (kind, count, data) in chunks {
        chunk_at.push(bytes.len());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    SparseImage { bytes, chunk_at }
}

pub fn android_sparse(disk: &[u8], block: u32) -> Vec<u8> {
    let block_bytes = block as usize;
    let mut disk = disk.to_vec();
    disk.resize(disk.len().next_multiple_of(block_bytes), 0);
    let mut chunks: Vec<(u16, u32, Vec<u8>)> = Vec::new();
    for piece in disk.chunks(block_bytes) {
        let word = &piece[..4];
        let filled = piece.chunks(4).all(|part| part == word);
        let kind = match (filled, word == [0; 4]) {
            (true, true) => SPARSE_SKIP,
            (true, false) => SPARSE_FILL,
            _ => SPARSE_RAW,
        };
        match chunks.last_mut() {
            Some((last, count, data)) if *last == kind && (kind != SPARSE_FILL || data == word) => {
                *count += 1;
                if kind == SPARSE_RAW {
                    data.extend_from_slice(piece);
                }
            }
            _ => chunks.push((
                kind,
                1,
                match kind {
                    SPARSE_RAW => piece.to_vec(),
                    SPARSE_FILL => word.to_vec(),
                    _ => Vec::new(),
                },
            )),
        }
    }
    chunks.push((SPARSE_CRC, 0, crc32fast::hash(&disk).to_le_bytes().to_vec()));
    android_sparse_chunks(block, (disk.len() / block_bytes) as u32, &chunks).bytes
}
