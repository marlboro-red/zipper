#![allow(dead_code)]
//! Writes ISO 9660 images with Joliet, Rock Ridge and El Torito, and UDF
//! file systems beside or instead of them, for the differential and hostile
//! tests. The layout records where each structure sits, so a test can damage
//! one field and recompute the UDF tag around it.

use std::collections::BTreeMap;

pub const SECTOR: usize = 2048;
const FIRST_FREE_SECTOR: u32 = 264;
const ANCHOR_SECTOR: u32 = 256;
const RECORDING_DATE: [u8; 7] = [124, 1, 2, 3, 4, 5, 0];

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Dir,
    File(Vec<u8>),
    Symlink(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub path: String,
    pub kind: Kind,
    pub mode: u32,
    pub iso_id: Option<Vec<u8>>,
    pub rock_ridge_name: Option<Vec<u8>>,
    pub joliet_id: Option<Vec<u16>>,
    pub udf_name: Option<Vec<u8>>,
}

pub fn dir(path: &str) -> Item {
    Item {
        path: path.to_owned(),
        kind: Kind::Dir,
        mode: 0o40755,
        iso_id: None,
        rock_ridge_name: None,
        joliet_id: None,
        udf_name: None,
    }
}

pub fn file(path: &str, data: &[u8]) -> Item {
    Item {
        kind: Kind::File(data.to_vec()),
        mode: 0o100644,
        ..dir(path)
    }
}

