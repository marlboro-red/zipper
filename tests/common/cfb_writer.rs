#![allow(dead_code)]

pub const FREE: u32 = 0xFFFF_FFFF;
pub const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
pub const FAT_SECTOR: u32 = 0xFFFF_FFFD;
pub const DIFAT_SECTOR: u32 = 0xFFFF_FFFC;
pub const NO_ENTRY: u32 = 0xFFFF_FFFF;
pub const MINI_STREAM_CUTOFF: usize = 4096;
pub const ENTRY: usize = 128;
const MINI_SECTOR: usize = 64;
const MSI_CHARACTERS: &[u8; 64] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._";

#[derive(Clone, Debug)]
pub enum Node {
    Stream {
        name: Vec<u16>,
        data: Vec<u8>,
    },
    Storage {
        name: Vec<u16>,
        children: Vec<Node>,
        modified: u64,
    },
}

pub fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

pub fn stream(name: &str, data: &[u8]) -> Node {
    Node::Stream {
        name: units(name),
        data: data.to_vec(),
    }
}

pub fn raw_stream(name: Vec<u16>, data: &[u8]) -> Node {
    Node::Stream {
        name,
        data: data.to_vec(),
    }
}

pub fn storage(name: &str, children: Vec<Node>) -> Node {
    Node::Storage {
        name: units(name),
        children,
        modified: 0,
    }
}

pub fn msi_name(name: &str, table: bool) -> Vec<u16> {
    let index = |byte: u8| MSI_CHARACTERS.iter().position(|&known| known == byte);
    let mut encoded = Vec::new();
    if table {
        encoded.push(0x4840);
    }
    let bytes = name.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match (
            index(bytes[at]),
            bytes.get(at + 1).and_then(|&next| index(next)),
        ) {
            (Some(first), Some(second)) => {
                encoded.push(0x3800 + first as u16 + ((second as u16) << 6));
                at += 2;
            }
            (Some(first), None) => {
                encoded.push(0x4800 + first as u16);
                at += 1;
            }
            (None, _) => {
                encoded.push(u16::from(bytes[at]));
                at += 1;
            }
        }
    }
    encoded
}

#[derive(Clone, Debug)]
pub struct Compound {
    pub version: u16,
    pub children: Vec<Node>,
    pub root_modified: u64,
    pub scatter: Option<u64>,
}

