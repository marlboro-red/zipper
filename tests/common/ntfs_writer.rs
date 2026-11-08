#![allow(dead_code)]

pub const SECTOR: usize = 512;
pub const ROOT: u64 = 5;
pub const STANDARD_INFORMATION: u32 = 0x10;
pub const ATTRIBUTE_LIST: u32 = 0x20;
pub const FILE_NAME: u32 = 0x30;
pub const DATA: u32 = 0x80;
pub const INDEX_ROOT: u32 = 0x90;
pub const REPARSE_POINT: u32 = 0xC0;
pub const IN_USE: u16 = 1;
pub const DIRECTORY: u16 = 2;
pub const POSIX_NAME: u8 = 0;
pub const WIN32_NAME: u8 = 1;
pub const DOS_NAME: u8 = 2;
pub const WIN32_AND_DOS_NAME: u8 = 3;
pub const COMPRESSED: u16 = 0x0001;
pub const SPARSE: u16 = 0x8000;
pub const UNIT_CLUSTERS: usize = 16;
pub const LZNT1_CHUNK: usize = 4096;
pub const SYMLINK_TAG: u32 = 0xA000_000C;
pub const JUNCTION_TAG: u32 = 0xA000_0003;

pub fn reference(index: u64, sequence: u16) -> u64 {
    index | u64::from(sequence) << 48
}

pub fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn padded(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.resize(bytes.len().div_ceil(8) * 8, 0);
    bytes
}