pub fn symlink(path: &str, target: &str) -> Item {
    Item {
        kind: Kind::Symlink(target.to_owned()),
        mode: 0o120777,
        ..dir(path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ads {
    Short,
    Long,
    Inline,
}

#[derive(Clone, Debug)]
pub struct Udf {
    pub revision: u16,
    pub extended: bool,
    pub ads: Ads,
    pub metadata: bool,
    pub inline_dirs: bool,
    pub partition_gap: u32,
}

impl Default for Udf {
    fn default() -> Self {
        Udf {
            revision: 0x0102,
            extended: false,
            ads: Ads::Short,
            metadata: false,
            inline_dirs: false,
            partition_gap: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Boot {
    pub bootable: bool,
    pub media: u8,
    pub sectors: u16,
    pub data: Vec<u8>,
    pub vendor: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Disc {
    pub items: Vec<Item>,
    pub iso: bool,
    pub joliet: bool,
    pub rock_ridge: bool,
    pub udf: Option<Udf>,
    pub boot: Vec<Boot>,
    pub extent_limit: Option<u32>,
    pub names_in_continuation: bool,
    pub joliet_versions: bool,
}

#[derive(Default, Debug)]
pub struct Layout {
    pub primary: Option<usize>,
    pub joliet: Option<usize>,
    pub boot_record: Option<usize>,
    pub catalog: Option<usize>,
    pub iso_dirs: BTreeMap<String, (u32, u32)>,
    pub iso_records: BTreeMap<String, usize>,
    pub joliet_dirs: BTreeMap<String, (u32, u32)>,
    pub joliet_records: BTreeMap<String, usize>,
    pub continuation: BTreeMap<String, usize>,
    pub file_data: BTreeMap<String, Vec<(u32, u32)>>,
    pub anchor: Option<usize>,
    pub partition_descriptor: Option<usize>,
    pub logical_volume: Option<usize>,
    pub partition_start: u32,
    pub metadata_start: u32,
    pub file_set: Option<usize>,
    pub udf_entries: BTreeMap<String, usize>,
    pub udf_fids: BTreeMap<String, usize>,
    pub udf_dir_data: BTreeMap<String, (usize, usize)>,
    pub metadata_file: Option<usize>,
}

pub struct Built {
    pub bytes: Vec<u8>,
    pub layout: Layout,
}

fn both16(value: u16) -> [u8; 4] {
    let le = value.to_le_bytes();
    let be = value.to_be_bytes();
    [le[0], le[1], be[0], be[1]]
}

fn both32(value: u32) -> [u8; 8] {
    let mut out = [0; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Fill in the UDF tag at `at` for a descriptor of `length` bytes, with
/// its identifier, version and location already written.
pub fn retag(bytes: &mut [u8], at: usize, length: usize) {
    let crc_length = (length - 16) as u16;
    bytes[at + 10..at + 12].copy_from_slice(&crc_length.to_le_bytes());
    let crc = crc16(&bytes[at + 16..at + length]);
    bytes[at + 8..at + 10].copy_from_slice(&crc.to_le_bytes());
    bytes[at + 5] = 0;
    let sum = bytes[at..at + 16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    bytes[at + 4] = sum;
}

/// Recompute the tag at `at` with the CRC length the tag already holds.
pub fn retag_in_place(bytes: &mut [u8], at: usize) {
    let crc_length = u16::from_le_bytes([bytes[at + 10], bytes[at + 11]]) as usize;
    retag(bytes, at, crc_length + 16);
}

fn put_tag(bytes: &mut [u8], at: usize, id: u16, version: u16, location: u32, length: usize) {
    bytes[at..at + 2].copy_from_slice(&id.to_le_bytes());
    bytes[at + 2..at + 4].copy_from_slice(&version.to_le_bytes());
    bytes[at + 12..at + 16].copy_from_slice(&location.to_le_bytes());
    retag(bytes, at, length);
}

fn udf_time() -> [u8; 12] {
    let mut time = [0u8; 12];
    time[0..2].copy_from_slice(&(0x1000u16).to_le_bytes());
    time[2..4].copy_from_slice(&2024u16.to_le_bytes());
    time[4..10].copy_from_slice(&[1, 2, 3, 4, 5, 0]);
    time
}

fn entity(bytes: &mut [u8], at: usize, id: &[u8], suffix: &[u8]) {
    bytes[at + 1..at + 1 + id.len()].copy_from_slice(id);
    bytes[at + 24..at + 24 + suffix.len()].copy_from_slice(suffix);
}

pub fn iso_default_id(name: &str, is_dir: bool) -> Vec<u8> {
    let mut id: Vec<u8> = name
        .bytes()
        .map(|byte| match byte.to_ascii_uppercase() {
            upper @ (b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.') => upper,
            _ => b'_',
        })
        .collect();
    if is_dir {
        id.retain(|byte| *byte != b'.');
        return id;
    }
    match id.iter().rposition(|byte| *byte == b'.') {
        Some(last) => {
            for byte in &mut id[..last] {
                if *byte == b'.' {
                    *byte = b'_';
                }
            }
        }
        None => id.push(b'.'),
    }
    id.extend_from_slice(b";1");
    id
}

#[derive(Clone)]
struct Node {
    item: Item,
    children: Vec<usize>,
}

struct Tree {
    nodes: Vec<Node>,
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap()
}

impl Tree {
    fn new(items: &[Item]) -> Tree {
        let mut nodes = vec![Node {
            item: dir(""),
            children: Vec::new(),
        }];
        let mut by_path: BTreeMap<String, usize> = BTreeMap::from([(String::new(), 0)]);
        for item in items {
            let parent = match item.path.rfind('/') {
                Some(slash) => &item.path[..slash],
                None => "",
            };
            let parent = *by_path
                .get(parent)
                .unwrap_or_else(|| panic!("{} comes before its folder", item.path));
            let index = nodes.len();
            nodes.push(Node {
                item: item.clone(),
                children: Vec::new(),
            });
            nodes[parent].children.push(index);
            by_path.insert(item.path.clone(), index);
        }
        Tree { nodes }
    }

    fn is_dir(&self, index: usize) -> bool {
        self.nodes[index].item.kind == Kind::Dir
    }
}

struct Writer {
    bytes: Vec<u8>,
    next: u32,
}

impl Writer {
    fn alloc(&mut self, length: usize) -> u32 {
        let sectors = length.div_ceil(SECTOR).max(1) as u32;
        let start = self.next;
        self.next += sectors;
        self.ensure(self.next);
        start
    }

    fn ensure(&mut self, sectors: u32) {
        let length = sectors as usize * SECTOR;
        if self.bytes.len() < length {
            self.bytes.resize(length, 0);
        }
    }

    fn at(sector: u32) -> usize {
        sector as usize * SECTOR
    }
}

struct IsoRecord {
    id: Vec<u8>,
    flags: u8,
    sectors: Vec<(u32, u32)>,
    system_use: Vec<u8>,
    continuation: Option<Vec<u8>>,
}

fn record_bytes(record: &IsoRecord, continuation_at: Option<(u32, u32)>) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, (sector, size)) in record.sectors.iter().enumerate() {
        let id_length = record.id.len();
        let pad = usize::from(id_length.is_multiple_of(2));
        let mut system_use = record.system_use.clone();
        if let (Some(_), Some((sector, length))) = (&record.continuation, continuation_at) {
            system_use.extend_from_slice(b"CE\x1c\x01");
            system_use.extend_from_slice(&both32(sector));
            system_use.extend_from_slice(&both32(0));
            system_use.extend_from_slice(&both32(length));
        }
        let length = 33 + id_length + pad + system_use.len();
        assert!(length <= 255, "a record of {length} bytes");
        let mut bytes = vec![0u8; length];
        bytes[0] = length as u8;
        bytes[2..10].copy_from_slice(&both32(*sector));
        bytes[10..18].copy_from_slice(&both32(*size));
        bytes[18..25].copy_from_slice(&RECORDING_DATE);
        let last = index + 1 == record.sectors.len();
        bytes[25] = record.flags | if last { 0 } else { 0x80 };
        bytes[28..32].copy_from_slice(&both16(1));
        bytes[32] = id_length as u8;
        bytes[33..33 + id_length].copy_from_slice(&record.id);
        bytes[33 + id_length + pad..].copy_from_slice(&system_use);
        out.extend_from_slice(&bytes);
    }
    out
}

fn rock_ridge_px(mode: u32) -> Vec<u8> {
    let mut entry = b"PX\x2c\x01".to_vec();
    for value in [mode, 1, 0, 0, 0] {
        entry.extend_from_slice(&both32(value));
    }
    entry
}

fn rock_ridge_nm(name: &[u8]) -> Vec<u8> {
    let mut entry = vec![b'N', b'M', (5 + name.len()) as u8, 1, 0];
    entry.extend_from_slice(name);
    entry
}

pub fn rock_ridge_sl(target: &str) -> Vec<u8> {
    let mut components = Vec::new();
    if target.starts_with('/') {
        components.extend_from_slice(&[8, 0]);
    }
    for part in target.split('/').filter(|part| !part.is_empty()) {
        match part {
            "." => components.extend_from_slice(&[2, 0]),
            ".." => components.extend_from_slice(&[4, 0]),
            _ => {
                components.extend_from_slice(&[0, part.len() as u8]);
                components.extend_from_slice(part.as_bytes());
            }
        }
    }
    let mut entry = vec![b'S', b'L', (5 + components.len()) as u8, 1, 0];
    entry.extend_from_slice(&components);
    entry
}

fn joliet_units(name: &str, is_dir: bool, versions: bool) -> Vec<u16> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    if !is_dir && versions {
        units.extend(";1".encode_utf16());
    }
    units
}

fn units_be(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_be_bytes()).collect()
}

struct Prepared {
    records: Vec<Vec<IsoRecord>>,
}

impl Disc {
    pub fn build(&self) -> Built {
        let tree = Tree::new(&self.items);
        let mut writer = Writer {
            bytes: vec![0; Writer::at(FIRST_FREE_SECTOR)],
            next: FIRST_FREE_SECTOR,
        };
        let mut layout = Layout::default();
        let udf_tag_version = match &self.udf {
            Some(udf) if udf.revision >= 0x0200 => 3,
            _ => 2,
        };
        let mut descriptor = 16u32;
        let mut take = || {
            let at = descriptor;
            descriptor += 1;
            at
        };
        let primary = self.iso.then(&mut take);
        let boot_record = (self.iso && !self.boot.is_empty()).then(&mut take);
        let joliet = (self.iso && self.joliet).then(&mut take);
        if self.iso {
            let terminator = Writer::at(take());
            writer.bytes[terminator..terminator + 7].copy_from_slice(b"\xffCD001\x01");
        }
        if self.udf.is_some() {
            for id in [
                &b"BEA01"[..],
                if udf_tag_version == 3 {
                    b"NSR03"
                } else {
                    b"NSR02"
                },
                b"TEA01",
            ] {
                let at = Writer::at(take());
                writer.bytes[at + 1..at + 6].copy_from_slice(id);
                writer.bytes[at + 6] = 1;
            }
        }

        let catalog = boot_record.map(|_| writer.alloc(SECTOR));
        let boot_data: Vec<u32> = self
            .boot
            .iter()
            .map(|boot| writer.alloc(boot.data.len()))
            .collect();
        for (boot, sector) in self.boot.iter().zip(&boot_data) {
            let at = Writer::at(*sector);
            writer.bytes[at..at + boot.data.len()].copy_from_slice(&boot.data);
        }

        let primary_tree = self.iso.then(|| self.prepare(&tree, false));
        let joliet_tree = (self.iso && self.joliet).then(|| self.prepare(&tree, true));
        let mut dir_sectors_primary = vec![(0u32, 0u32); tree.nodes.len()];
        let mut dir_sectors_joliet = vec![(0u32, 0u32); tree.nodes.len()];
        for (prepared, sectors) in [
            (&primary_tree, &mut dir_sectors_primary),
            (&joliet_tree, &mut dir_sectors_joliet),
        ] {
            let Some(prepared) = prepared else { continue };
            for (index, records) in prepared.records.iter().enumerate() {
                if !tree.is_dir(index) {
                    continue;
                }
                let size = packed_size(records);
                sectors[index] = (writer.alloc(size), size as u32);
            }
        }

        let partition_start = writer.next + self.udf.as_ref().map_or(0, |udf| udf.partition_gap);
        writer.next = partition_start;
        writer.ensure(writer.next);
        layout.partition_start = partition_start;
        let udf_plan = self
            .udf
            .as_ref()
            .map(|udf| UdfPlan::allocate(udf, &tree, &mut writer, partition_start));

        let mut file_sectors = vec![Vec::new(); tree.nodes.len()];
        for (index, node) in tree.nodes.iter().enumerate() {
            if let Kind::File(data) = &node.item.kind {
                let limit = self.extent_limit.unwrap_or(u32::MAX) as usize;
                let mut chunks: Vec<&[u8]> = data.chunks(limit.max(1)).collect();
                if chunks.is_empty() {
                    chunks.push(&[]);
                }
                for chunk in chunks {
                    let sector = if chunk.is_empty() {
                        0
                    } else {
                        writer.alloc(chunk.len())
                    };
                    let at = Writer::at(sector);
                    let only_inline = !self.iso
                        && self.udf.as_ref().is_some_and(|udf| {
                            udf.ads == Ads::Inline && entry_header(udf) + data.len() <= SECTOR
                        });
                    if !only_inline {
                        writer.bytes[at..at + chunk.len()].copy_from_slice(chunk);
                    }
                    file_sectors[index].push((sector, chunk.len() as u32));
                }
                layout
                    .file_data
                    .insert(node.item.path.clone(), file_sectors[index].clone());
            }
        }

        if let (Some(prepared), Some(sector)) = (&primary_tree, primary) {
            self.write_iso_tree(
                &tree,
                prepared,
                &dir_sectors_primary,
                &file_sectors,
                &mut writer,
                &mut layout.iso_dirs,
                &mut layout.iso_records,
                Some(&mut layout.continuation),
            );
            let at = Writer::at(sector);
            self.volume_descriptor(&mut writer.bytes, at, 1, dir_sectors_primary[0]);
            layout.primary = Some(at);
        }
        if let (Some(prepared), Some(sector)) = (&joliet_tree, joliet) {
            self.write_iso_tree(
                &tree,
                prepared,
                &dir_sectors_joliet,
                &file_sectors,
                &mut writer,
                &mut layout.joliet_dirs,
                &mut layout.joliet_records,
                None,
            );
            let at = Writer::at(sector);
            self.volume_descriptor(&mut writer.bytes, at, 2, dir_sectors_joliet[0]);
            layout.joliet = Some(at);
        }
        if let (Some(sector), Some(catalog)) = (boot_record, catalog) {
            let at = Writer::at(sector);
            writer.bytes[at..at + 7].copy_from_slice(b"\x00CD001\x01");
            writer.bytes[at + 7..at + 30].copy_from_slice(b"EL TORITO SPECIFICATION");
            writer.bytes[at + 0x47..at + 0x4B].copy_from_slice(&catalog.to_le_bytes());
            layout.boot_record = Some(at);
            let at = Writer::at(catalog);
            self.write_catalog(&mut writer.bytes[at..at + SECTOR], &boot_data);
            layout.catalog = Some(at);
        }
        if let (Some(udf), Some(plan)) = (&self.udf, udf_plan) {
            plan.write(
                udf,
                udf_tag_version,
                &tree,
                &file_sectors,
                &mut writer,
                &mut layout,
            );
        }
        let total = writer.next;
        writer.ensure(total);
        if let Some(at) = layout.primary {
            writer.bytes[at + 80..at + 88].copy_from_slice(&both32(total));
        }
        if let Some(at) = layout.joliet {
            writer.bytes[at + 80..at + 88].copy_from_slice(&both32(total));
        }
        Built {
            bytes: writer.bytes,
            layout,
        }
    }

    fn prepare(&self, tree: &Tree, joliet: bool) -> Prepared {
        let mut records: Vec<Vec<IsoRecord>> = Vec::new();
        for (index, node) in tree.nodes.iter().enumerate() {
            let mut list = Vec::new();
            if tree.is_dir(index) {
                for id in [vec![0u8], vec![1u8]] {
                    let mut system_use = Vec::new();
                    if self.rock_ridge && !joliet {
                        if index == 0 && id == [0] {
                            system_use.extend_from_slice(b"SP\x07\x01\xbe\xef\x00");
                        }
                        system_use.extend_from_slice(&rock_ridge_px(0o40755));
                    }
                    list.push(IsoRecord {
                        id,
                        flags: 2,
                        sectors: vec![(0, 0)],
                        system_use,
                        continuation: None,
                    });
                }
                let mut children: Vec<IsoRecord> = Vec::new();
                for child in &node.children {
                    let item = &tree.nodes[*child].item;
                    if matches!(item.kind, Kind::Symlink(_)) && (joliet || !self.rock_ridge) {
                        continue;
                    }
                    let is_dir = item.kind == Kind::Dir;
                    let name = base(&item.path);
                    let id =
                        if joliet {
                            units_be(&item.joliet_id.clone().unwrap_or_else(|| {
                                joliet_units(name, is_dir, self.joliet_versions)
                            }))
                        } else {
                            item.iso_id
                                .clone()
                                .unwrap_or_else(|| iso_default_id(name, is_dir))
                        };
                    let mut system_use = Vec::new();
                    let mut continuation = None;
                    if self.rock_ridge && !joliet {
                        system_use.extend_from_slice(&rock_ridge_px(item.mode));
                        let nm = rock_ridge_nm(
                            item.rock_ridge_name.as_deref().unwrap_or(name.as_bytes()),
                        );
                        let mut names = nm;
                        if let Kind::Symlink(target) = &item.kind {
                            names.extend_from_slice(&rock_ridge_sl(target));
                        }
                        if self.names_in_continuation
                            || 33 + id.len() + 1 + system_use.len() + names.len() > 255
                        {
                            continuation = Some(names);
                        } else {
                            system_use.extend_from_slice(&names);
                        }
                    }
                    let extents = match &item.kind {
                        Kind::File(data) => {
                            let limit = self.extent_limit.unwrap_or(u32::MAX) as usize;
                            data.len().div_ceil(limit.max(1)).max(1)
                        }
                        _ => 1,
                    };
                    children.push(IsoRecord {
                        id,
                        flags: if is_dir { 2 } else { 0 },
                        sectors: vec![(*child as u32, 0); extents],
                        system_use,
                        continuation,
                    });
                }
                children.sort_by(|a, b| a.id.cmp(&b.id));
                list.extend(children);
            }
            records.push(list);
        }
        Prepared { records }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_iso_tree(
        &self,
        tree: &Tree,
        prepared: &Prepared,
        dir_sectors: &[(u32, u32)],
        file_sectors: &[Vec<(u32, u32)>],
        writer: &mut Writer,
        dirs: &mut BTreeMap<String, (u32, u32)>,
        records_at: &mut BTreeMap<String, usize>,
        mut continuation_at: Option<&mut BTreeMap<String, usize>>,
    ) {
        let parent_of = {
            let mut parents = vec![0usize; tree.nodes.len()];
            for (index, node) in tree.nodes.iter().enumerate() {
                for child in &node.children {
                    parents[*child] = index;
                }
            }
            parents
        };
        for (index, records) in prepared.records.iter().enumerate() {
            if !tree.is_dir(index) {
                continue;
            }
            let (sector, size) = dir_sectors[index];
            dirs.insert(tree.nodes[index].item.path.clone(), (sector, size));
            let mut offset = Writer::at(sector);
            let end_of = |offset: usize| offset - offset % SECTOR + SECTOR;
            for (position, record) in records.iter().enumerate() {
                let target = match position {
                    0 => vec![dir_sectors[index]],
                    1 => vec![dir_sectors[parent_of[index]]],
                    _ => {
                        let child = record.sectors[0].0 as usize;
                        if tree.is_dir(child) {
                            vec![dir_sectors[child]]
                        } else if let Kind::File(_) = tree.nodes[child].item.kind {
                            file_sectors[child].clone()
                        } else {
                            vec![(0, 0)]
                        }
                    }
                };
                let continuation = record.continuation.as_ref().map(|area| {
                    let sector = writer.alloc(area.len());
                    let at = Writer::at(sector);
                    writer.bytes[at..at + area.len()].copy_from_slice(area);
                    if let (Some(map), Some(child)) =
                        (continuation_at.as_deref_mut(), record.sectors.first())
                    {
                        map.insert(tree.nodes[child.0 as usize].item.path.clone(), at);
                    }
                    (sector, area.len() as u32)
                });
                let bytes = record_bytes(
                    &IsoRecord {
                        id: record.id.clone(),
                        flags: record.flags,
                        sectors: target,
                        system_use: record.system_use.clone(),
                        continuation: record.continuation.clone(),
                    },
                    continuation,
                );
                for (written, piece) in split_records(&bytes).into_iter().enumerate() {
                    if offset + piece.len() > end_of(offset) {
                        offset = end_of(offset);
                    }
                    writer.bytes[offset..offset + piece.len()].copy_from_slice(piece);
                    if position >= 2 && written == 0 {
                        let child = record.sectors[0].0 as usize;
                        records_at.insert(tree.nodes[child].item.path.clone(), offset);
                    }
                    offset += piece.len();
                }
            }
        }
    }

    fn volume_descriptor(&self, bytes: &mut [u8], at: usize, kind: u8, root: (u32, u32)) {
        let d = &mut bytes[at..at + SECTOR];
        d[0] = kind;
        d[1..6].copy_from_slice(b"CD001");
        d[6] = 1;
        let text = |d: &mut [u8], from: usize, length: usize| {
            for index in 0..length {
                d[from + index] = if kind == 2 && index % 2 == 0 { 0 } else { b' ' };
            }
        };
        text(d, 8, 32);
        text(d, 40, 32);
        if kind == 2 {
            d[88..91].copy_from_slice(b"%/E");
        }
        d[120..124].copy_from_slice(&both16(1));
        d[124..128].copy_from_slice(&both16(1));
        d[128..132].copy_from_slice(&both16(SECTOR as u16));
        let root_record = record_bytes(
            &IsoRecord {
                id: vec![0],
                flags: 2,
                sectors: vec![root],
                system_use: Vec::new(),
                continuation: None,
            },
            None,
        );
        d[156..190].copy_from_slice(&root_record);
        for (from, length) in [
            (190, 128),
            (318, 128),
            (446, 128),
            (574, 128),
            (702, 37),
            (739, 37),
            (776, 37),
        ] {
            text(d, from, length);
        }
        for date in 0..4 {
            let from = 813 + date * 17;
            d[from..from + 16].copy_from_slice(b"0000000000000000");
        }
        d[881] = 1;
    }

    fn write_catalog(&self, catalog: &mut [u8], data: &[u32]) {
        catalog[0] = 1;
        catalog[30] = 0x55;
        catalog[31] = 0xAA;
        let sum = catalog[..32].chunks(2).fold(0u16, |sum, word| {
            sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]))
        });
        catalog[28..30].copy_from_slice(&(0u16.wrapping_sub(sum)).to_le_bytes());
        let entry = |slot: &mut [u8], boot: &Boot, sector: u32| {
            slot[0] = if boot.bootable { 0x88 } else { 0 };
            slot[1] = boot.media;
            slot[6..8].copy_from_slice(&boot.sectors.to_le_bytes());
            slot[8..12].copy_from_slice(&sector.to_le_bytes());
            if let Some(vendor) = &boot.vendor {
                slot[12] = 1;
                slot[13..13 + vendor.len()].copy_from_slice(vendor.as_bytes());
            }
        };
        let Some(first) = self.boot.first() else {
            return;
        };
        entry(&mut catalog[32..64], first, data[0]);
        if self.boot.len() > 1 {
            catalog[64] = 0x91;
            catalog[66..68].copy_from_slice(&((self.boot.len() - 1) as u16).to_le_bytes());
            for (index, boot) in self.boot.iter().enumerate().skip(1) {
                let at = 64 + 32 * index;
                entry(&mut catalog[at..at + 32], boot, data[index]);
            }
        }
    }
}

fn split_records(bytes: &[u8]) -> Vec<&[u8]> {
    let mut pieces = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let length = rest[0] as usize;
        pieces.push(&rest[..length]);
        rest = &rest[length..];
    }
    pieces
}

fn packed_size(records: &[IsoRecord]) -> usize {
    let mut offset = 0usize;
    for record in records {
        let fake = record_bytes(
            &IsoRecord {
                id: record.id.clone(),
                flags: record.flags,
                sectors: record.sectors.clone(),
                system_use: record.system_use.clone(),
                continuation: record.continuation.clone(),
            },
            record.continuation.as_ref().map(|_| (0, 0)),
        );
        for piece in split_records(&fake) {
            if offset % SECTOR + piece.len() > SECTOR {
                offset = offset - offset % SECTOR + SECTOR;
            }
            offset += piece.len();
        }
    }
    offset.div_ceil(SECTOR).max(1) * SECTOR
}

struct UdfPlan {
    file_set: u32,
    entries: Vec<u32>,
    dir_data: Vec<Option<(u32, usize)>>,
    file_blocks: Vec<Option<u32>>,
    metadata_file: Option<u32>,
    metadata_area: u32,
    metadata_blocks: u32,
    icb_partition: u16,
}

const METADATA_BLOCKS: u32 = 64;

fn fid_length(name: &[u8]) -> usize {
    (38 + name.len()).div_ceil(4) * 4
}

fn udf_name(item: &Item) -> Vec<u8> {
    item.udf_name.clone().unwrap_or_else(|| {
        let name = base(&item.path);
        if name.chars().all(|c| (c as u32) < 0x100) {
            let mut out = vec![8];
            out.extend(name.chars().map(|c| c as u8));
            out
        } else {
            let mut out = vec![16];
            out.extend(name.encode_utf16().flat_map(|unit| unit.to_be_bytes()));
            out
        }
    })
}

fn dir_data_length(tree: &Tree, index: usize) -> usize {
    40 + tree.nodes[index]
        .children
        .iter()
        .map(|child| fid_length(&udf_name(&tree.nodes[*child].item)))
        .sum::<usize>()
}

fn entry_header(udf: &Udf) -> usize {
    if udf.extended { 216 } else { 176 }
}

impl UdfPlan {
    fn allocate(udf: &Udf, tree: &Tree, writer: &mut Writer, partition_start: u32) -> UdfPlan {
        let physical = |writer: &mut Writer, length: usize| writer.alloc(length) - partition_start;
        let metadata_file = udf.metadata.then(|| physical(writer, SECTOR));
        let (metadata_area, mut metadata_next) = if udf.metadata {
            let start = physical(writer, METADATA_BLOCKS as usize * SECTOR);
            (start, 0u32)
        } else {
            (0, 0)
        };
        let mut icb_block = |writer: &mut Writer, length: usize| -> u32 {
            if udf.metadata {
                let block = metadata_next;
                metadata_next += length.div_ceil(SECTOR).max(1) as u32;
                assert!(metadata_next <= METADATA_BLOCKS, "metadata area is full");
                block
            } else {
                writer.alloc(length) - partition_start
            }
        };
        let file_set = icb_block(writer, SECTOR);
        let mut entries = Vec::new();
        let mut dir_data = Vec::new();
        for index in 0..tree.nodes.len() {
            entries.push(icb_block(writer, SECTOR));
            if tree.is_dir(index) {
                let length = dir_data_length(tree, index);
                if udf.inline_dirs && length + entry_header(udf) <= SECTOR {
                    dir_data.push(None);
                } else {
                    dir_data.push(Some((icb_block(writer, length), length)));
                }
            } else {
                dir_data.push(None);
            }
        }
        UdfPlan {
            file_set,
            entries,
            dir_data,
            file_blocks: vec![None; tree.nodes.len()],
            metadata_file,
            metadata_area,
            metadata_blocks: METADATA_BLOCKS,
            icb_partition: u16::from(udf.metadata),
        }
    }

    fn icb_at(&self, layout: &Layout, block: u32) -> usize {
        let start = if self.icb_partition == 1 {
            layout.partition_start + self.metadata_area
        } else {
            layout.partition_start
        };
        Writer::at(start + block)
    }

    fn write(
        mut self,
        udf: &Udf,
        version: u16,
        tree: &Tree,
        file_sectors: &[Vec<(u32, u32)>],
        writer: &mut Writer,
        layout: &mut Layout,
    ) {
        let start = layout.partition_start;
        layout.metadata_start = start + self.metadata_area;
        for (index, sectors) in file_sectors.iter().enumerate() {
            if let Some((sector, _)) = sectors.first() {
                self.file_blocks[index] = Some(sector.saturating_sub(start));
            }
        }
        let total = writer.next;
        let partition_length = total - start + 16;
        writer.ensure(total + 17);
        writer.next = total + 17;

        let anchor = Writer::at(ANCHOR_SECTOR);
        let vds = ANCHOR_SECTOR + 1;
        writer.bytes[anchor + 16..anchor + 20].copy_from_slice(&(4 * SECTOR as u32).to_le_bytes());
        writer.bytes[anchor + 20..anchor + 24].copy_from_slice(&vds.to_le_bytes());
        writer.bytes[anchor + 24..anchor + 28].copy_from_slice(&(4 * SECTOR as u32).to_le_bytes());
        writer.bytes[anchor + 28..anchor + 32].copy_from_slice(&vds.to_le_bytes());
        put_tag(&mut writer.bytes, anchor, 2, version, ANCHOR_SECTOR, 512);
        layout.anchor = Some(anchor);

        let suffix = udf.revision.to_le_bytes();
        let pvd = Writer::at(vds);
        put_tag(&mut writer.bytes, pvd, 1, version, vds, 512);

        let pd = Writer::at(vds + 1);
        writer.bytes[pd + 20..pd + 22].copy_from_slice(&1u16.to_le_bytes());
        entity(
            &mut writer.bytes,
            pd + 24,
            if version == 3 { b"+NSR03" } else { b"+NSR02" },
            &[],
        );
        writer.bytes[pd + 184..pd + 188].copy_from_slice(&1u32.to_le_bytes());
        writer.bytes[pd + 188..pd + 192].copy_from_slice(&start.to_le_bytes());
        writer.bytes[pd + 192..pd + 196].copy_from_slice(&partition_length.to_le_bytes());
        put_tag(&mut writer.bytes, pd, 5, version, vds + 1, 512);
        layout.partition_descriptor = Some(pd);

        let lvd = Writer::at(vds + 2);
        writer.bytes[lvd + 212..lvd + 216].copy_from_slice(&(SECTOR as u32).to_le_bytes());
        entity(
            &mut writer.bytes,
            lvd + 216,
            b"*OSTA UDF Compliant",
            &suffix,
        );
        writer.bytes[lvd + 248..lvd + 252].copy_from_slice(&(SECTOR as u32).to_le_bytes());
        writer.bytes[lvd + 252..lvd + 256].copy_from_slice(&self.file_set.to_le_bytes());
        writer.bytes[lvd + 256..lvd + 258].copy_from_slice(&self.icb_partition.to_le_bytes());
        let mut maps = vec![1u8, 6, 1, 0, 0, 0];
        if let Some(metadata_file) = self.metadata_file {
            let mut map = vec![0u8; 64];
            map[0] = 2;
            map[1] = 64;
            map[5..28].copy_from_slice(b"*UDF Metadata Partition");
            map[28..30].copy_from_slice(&suffix);
            map[36..38].copy_from_slice(&1u16.to_le_bytes());
            map[40..44].copy_from_slice(&metadata_file.to_le_bytes());
            map[44..48].copy_from_slice(&metadata_file.to_le_bytes());
            map[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
            map[52..56].copy_from_slice(&32u32.to_le_bytes());
            map[56..58].copy_from_slice(&1u16.to_le_bytes());
            maps.extend_from_slice(&map);
        }
        let map_count = if self.metadata_file.is_some() {
            2u32
        } else {
            1
        };
        writer.bytes[lvd + 264..lvd + 268].copy_from_slice(&(maps.len() as u32).to_le_bytes());
        writer.bytes[lvd + 268..lvd + 272].copy_from_slice(&map_count.to_le_bytes());
        writer.bytes[lvd + 440..lvd + 440 + maps.len()].copy_from_slice(&maps);
        put_tag(
            &mut writer.bytes,
            lvd,
            6,
            version,
            vds + 2,
            440 + maps.len(),
        );
        layout.logical_volume = Some(lvd);

        let terminator = Writer::at(vds + 3);
        put_tag(&mut writer.bytes, terminator, 8, version, vds + 3, 512);
        let end_anchor = Writer::at(total + 16);
        let (main, reserve) = (anchor + 16, end_anchor + 16);
        let extents = writer.bytes[main..main + 16].to_vec();
        writer.bytes[reserve..reserve + 16].copy_from_slice(&extents);
        put_tag(&mut writer.bytes, end_anchor, 2, version, total + 16, 512);

        if let Some(metadata_file) = self.metadata_file {
            let at = Writer::at(start + metadata_file);
            let header = entry_header(udf);
            let b = &mut writer.bytes;
            b[at + 20..at + 22].copy_from_slice(&4u16.to_le_bytes());
            b[at + 27] = 250;
            b[at + 56..at + 64]
                .copy_from_slice(&(u64::from(self.metadata_blocks) * SECTOR as u64).to_le_bytes());
            b[at + header - 4..at + header].copy_from_slice(&8u32.to_le_bytes());
            b[at + header..at + header + 4]
                .copy_from_slice(&(self.metadata_blocks * SECTOR as u32).to_le_bytes());
            b[at + header + 4..at + header + 8].copy_from_slice(&self.metadata_area.to_le_bytes());
            put_tag(
                b,
                at,
                if udf.extended { 266 } else { 261 },
                version,
                metadata_file,
                header + 8,
            );
            layout.metadata_file = Some(at);
        }

        let fsd = self.icb_at(layout, self.file_set);
        {
            let b = &mut writer.bytes;
            b[fsd + 16..fsd + 28].copy_from_slice(&udf_time());
            b[fsd + 28..fsd + 30].copy_from_slice(&3u16.to_le_bytes());
            b[fsd + 400..fsd + 404].copy_from_slice(&(SECTOR as u32).to_le_bytes());
            b[fsd + 404..fsd + 408].copy_from_slice(&self.entries[0].to_le_bytes());
            b[fsd + 408..fsd + 410].copy_from_slice(&self.icb_partition.to_le_bytes());
            entity(b, fsd + 416, b"*OSTA UDF Compliant", &suffix);
            put_tag(b, fsd, 256, version, self.file_set, 512);
        }
        layout.file_set = Some(fsd);

        let mut parents = vec![0usize; tree.nodes.len()];
        for (index, node) in tree.nodes.iter().enumerate() {
            for child in &node.children {
                parents[*child] = index;
            }
        }
        for (index, parent) in parents.iter().enumerate() {
            self.write_entry(
                udf,
                version,
                tree,
                index,
                *parent,
                file_sectors,
                writer,
                layout,
            );
        }
    }

    fn long_ad(&self, length: u32, block: u32, partition: u16) -> [u8; 16] {
        let mut ad = [0u8; 16];
        ad[0..4].copy_from_slice(&length.to_le_bytes());
        ad[4..8].copy_from_slice(&block.to_le_bytes());
        ad[8..10].copy_from_slice(&partition.to_le_bytes());
        ad
    }

    #[allow(clippy::too_many_arguments)]
    fn write_entry(
        &self,
        udf: &Udf,
        version: u16,
        tree: &Tree,
        index: usize,
        parent: usize,
        file_sectors: &[Vec<(u32, u32)>],
        writer: &mut Writer,
        layout: &mut Layout,
    ) {
        let item = &tree.nodes[index].item;
        let header = entry_header(udf);
        let (file_type, data, extents_partition): (u8, Option<Vec<u8>>, u16) = match &item.kind {
            Kind::Dir => {
                let mut data = Vec::new();
                let mut fid = vec![0u8; 40];
                fid[16..18].copy_from_slice(&1u16.to_le_bytes());
                fid[18] = 0x0A;
                fid[20..36].copy_from_slice(&self.long_ad(
                    SECTOR as u32,
                    self.entries[parent],
                    self.icb_partition,
                ));
                data.extend_from_slice(&fid);
                let mut fid_offsets = Vec::new();
                for child in &tree.nodes[index].children {
                    let child_item = &tree.nodes[*child].item;
                    let name = udf_name(child_item);
                    let length = fid_length(&name);
                    let mut fid = vec![0u8; length];
                    fid[16..18].copy_from_slice(&1u16.to_le_bytes());
                    fid[18] = if child_item.kind == Kind::Dir { 2 } else { 0 };
                    fid[19] = name.len() as u8;
                    fid[20..36].copy_from_slice(&self.long_ad(
                        SECTOR as u32,
                        self.entries[*child],
                        self.icb_partition,
                    ));
                    fid[38..38 + name.len()].copy_from_slice(&name);
                    fid_offsets.push((child_item.path.clone(), data.len(), length));
                    data.extend_from_slice(&fid);
                }
                let dir_block = self.dir_data[index];
                let first_fid_block = dir_block.map_or(self.entries[index], |(block, _)| block);
                let mut fixed = vec![(String::new(), 0usize, 40usize)];
                fixed.extend(fid_offsets);
                for (path, offset, length) in fixed
                    .iter()
                    .map(|(path, offset, length)| (path, *offset, *length))
                {
                    let block = first_fid_block + (offset / SECTOR) as u32;
                    data[offset..offset + 2].copy_from_slice(&257u16.to_le_bytes());
                    data[offset + 2..offset + 4].copy_from_slice(&version.to_le_bytes());
                    data[offset + 12..offset + 16].copy_from_slice(&block.to_le_bytes());
                    retag(&mut data, offset, length);
                    if !path.is_empty() {
                        let base_at = match dir_block {
                            Some((block, _)) => self.icb_at(layout, block),
                            None => self.icb_at(layout, self.entries[index]) + header,
                        };
                        layout.udf_fids.insert(path.clone(), base_at + offset);
                    }
                }
                (4, Some(data), self.icb_partition)
            }
            Kind::File(data) => (5, Some(data.clone()), 0),
            Kind::Symlink(target) => {
                let mut components = Vec::new();
                for part in target.split('/').filter(|part| !part.is_empty()) {
                    match part {
                        ".." => components.extend_from_slice(&[3, 0, 0, 0]),
                        "." => components.extend_from_slice(&[4, 0, 0, 0]),
                        _ => {
                            components.extend_from_slice(&[5, (part.len() + 1) as u8, 0, 0, 8]);
                            components.extend_from_slice(part.as_bytes());
                        }
                    }
                }
                (12, Some(components), 0)
            }
        };
        let data = data.unwrap();
        let at = self.icb_at(layout, self.entries[index]);
        let inline = match &item.kind {
            Kind::Dir => self.dir_data[index].is_none(),
            Kind::Symlink(_) => true,
            Kind::File(bytes) => udf.ads == Ads::Inline && header + bytes.len() <= SECTOR,
        };
        let mut ads = Vec::new();
        let flags: u16;
        if inline {
            flags = 3;
            ads.extend_from_slice(&data);
        } else {
            let pieces: Vec<(u32, u32)> = match &item.kind {
                Kind::Dir => {
                    let (block, length) = self.dir_data[index].unwrap();
                    let offset = self.icb_at(layout, block);
                    writer.bytes[offset..offset + data.len()].copy_from_slice(&data);
                    layout
                        .udf_dir_data
                        .insert(item.path.clone(), (offset, data.len()));
                    vec![(block, length as u32)]
                }
                _ => file_sectors[index]
                    .iter()
                    .filter(|(_, length)| *length > 0)
                    .map(|(sector, length)| (sector - layout.partition_start, *length))
                    .collect(),
            };
            let long = udf.ads == Ads::Long || (udf.metadata && extents_partition == 0);
            flags = if long { 1 } else { 0 };
            for (block, length) in pieces {
                if long {
                    ads.extend_from_slice(&self.long_ad(length, block, extents_partition));
                } else {
                    ads.extend_from_slice(&length.to_le_bytes());
                    ads.extend_from_slice(&block.to_le_bytes());
                }
            }
        }
        let b = &mut writer.bytes;
        b[at + 20..at + 22].copy_from_slice(&4u16.to_le_bytes());
        b[at + 27] = file_type;
        b[at + 34..at + 36].copy_from_slice(&flags.to_le_bytes());
        b[at + 44..at + 48].copy_from_slice(&0x14A5u32.to_le_bytes());
        b[at + 48..at + 50].copy_from_slice(&1u16.to_le_bytes());
        b[at + 56..at + 64].copy_from_slice(&(data.len() as u64).to_le_bytes());
        let recorded = if inline {
            0
        } else {
            data.len().div_ceil(SECTOR) as u64
        };
        let times = if udf.extended {
            b[at + 64..at + 72].copy_from_slice(&(data.len() as u64).to_le_bytes());
            b[at + 72..at + 80].copy_from_slice(&recorded.to_le_bytes());
            [80, 92, 104, 116]
        } else {
            b[at + 64..at + 72].copy_from_slice(&recorded.to_le_bytes());
            [72, 84, 96, 96]
        };
        for time in times {
            b[at + time..at + time + 12].copy_from_slice(&udf_time());
        }
        b[at + header - 4..at + header].copy_from_slice(&(ads.len() as u32).to_le_bytes());
        b[at + header..at + header + ads.len()].copy_from_slice(&ads);
        put_tag(
            b,
            at,
            if udf.extended { 266 } else { 261 },
            version,
            self.entries[index],
            header + ads.len(),
        );
        layout.udf_entries.insert(item.path.clone(), at);
    }
}