impl Compound {
    pub fn new(version: u16, children: Vec<Node>) -> Self {
        Compound {
            version,
            children,
            root_modified: 0,
            scatter: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Flat {
    pub name: Vec<u16>,
    pub kind: u8,
    pub left: u32,
    pub right: u32,
    pub child: u32,
    pub modified: u64,
    pub start: u32,
    pub size: u64,
    data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Built {
    pub bytes: Vec<u8>,
    pub sector_size: usize,
    pub fat_sectors: Vec<u32>,
    pub difat_sectors: Vec<u32>,
    pub directory_sectors: Vec<u32>,
    pub mini_fat_sectors: Vec<u32>,
    pub mini_stream_sectors: Vec<u32>,
    pub entries: Vec<Flat>,
}

impl Built {
    pub fn sector_offset(&self, sector: u32) -> usize {
        (sector as usize + 1) * self.sector_size
    }

    pub fn fat_offset(&self, sector: u32) -> usize {
        let per = self.sector_size / 4;
        let holder = self.fat_sectors[sector as usize / per];
        self.sector_offset(holder) + 4 * (sector as usize % per)
    }

    pub fn entry_offset(&self, entry: u32) -> usize {
        let per = self.sector_size / ENTRY;
        let holder = self.directory_sectors[entry as usize / per];
        self.sector_offset(holder) + ENTRY * (entry as usize % per)
    }

    pub fn mini_fat_offset(&self, mini: u32) -> usize {
        let per = self.sector_size / 4;
        let holder = self.mini_fat_sectors[mini as usize / per];
        self.sector_offset(holder) + 4 * (mini as usize % per)
    }

    pub fn get_u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap())
    }

    pub fn set_u32(&mut self, at: usize, value: u32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn set_u16(&mut self, at: usize, value: u16) {
        self.bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn fat(&self, sector: u32) -> u32 {
        self.get_u32(self.fat_offset(sector))
    }

    pub fn set_fat(&mut self, sector: u32, value: u32) {
        let at = self.fat_offset(sector);
        self.set_u32(at, value);
    }

    pub fn entry_field(&self, entry: u32, field: usize) -> usize {
        self.entry_offset(entry) + field
    }

    pub fn index_of(&self, name: &str) -> u32 {
        let wanted = units(name);
        self.entries
            .iter()
            .position(|entry| entry.name == wanted)
            .unwrap_or_else(|| panic!("no entry {name:?}")) as u32
    }
}

fn compare(a: &[u16], b: &[u16]) -> std::cmp::Ordering {
    let upper = |name: &[u16]| -> Vec<u16> {
        name.iter()
            .map(|&unit| {
                if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) {
                    unit - 32
                } else {
                    unit
                }
            })
            .collect()
    };
    a.len().cmp(&b.len()).then_with(|| upper(a).cmp(&upper(b)))
}

fn balanced(entries: &mut [Flat], sorted: &[u32]) -> u32 {
    if sorted.is_empty() {
        return NO_ENTRY;
    }
    let middle = sorted.len() / 2;
    let root = sorted[middle];
    let left = balanced(entries, &sorted[..middle]);
    let right = balanced(entries, &sorted[middle + 1..]);
    entries[root as usize].left = left;
    entries[root as usize].right = right;
    root
}

fn flatten(entries: &mut Vec<Flat>, parent: usize, children: &[Node]) {
    let mut ids = Vec::new();
    for node in children {
        let id = entries.len() as u32;
        ids.push(id);
        let (name, kind, modified, data) = match node {
            Node::Stream { name, data } => (name.clone(), 2, 0, data.clone()),
            Node::Storage { name, modified, .. } => (name.clone(), 1, *modified, Vec::new()),
        };
        entries.push(Flat {
            name,
            kind,
            left: NO_ENTRY,
            right: NO_ENTRY,
            child: NO_ENTRY,
            modified,
            start: if kind == 2 { END_OF_CHAIN } else { 0 },
            size: data.len() as u64,
            data,
        });
        if let Node::Storage { children, .. } = node {
            flatten(entries, id as usize, children);
        }
    }
    ids.sort_by(|&a, &b| compare(&entries[a as usize].name, &entries[b as usize].name));
    entries[parent].child = balanced(entries, &ids);
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

impl Compound {
    pub fn build(&self) -> Built {
        let sector_size = if self.version == 4 { 4096 } else { 512 };
        let per = sector_size / 4;
        let mut entries = vec![Flat {
            name: units("Root Entry"),
            kind: 5,
            left: NO_ENTRY,
            right: NO_ENTRY,
            child: NO_ENTRY,
            modified: self.root_modified,
            start: END_OF_CHAIN,
            size: 0,
            data: Vec::new(),
        }];
        flatten(&mut entries, 0, &self.children);

        let mut mini_stream = Vec::new();
        let mut mini_fat: Vec<u32> = Vec::new();
        for entry in entries.iter_mut().filter(|entry| entry.kind == 2) {
            if entry.data.is_empty() || entry.data.len() >= MINI_STREAM_CUTOFF {
                continue;
            }
            let first = mini_fat.len() as u32;
            let count = entry.data.len().div_ceil(MINI_SECTOR);
            for index in 0..count {
                mini_fat.push(if index + 1 == count {
                    END_OF_CHAIN
                } else {
                    first + index as u32 + 1
                });
            }
            entry.start = first;
            mini_stream.extend_from_slice(&entry.data);
            mini_stream.resize(mini_fat.len() * MINI_SECTOR, 0);
        }

        let directory_count = (entries.len() * ENTRY).div_ceil(sector_size);
        let mini_fat_count = (mini_fat.len() * 4).div_ceil(sector_size);
        let mini_stream_count = mini_stream.len().div_ceil(sector_size);
        let large: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind == 2 && entry.data.len() >= MINI_STREAM_CUTOFF)
            .map(|(index, _)| index)
            .collect();
        let large_counts: usize = large
            .iter()
            .map(|&index| entries[index].data.len().div_ceil(sector_size))
            .sum();
        let data_count = directory_count + mini_fat_count + mini_stream_count + large_counts;
        let mut fat_count = 1usize;
        let difat_count = loop {
            let difat = if fat_count > 109 {
                (fat_count - 109).div_ceil(per - 1)
            } else {
                0
            };
            let need = (data_count + fat_count + difat).div_ceil(per);
            if need <= fat_count {
                break difat;
            }
            fat_count = need;
        };
        let fat_sectors: Vec<u32> = (0..fat_count as u32).collect();
        let difat_sectors: Vec<u32> =
            (fat_count as u32..(fat_count + difat_count) as u32).collect();
        let mut slots: Vec<u32> = ((fat_count + difat_count) as u32
            ..(fat_count + difat_count + data_count) as u32)
            .collect();
        if let Some(seed) = self.scatter {
            let mut random = Random(seed | 1);
            for index in (1..slots.len()).rev() {
                let other = (random.next() % (index as u64 + 1)) as usize;
                slots.swap(index, other);
            }
        }
        let mut slots = slots.into_iter();
        let mut take = |count: usize| -> Vec<u32> { slots.by_ref().take(count).collect() };
        let directory_sectors = take(directory_count);
        let mini_fat_sectors = take(mini_fat_count);
        let mini_stream_sectors = take(mini_stream_count);
        let large_sectors: Vec<Vec<u32>> = large
            .iter()
            .map(|&index| take(entries[index].data.len().div_ceil(sector_size)))
            .collect();

        let total_sectors = fat_count + difat_count + data_count;
        let mut fat = vec![FREE; fat_count * per];
        for &sector in &fat_sectors {
            fat[sector as usize] = FAT_SECTOR;
        }
        for &sector in &difat_sectors {
            fat[sector as usize] = DIFAT_SECTOR;
        }
        let mut chain = |sectors: &[u32]| {
            for pair in sectors.windows(2) {
                fat[pair[0] as usize] = pair[1];
            }
            if let Some(&last) = sectors.last() {
                fat[last as usize] = END_OF_CHAIN;
            }
        };
        chain(&directory_sectors);
        chain(&mini_fat_sectors);
        chain(&mini_stream_sectors);
        for sectors in &large_sectors {
            chain(sectors);
        }
        let first = |sectors: &[u32]| sectors.first().copied().unwrap_or(END_OF_CHAIN);
        entries[0].start = first(&mini_stream_sectors);
        entries[0].size = mini_stream.len() as u64;
        for (&index, sectors) in large.iter().zip(&large_sectors) {
            entries[index].start = first(sectors);
        }

        let mut bytes = vec![0u8; (total_sectors + 1) * sector_size];
        let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
        put(0, &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
        put(0x18, &0x3Eu16.to_le_bytes());
        put(0x1A, &self.version.to_le_bytes());
        put(0x1C, &0xFFFEu16.to_le_bytes());
        put(
            0x1E,
            &(if self.version == 4 { 12u16 } else { 9 }).to_le_bytes(),
        );
        put(0x20, &6u16.to_le_bytes());
        let directory_field = if self.version == 4 {
            directory_count as u32
        } else {
            0
        };
        put(0x28, &directory_field.to_le_bytes());
        put(0x2C, &(fat_count as u32).to_le_bytes());
        put(0x30, &first(&directory_sectors).to_le_bytes());
        put(0x38, &(MINI_STREAM_CUTOFF as u32).to_le_bytes());
        put(0x3C, &first(&mini_fat_sectors).to_le_bytes());
        put(0x40, &(mini_fat_count as u32).to_le_bytes());
        put(0x44, &first(&difat_sectors).to_le_bytes());
        put(0x48, &(difat_count as u32).to_le_bytes());
        for index in 0..109 {
            let value = fat_sectors.get(index).copied().unwrap_or(FREE);
            put(0x4C + 4 * index, &value.to_le_bytes());
        }
        let offset = |sector: u32| (sector as usize + 1) * sector_size;
        let mut listed = fat_sectors.iter().skip(109);
        for (position, &sector) in difat_sectors.iter().enumerate() {
            for slot in 0..per - 1 {
                let value = listed.next().copied().unwrap_or(FREE);
                put(offset(sector) + 4 * slot, &value.to_le_bytes());
            }
            let next = difat_sectors
                .get(position + 1)
                .copied()
                .unwrap_or(END_OF_CHAIN);
            put(offset(sector) + 4 * (per - 1), &next.to_le_bytes());
        }
        for (index, value) in fat.iter().enumerate() {
            let holder = fat_sectors[index / per];
            put(offset(holder) + 4 * (index % per), &value.to_le_bytes());
        }
        for (index, value) in mini_fat.iter().enumerate() {
            let holder = mini_fat_sectors[index / per];
            put(offset(holder) + 4 * (index % per), &value.to_le_bytes());
        }
        for (index, chunk) in mini_stream.chunks(sector_size).enumerate() {
            put(offset(mini_stream_sectors[index]), chunk);
        }
        for (&index, sectors) in large.iter().zip(&large_sectors) {
            for (chunk, &sector) in entries[index].data.chunks(sector_size).zip(sectors) {
                put(offset(sector), chunk);
            }
        }
        let per_directory = sector_size / ENTRY;
        for slot in 0..directory_count * per_directory {
            let at =
                offset(directory_sectors[slot / per_directory]) + ENTRY * (slot % per_directory);
            let Some(entry) = entries.get(slot) else {
                put(at + 68, &NO_ENTRY.to_le_bytes());
                put(at + 72, &NO_ENTRY.to_le_bytes());
                put(at + 76, &NO_ENTRY.to_le_bytes());
                continue;
            };
            for (position, unit) in entry.name.iter().enumerate() {
                put(at + 2 * position, &unit.to_le_bytes());
            }
            put(at + 64, &((entry.name.len() as u16 + 1) * 2).to_le_bytes());
            put(at + 66, &[entry.kind, 1]);
            put(at + 68, &entry.left.to_le_bytes());
            put(at + 72, &entry.right.to_le_bytes());
            put(at + 76, &entry.child.to_le_bytes());
            put(at + 108, &entry.modified.to_le_bytes());
            put(at + 116, &entry.start.to_le_bytes());
            put(at + 120, &(entry.size as u32).to_le_bytes());
            if self.version == 4 {
                put(at + 124, &((entry.size >> 32) as u32).to_le_bytes());
            }
        }
        Built {
            bytes,
            sector_size,
            fat_sectors,
            difat_sectors,
            directory_sectors,
            mini_fat_sectors,
            mini_stream_sectors,
            entries,
        }
    }
}