pub fn resident(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let name = utf16(name);
    let value_offset = (0x18 + name.len()).div_ceil(8) * 8;
    let mut bytes = vec![0u8; value_offset];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    bytes[9] = (name.len() / 2) as u8;
    bytes[0x0A..0x0C].copy_from_slice(&0x18u16.to_le_bytes());
    bytes[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&(value_offset as u16).to_le_bytes());
    bytes[0x18..0x18 + name.len()].copy_from_slice(&name);
    bytes.extend_from_slice(value);
    let mut bytes = padded(bytes);
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Sizes {
    pub low_vcn: u64,
    pub high_vcn: u64,
    pub allocated: u64,
    pub size: u64,
    pub initialized: u64,
    pub unit: u8,
    pub flags: u16,
    pub packed: Option<u64>,
}

pub fn non_resident(kind: u32, name: &str, sizes: Sizes, runs: &[u8]) -> Vec<u8> {
    let name = utf16(name);
    let head = if sizes.packed.is_some() { 0x48 } else { 0x40 };
    let runs_offset = (head + name.len()).div_ceil(8) * 8;
    let mut bytes = vec![0u8; runs_offset];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    bytes[8] = 1;
    bytes[9] = (name.len() / 2) as u8;
    bytes[0x0A..0x0C].copy_from_slice(&(head as u16).to_le_bytes());
    bytes[0x0C..0x0E].copy_from_slice(&sizes.flags.to_le_bytes());
    bytes[0x10..0x18].copy_from_slice(&sizes.low_vcn.to_le_bytes());
    bytes[0x18..0x20].copy_from_slice(&sizes.high_vcn.to_le_bytes());
    bytes[0x20..0x22].copy_from_slice(&(runs_offset as u16).to_le_bytes());
    bytes[0x22] = sizes.unit;
    bytes[0x28..0x30].copy_from_slice(&sizes.allocated.to_le_bytes());
    bytes[0x30..0x38].copy_from_slice(&sizes.size.to_le_bytes());
    bytes[0x38..0x40].copy_from_slice(&sizes.initialized.to_le_bytes());
    if let Some(packed) = sizes.packed {
        bytes[0x40..0x48].copy_from_slice(&packed.to_le_bytes());
    }
    bytes[head..head + name.len()].copy_from_slice(&name);
    bytes.extend_from_slice(runs);
    bytes.push(0);
    let mut bytes = padded(bytes);
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes
}

pub fn standard_information_all_times(ticks: u64, attributes: u32) -> Vec<u8> {
    let mut value = vec![0u8; 0x48];
    for at in [0, 8, 16, 24] {
        value[at..at + 8].copy_from_slice(&ticks.to_le_bytes());
    }
    value[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
    resident(STANDARD_INFORMATION, "", &value)
}

pub fn file_name_value(parent: u64, kind: u8, name: &str) -> Vec<u8> {
    let units = utf16(name);
    let mut value = vec![0u8; 0x42];
    value[0..8].copy_from_slice(&parent.to_le_bytes());
    value[0x40] = (units.len() / 2) as u8;
    value[0x41] = kind;
    value.extend_from_slice(&units);
    value
}

pub fn file_name(parent: u64, kind: u8, name: &str) -> Vec<u8> {
    resident(FILE_NAME, "", &file_name_value(parent, kind, name))
}

pub fn runlist(runs: &[(u64, Option<u64>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut previous = 0i64;
    for &(length, lcn) in runs {
        let length_bytes = (1..=8).find(|&n| n == 8 || length >> (8 * n) == 0).unwrap();
        let (offset_bytes, delta) = match lcn {
            None => (0, 0),
            Some(lcn) => {
                let delta = lcn as i64 - previous;
                previous = lcn as i64;
                let n = (1..=8)
                    .find(|&n| n == 8 || (delta << (64 - 8 * n)) >> (64 - 8 * n) == delta)
                    .unwrap();
                (n, delta)
            }
        };
        bytes.push((offset_bytes << 4 | length_bytes) as u8);
        bytes.extend_from_slice(&length.to_le_bytes()[..length_bytes]);
        bytes.extend_from_slice(&delta.to_le_bytes()[..offset_bytes]);
    }
    bytes
}

pub fn symlink_reparse(target: &str, relative: bool) -> Vec<u8> {
    let name = utf16(target);
    let mut body = Vec::new();
    for value in [
        0u16,
        name.len() as u16,
        name.len() as u16,
        name.len() as u16,
    ] {
        body.extend(value.to_le_bytes());
    }
    body.extend(u32::from(relative).to_le_bytes());
    body.extend(&name);
    body.extend(&name);
    reparse(SYMLINK_TAG, &body)
}

pub fn junction_reparse(target: &str) -> Vec<u8> {
    let name = utf16(target);
    let mut body = Vec::new();
    for value in [
        0u16,
        name.len() as u16,
        name.len() as u16 + 2,
        name.len() as u16,
    ] {
        body.extend(value.to_le_bytes());
    }
    body.extend(&name);
    body.extend([0, 0]);
    body.extend(&name);
    body.extend([0, 0]);
    reparse(JUNCTION_TAG, &body)
}

pub fn reparse(tag: u32, body: &[u8]) -> Vec<u8> {
    let mut data = tag.to_le_bytes().to_vec();
    data.extend((body.len() as u16).to_le_bytes());
    data.extend([0, 0]);
    data.extend(body);
    data
}

#[derive(Clone, Debug)]
pub struct Record {
    pub flags: u16,
    pub sequence: u16,
    pub base: u64,
    pub attributes: Vec<Vec<u8>>,
}

impl Record {
    pub fn new(flags: u16, attributes: Vec<Vec<u8>>) -> Self {
        Record {
            flags,
            sequence: 1,
            base: 0,
            attributes,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub sector: usize,
    pub cluster: usize,
    pub record: usize,
    pub records: usize,
    pub clusters: u64,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry {
            sector: SECTOR,
            cluster: 4096,
            record: 1024,
            records: 64,
            clusters: 512,
        }
    }
}

pub fn record_bytes(geometry: &Geometry, index: u64, record: &Record) -> Vec<u8> {
    let sectors = geometry.record / geometry.sector;
    let usa = 0x30;
    let first = (usa + 2 * (sectors + 1)).div_ceil(8) * 8;
    let mut bytes = vec![0u8; geometry.record];
    bytes[0..4].copy_from_slice(b"FILE");
    bytes[4..6].copy_from_slice(&(usa as u16).to_le_bytes());
    bytes[6..8].copy_from_slice(&((sectors + 1) as u16).to_le_bytes());
    bytes[0x10..0x12].copy_from_slice(&record.sequence.to_le_bytes());
    bytes[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
    bytes[0x16..0x18].copy_from_slice(&record.flags.to_le_bytes());
    bytes[0x1C..0x20].copy_from_slice(&(geometry.record as u32).to_le_bytes());
    bytes[0x20..0x28].copy_from_slice(&record.base.to_le_bytes());
    bytes[0x2C..0x30].copy_from_slice(&(index as u32).to_le_bytes());
    let mut at = first;
    for attribute in &record.attributes {
        assert!(
            at + attribute.len() + 8 <= geometry.record,
            "record {index} overflows"
        );
        bytes[at..at + attribute.len()].copy_from_slice(attribute);
        at += attribute.len();
    }
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    at += 8;
    bytes[0x18..0x1C].copy_from_slice(&(at as u32).to_le_bytes());
    apply_update_sequence(&mut bytes, geometry.sector, 0x5A5A);
    bytes
}

pub fn apply_update_sequence(bytes: &mut [u8], sector: usize, check: u16) {
    let usa = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    let count = usize::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    bytes[usa..usa + 2].copy_from_slice(&check.to_le_bytes());
    for index in 1..count {
        let end = index * sector - 2;
        let saved = [bytes[end], bytes[end + 1]];
        bytes[usa + 2 * index..usa + 2 * index + 2].copy_from_slice(&saved);
        bytes[end..end + 2].copy_from_slice(&check.to_le_bytes());
    }
}

pub fn boot_sector(geometry: &Geometry, mft_cluster: u64) -> Vec<u8> {
    let mut boot = vec![0u8; SECTOR];
    boot[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    boot[3..11].copy_from_slice(b"NTFS    ");
    boot[11..13].copy_from_slice(&(geometry.sector as u16).to_le_bytes());
    boot[13] = (geometry.cluster / geometry.sector) as u8;
    boot[21] = 0xF8;
    boot[0x18..0x1A].copy_from_slice(&63u16.to_le_bytes());
    boot[0x1A..0x1C].copy_from_slice(&255u16.to_le_bytes());
    boot[0x24] = 0x80;
    boot[0x26] = 0x80;
    let sectors = geometry.clusters * (geometry.cluster / geometry.sector) as u64 - 1;
    boot[0x28..0x30].copy_from_slice(&sectors.to_le_bytes());
    boot[0x30..0x38].copy_from_slice(&mft_cluster.to_le_bytes());
    boot[0x38..0x40].copy_from_slice(&(mft_cluster + 1).to_le_bytes());
    let record_field = if geometry.record >= geometry.cluster {
        (geometry.record / geometry.cluster) as u32
    } else {
        (256 - geometry.record.trailing_zeros()) & 0xFF
    };
    boot[0x40..0x44].copy_from_slice(&record_field.to_le_bytes());
    boot[0x44..0x48].copy_from_slice(&1u32.to_le_bytes());
    boot[0x48..0x50].copy_from_slice(&0x1234_5678_9ABC_DEF0u64.to_le_bytes());
    boot[510] = 0x55;
    boot[511] = 0xAA;
    boot
}

pub struct Image {
    pub geometry: Geometry,
    pub bytes: Vec<u8>,
    pub mft_cluster: u64,
    pub next_cluster: u64,
    pub records: Vec<Option<Record>>,
    written: Vec<bool>,
}

impl Image {
    pub fn new(geometry: Geometry) -> Self {
        let mft_clusters = (geometry.records * geometry.record).div_ceil(geometry.cluster) as u64;
        let mft_cluster = 4u64.max((8192 / geometry.cluster) as u64);
        Image {
            geometry,
            bytes: vec![0; geometry.clusters as usize * geometry.cluster],
            mft_cluster,
            next_cluster: mft_cluster + mft_clusters,
            records: vec![None; geometry.records],
            written: vec![false; geometry.records],
        }
    }

    pub fn write_record_now(&mut self, index: u64, record: &Record) {
        let bytes = record_bytes(&self.geometry, index, record);
        let at = self.record_offset(index);
        self.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
        self.written[index as usize] = true;
    }

    pub fn record_offset(&self, index: u64) -> usize {
        self.mft_cluster as usize * self.geometry.cluster + index as usize * self.geometry.record
    }

    pub fn find_by_first_name(&self, name: &str) -> u64 {
        let wanted = file_name_value(0, 0, name)[0x42..].to_vec();
        self.records
            .iter()
            .position(|record| {
                record.as_ref().is_some_and(|record| {
                    record.attributes.iter().any(|attribute| {
                        u32::from_le_bytes(attribute[0..4].try_into().unwrap()) == FILE_NAME
                            && attribute.ends_with(&wanted)
                            || u32::from_le_bytes(attribute[0..4].try_into().unwrap()) == FILE_NAME
                                && attribute
                                    .windows(wanted.len())
                                    .any(|window| window == wanted.as_slice())
                    })
                })
            })
            .expect("no record has that name") as u64
    }

    pub fn record_mut(&mut self, index: u64) -> &mut Record {
        self.records[index as usize]
            .as_mut()
            .expect("record not set")
    }

    pub fn cluster_bytes(&self) -> u64 {
        self.geometry.cluster as u64
    }

    pub fn reserve(&mut self, count: u64) -> u64 {
        let first = self.next_cluster;
        self.next_cluster += count;
        assert!(
            self.next_cluster <= self.geometry.clusters,
            "the test volume is full"
        );
        first
    }

    pub fn place(&mut self, data: &[u8]) -> u64 {
        let count = (data.len() as u64).div_ceil(self.cluster_bytes()).max(1);
        let first = self.reserve(count);
        let at = first as usize * self.geometry.cluster;
        self.bytes[at..at + data.len()].copy_from_slice(data);
        first
    }

    pub fn write_at_cluster(&mut self, cluster: u64, data: &[u8]) {
        let at = cluster as usize * self.geometry.cluster;
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }

    pub fn set(&mut self, index: u64, record: Record) {
        self.records[index as usize] = Some(record);
    }

    pub fn mft_record(&self) -> Record {
        let size = (self.geometry.records * self.geometry.record) as u64;
        let clusters = size.div_ceil(self.cluster_bytes());
        let sizes = Sizes {
            high_vcn: clusters - 1,
            allocated: clusters * self.cluster_bytes(),
            size,
            initialized: size,
            ..Sizes::default()
        };
        Record::new(
            IN_USE,
            vec![
                standard_information_all_times(TICKS_2020, 0x6),
                file_name(reference(ROOT, 5), WIN32_AND_DOS_NAME, "$MFT"),
                non_resident(
                    DATA,
                    "",
                    sizes,
                    &runlist(&[(clusters, Some(self.mft_cluster))]),
                ),
            ],
        )
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.records[0].is_none() {
            self.records[0] = Some(self.mft_record());
        }
        let geometry = self.geometry;
        let mft_at = self.mft_cluster as usize * geometry.cluster;
        for index in 0..geometry.records {
            if self.written[index] {
                continue;
            }
            let record = self.records[index]
                .clone()
                .unwrap_or_else(|| Record::new(0, Vec::new()));
            let bytes = record_bytes(&geometry, index as u64, &record);
            let at = mft_at + index * geometry.record;
            self.bytes[at..at + geometry.record].copy_from_slice(&bytes);
        }
        let boot = boot_sector(&geometry, self.mft_cluster);
        self.bytes[..SECTOR].copy_from_slice(&boot);
        self.bytes
    }
}

pub const TICKS_2020: u64 = 132_223_104_000_000_000;

pub fn lznt1_padded_to_chunks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(LZNT1_CHUNK) {
        let packed = lznt1_chunk(chunk);
        if packed.len() >= LZNT1_CHUNK {
            out.extend_from_slice(&((LZNT1_CHUNK - 1) as u16 | 0x3000).to_le_bytes());
            out.extend_from_slice(chunk);
            out.resize(out.len() + LZNT1_CHUNK - chunk.len(), 0);
        } else {
            out.extend_from_slice(&((packed.len() - 1) as u16 | 0xB000).to_le_bytes());
            out.extend_from_slice(&packed);
        }
    }
    out
}

fn lznt1_chunk(chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < chunk.len() {
        let flags_at = out.len();
        out.push(0u8);
        for bit in 0..8 {
            if at == chunk.len() {
                break;
            }
            let mut bits = 4;
            while (at.max(1) - 1) >> bits != 0 {
                bits += 1;
            }
            let longest = (0xFFFF >> bits) + 3;
            let mut best = (0, 0);
            for distance in 1..=at.min(1 << bits).min(64) {
                let mut count = 0;
                while at + count < chunk.len()
                    && count < longest
                    && chunk[at + count] == chunk[at + count - distance]
                {
                    count += 1;
                }
                if count > best.0 {
                    best = (count, distance);
                }
            }
            if best.0 >= 3 {
                let token = ((best.1 - 1) << (16 - bits)) | (best.0 - 3);
                out.extend_from_slice(&(token as u16).to_le_bytes());
                out[flags_at] |= 1 << bit;
                at += best.0;
            } else {
                out.push(chunk[at]);
                at += 1;
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    ResidentOrOneRun,
    Fragments(usize),
    Lznt1,
    SparseWithUnitLog(u8),
    InExtensionRecord,
}

#[derive(Clone, Debug)]
pub enum Node {
    Dir {
        name: String,
        children: Vec<Node>,
        ticks: u64,
    },
    File {
        name: String,
        data: Vec<u8>,
        form: Form,
        attributes: u32,
        ticks: u64,
        dos_name: Option<String>,
        link_group: u32,
        streams: Vec<(String, Vec<u8>)>,
    },
    Link {
        name: String,
        reparse: Vec<u8>,
        directory: bool,
    },
}

pub fn dir(name: &str, children: Vec<Node>) -> Node {
    Node::Dir {
        name: name.to_owned(),
        children,
        ticks: TICKS_2020,
    }
}

pub fn file(name: &str, data: &[u8]) -> Node {
    Node::File {
        name: name.to_owned(),
        data: data.to_vec(),
        form: Form::ResidentOrOneRun,
        attributes: 0x20,
        ticks: TICKS_2020 + 10_000_000,
        dos_name: None,
        link_group: 0,
        streams: Vec::new(),
    }
}

pub fn file_as(name: &str, data: &[u8], form: Form) -> Node {
    match file(name, data) {
        Node::File {
            name,
            data,
            attributes,
            ticks,
            dos_name,
            link_group,
            streams,
            ..
        } => Node::File {
            name,
            data,
            form,
            attributes,
            ticks,
            dos_name,
            link_group,
            streams,
        },
        _ => unreachable!(),
    }
}

pub fn hard_link_in_group(name: &str, data: &[u8], group: u32) -> Node {
    match file(name, data) {
        Node::File {
            name,
            data,
            form,
            attributes,
            ticks,
            dos_name,
            streams,
            ..
        } => Node::File {
            name,
            data,
            form,
            attributes,
            ticks,
            dos_name,
            link_group: group,
            streams,
        },
        _ => unreachable!(),
    }
}

pub fn link(name: &str, reparse: Vec<u8>, directory: bool) -> Node {
    Node::Link {
        name: name.to_owned(),
        reparse,
        directory,
    }
}

struct Builder {
    image: Image,
    next_record: u64,
    groups: std::collections::HashMap<u32, u64>,
}

impl Builder {
    fn record_index(&mut self) -> u64 {
        let index = self.next_record;
        self.next_record += 1;
        index
    }

    fn data_attributes(&mut self, data: &[u8], form: Form, name: &str) -> Vec<Vec<u8>> {
        let cluster = self.image.cluster_bytes();
        let clusters = (data.len() as u64).div_ceil(cluster);
        match form {
            Form::ResidentOrOneRun if data.len() <= 400 => vec![resident(DATA, name, data)],
            Form::ResidentOrOneRun | Form::InExtensionRecord | Form::Fragments(1) => {
                let lcn = self.image.place(data);
                let runs = if clusters == 0 {
                    Vec::new()
                } else {
                    runlist(&[(clusters, Some(lcn))])
                };
                vec![non_resident(
                    DATA,
                    name,
                    Sizes {
                        high_vcn: clusters.wrapping_sub(1),
                        allocated: clusters * cluster,
                        size: data.len() as u64,
                        initialized: data.len() as u64,
                        ..Sizes::default()
                    },
                    &runs,
                )]
            }
            Form::Fragments(pieces) => {
                let per = clusters.div_ceil(pieces as u64).max(1);
                let mut spans = Vec::new();
                let mut vcn = 0;
                while vcn < clusters {
                    let count = per.min(clusters - vcn);
                    spans.push((vcn, count));
                    vcn += count;
                }
                let mut lcns = vec![0; spans.len()];
                for (slot, &(vcn, count)) in spans.iter().enumerate().rev() {
                    let start = (vcn * cluster) as usize;
                    let end = (((vcn + count) * cluster) as usize).min(data.len());
                    lcns[slot] = self.image.place(&data[start..end]);
                    self.image.reserve(1);
                }
                let order: Vec<(u64, Option<u64>)> = spans
                    .iter()
                    .zip(&lcns)
                    .map(|(&(_, count), &lcn)| (count, Some(lcn)))
                    .collect();
                vec![non_resident(
                    DATA,
                    name,
                    Sizes {
                        high_vcn: clusters - 1,
                        allocated: clusters * cluster,
                        size: data.len() as u64,
                        initialized: data.len() as u64,
                        ..Sizes::default()
                    },
                    &runlist(&order),
                )]
            }
            Form::Lznt1 => {
                let unit = UNIT_CLUSTERS as u64 * cluster;
                let units = (data.len() as u64).div_ceil(unit);
                let mut runs = Vec::new();
                let mut packed_clusters = 0;
                for index in 0..units {
                    let start = (index * unit) as usize;
                    let end = (start + unit as usize).min(data.len());
                    let piece = &data[start..end];
                    if piece.iter().all(|&byte| byte == 0) {
                        runs.push((UNIT_CLUSTERS as u64, None));
                        continue;
                    }
                    let packed = lznt1_padded_to_chunks(piece);
                    let used = (packed.len() as u64).div_ceil(cluster);
                    if used < UNIT_CLUSTERS as u64 {
                        let lcn = self.image.place(&packed);
                        runs.push((used, Some(lcn)));
                        runs.push((UNIT_CLUSTERS as u64 - used, None));
                        packed_clusters += used;
                    } else {
                        let lcn = self.image.place(piece);
                        self.image
                            .reserve(UNIT_CLUSTERS as u64 - (piece.len() as u64).div_ceil(cluster));
                        runs.push((UNIT_CLUSTERS as u64, Some(lcn)));
                        packed_clusters += UNIT_CLUSTERS as u64;
                    }
                }
                let allocated_clusters = units * UNIT_CLUSTERS as u64;
                vec![non_resident(
                    DATA,
                    name,
                    Sizes {
                        high_vcn: allocated_clusters.wrapping_sub(1),
                        allocated: allocated_clusters * cluster,
                        size: data.len() as u64,
                        initialized: data.len() as u64,
                        unit: 4,
                        flags: COMPRESSED,
                        packed: Some(packed_clusters * cluster),
                        low_vcn: 0,
                    },
                    &runlist(&merged(&runs)),
                )]
            }
            Form::SparseWithUnitLog(unit) => {
                let mut runs = Vec::new();
                let mut placed_clusters = 0;
                for index in 0..clusters {
                    let start = (index * cluster) as usize;
                    let end = (start + cluster as usize).min(data.len());
                    let piece = &data[start..end];
                    if piece.iter().all(|&byte| byte == 0) {
                        runs.push((1, None));
                    } else {
                        let lcn = self.image.place(piece);
                        runs.push((1, Some(lcn)));
                        placed_clusters += 1;
                    }
                }
                let allocated_clusters = if unit == 0 {
                    clusters
                } else {
                    clusters.div_ceil(UNIT_CLUSTERS as u64) * UNIT_CLUSTERS as u64
                };
                if allocated_clusters > clusters {
                    runs.push((allocated_clusters - clusters, None));
                }
                vec![non_resident(
                    DATA,
                    name,
                    Sizes {
                        high_vcn: allocated_clusters.wrapping_sub(1),
                        allocated: allocated_clusters * cluster,
                        size: data.len() as u64,
                        initialized: data.len() as u64,
                        unit,
                        flags: SPARSE,
                        packed: Some(placed_clusters * cluster),
                        low_vcn: 0,
                    },
                    &runlist(&merged(&runs)),
                )]
            }
        }
    }

    fn add(&mut self, parent: u64, node: &Node) {
        match node {
            Node::Dir {
                name,
                children,
                ticks,
            } => {
                let index = self.record_index();
                self.image.set(
                    index,
                    Record::new(
                        IN_USE | DIRECTORY,
                        vec![
                            standard_information_all_times(*ticks, 0x10),
                            file_name(reference(parent, 1), WIN32_AND_DOS_NAME, name),
                        ],
                    ),
                );
                for child in children {
                    self.add(index, child);
                }
            }
            Node::Link {
                name,
                reparse,
                directory,
            } => {
                let index = self.record_index();
                let flags = if *directory {
                    IN_USE | DIRECTORY
                } else {
                    IN_USE
                };
                self.image.set(
                    index,
                    Record::new(
                        flags,
                        vec![
                            standard_information_all_times(TICKS_2020, 0x400),
                            file_name(reference(parent, 1), WIN32_AND_DOS_NAME, name),
                            resident(REPARSE_POINT, "", reparse),
                        ],
                    ),
                );
            }
            Node::File {
                name,
                data,
                form,
                attributes,
                ticks,
                dos_name,
                link_group,
                streams,
            } => {
                if *link_group != 0
                    && let Some(&index) = self.groups.get(link_group)
                {
                    let record = self.image.records[index as usize].as_mut().unwrap();
                    record
                        .attributes
                        .insert(1, file_name(reference(parent, 1), POSIX_NAME, name));
                    return;
                }
                let index = self.record_index();
                if *link_group != 0 {
                    self.groups.insert(*link_group, index);
                }
                let kind = if dos_name.is_some() {
                    WIN32_NAME
                } else {
                    WIN32_AND_DOS_NAME
                };
                let mut attributes_list = vec![
                    standard_information_all_times(*ticks, *attributes),
                    file_name(reference(parent, 1), kind, name),
                ];
                if let Some(dos) = dos_name {
                    attributes_list.push(file_name(reference(parent, 1), DOS_NAME, dos));
                }
                let mut data_attributes = self.data_attributes(data, *form, "");
                for (stream, bytes) in streams {
                    data_attributes.extend(self.data_attributes(
                        bytes,
                        Form::ResidentOrOneRun,
                        stream,
                    ));
                }
                if *form == Form::InExtensionRecord {
                    let extension = self.record_index();
                    let mut list = Vec::new();
                    for (kind, record) in [
                        (STANDARD_INFORMATION, index),
                        (FILE_NAME, index),
                        (DATA, extension),
                    ] {
                        let mut entry = vec![0u8; 0x20];
                        entry[0..4].copy_from_slice(&kind.to_le_bytes());
                        entry[4..6].copy_from_slice(&0x20u16.to_le_bytes());
                        entry[6] = 0;
                        entry[7] = 0x1A;
                        entry[0x10..0x18].copy_from_slice(&reference(record, 1).to_le_bytes());
                        list.extend(entry);
                    }
                    attributes_list.insert(1, resident(ATTRIBUTE_LIST, "", &list));
                    let mut extension_record = Record::new(IN_USE, data_attributes);
                    extension_record.base = reference(index, 1);
                    self.image.set(extension, extension_record);
                } else {
                    attributes_list.extend(data_attributes);
                }
                self.image.set(index, Record::new(IN_USE, attributes_list));
            }
        }
    }
}

fn merged(runs: &[(u64, Option<u64>)]) -> Vec<(u64, Option<u64>)> {
    let mut out: Vec<(u64, Option<u64>)> = Vec::new();
    for &(count, lcn) in runs {
        match (out.last_mut(), lcn) {
            (Some((last_count, None)), None) => *last_count += count,
            (Some((last_count, Some(last))), Some(lcn)) if *last + *last_count == lcn => {
                *last_count += count
            }
            _ => out.push((count, lcn)),
        }
    }
    out
}

pub fn build(geometry: Geometry, nodes: &[Node]) -> Vec<u8> {
    build_image(geometry, nodes).finish()
}

pub fn build_image(geometry: Geometry, nodes: &[Node]) -> Image {
    let mut builder = Builder {
        image: Image::new(geometry),
        next_record: 16,
        groups: std::collections::HashMap::new(),
    };
    builder.image.set(
        ROOT,
        Record::new(
            IN_USE | DIRECTORY,
            vec![
                standard_information_all_times(TICKS_2020, 0x16),
                file_name(reference(ROOT, 1), WIN32_AND_DOS_NAME, "."),
            ],
        ),
    );
    for node in nodes {
        builder.add(ROOT, node);
    }
    builder.image
}
