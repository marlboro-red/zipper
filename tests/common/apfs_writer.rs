#![allow(dead_code)]
//! Writes APFS containers for tests: a checkpoint area, the container and
//! volume object maps, multi-level B-trees and the file-system records of
//! each volume, laid out as the APFS Reference describes.
use std::collections::BTreeMap;

pub const ROOT_PARENT: u64 = 1;
pub const ROOT: u64 = 2;
pub const PRIVATE: u64 = 3;
pub const FIRST_USER_ID: u64 = 16;
pub const REGULAR: u16 = 0o100_000;
pub const DIRECTORY: u16 = 0o040_000;
pub const SYMLINK: u16 = 0o120_000;
pub const NANOS_2026: u64 = 1_780_000_000_000_000_000;
pub const XID: u64 = 20;
pub const HAS_UNCOMPRESSED_SIZE: u64 = 0x0004_0000;

const OBJ_PHYSICAL: u32 = 0x4000_0000;
const OBJ_EPHEMERAL: u32 = 0x8000_0000;
const NX_SUPERBLOCK: u32 = 1;
const BTREE: u32 = 2;
const BTREE_NODE: u32 = 3;
const SPACEMAN: u32 = 5;
const OMAP: u32 = 0xB;
const CHECKPOINT_MAP: u32 = 0xC;
const FS: u32 = 0xD;
const FSTREE: u32 = 0xE;

#[derive(Clone, Debug)]
pub enum Kind {
    Folder,
    File(Vec<u8>),
    Sparse {
        size: u64,
        pieces: Vec<(u64, Vec<u8>)>,
    },
    Symlink(String),
    HardLink(u64),
    Compressed {
        decmpfs: Vec<u8>,
        size: u64,
        resource: Option<Vec<u8>>,
    },
    Clone {
        of: u64,
        size: u64,
    },
    Fifo,
}

#[derive(Clone, Debug)]
pub struct Xattr {
    pub name: String,
    pub value: Vec<u8>,
    pub stream: bool,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub kind: Kind,
    pub mode: u16,
    pub modified: u64,
    pub xattrs: Vec<Xattr>,
    pub fragments: usize,
}

fn entry(id: u64, parent: u64, name: &str, kind: Kind, mode: u16) -> Entry {
    Entry {
        id,
        parent,
        name: name.to_owned(),
        kind,
        mode,
        modified: NANOS_2026 + id * 1_000_000_007,
        xattrs: Vec::new(),
        fragments: 1,
    }
}

pub fn folder(id: u64, parent: u64, name: &str) -> Entry {
    entry(id, parent, name, Kind::Folder, DIRECTORY | 0o755)
}

pub fn file(id: u64, parent: u64, name: &str, data: &[u8]) -> Entry {
    entry(id, parent, name, Kind::File(data.to_vec()), REGULAR | 0o644)
}

pub fn symlink(id: u64, parent: u64, name: &str, target: &str) -> Entry {
    entry(
        id,
        parent,
        name,
        Kind::Symlink(target.to_owned()),
        SYMLINK | 0o755,
    )
}

pub fn hard_link(id: u64, parent: u64, name: &str, inode: u64) -> Entry {
    entry(id, parent, name, Kind::HardLink(inode), REGULAR | 0o644)
}

pub fn sparse(id: u64, parent: u64, name: &str, size: u64, pieces: Vec<(u64, Vec<u8>)>) -> Entry {
    entry(
        id,
        parent,
        name,
        Kind::Sparse { size, pieces },
        REGULAR | 0o644,
    )
}

pub fn compressed(
    id: u64,
    parent: u64,
    name: &str,
    decmpfs: Vec<u8>,
    size: u64,
    resource: Option<Vec<u8>>,
) -> Entry {
    entry(
        id,
        parent,
        name,
        Kind::Compressed {
            decmpfs,
            size,
            resource,
        },
        REGULAR | 0o644,
    )
}

pub fn decmpfs_header(method: u32, size: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"fpmc".to_vec();
    bytes.extend(method.to_le_bytes());
    bytes.extend(size.to_le_bytes());
    bytes.extend(payload);
    bytes
}

#[derive(Clone, Debug)]
pub struct Volume {
    pub entries: Vec<Entry>,
    pub case_insensitive: bool,
    pub normalization_insensitive: bool,
    pub encrypted: bool,
    pub snapshots: u64,
    pub sealed: bool,
    pub private_entries: Vec<Entry>,
}

impl Volume {
    pub fn new(entries: Vec<Entry>) -> Volume {
        Volume {
            entries,
            case_insensitive: false,
            normalization_insensitive: true,
            encrypted: false,
            snapshots: 0,
            sealed: false,
            private_entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Container {
    pub volumes: Vec<Volume>,
    pub block_size: usize,
    pub records_per_leaf: usize,
    pub children_per_index: usize,
    pub omap_entries_per_leaf: usize,
    pub stale_block_zero: bool,
    pub fusion: bool,
    pub data_at: Option<u64>,
    pub spare_blocks: u64,
    pub max_volumes: u32,
}

impl Container {
    pub fn new(volumes: Vec<Volume>) -> Container {
        Container {
            volumes,
            block_size: 4096,
            records_per_leaf: 1000,
            children_per_index: 1000,
            omap_entries_per_leaf: 1000,
            stale_block_zero: false,
            fusion: false,
            data_at: None,
            spare_blocks: 8,
            max_volumes: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub block_size: usize,
    pub block_count: u64,
    pub descriptor_base: u64,
    pub descriptor_blocks: u64,
    pub newest_map: u64,
    pub newest_superblock: u64,
    pub older: Vec<u64>,
    pub data_base: u64,
    pub container_omap: u64,
    pub container_omap_nodes: Vec<u64>,
    pub volume_superblocks: Vec<u64>,
    pub volume_omaps: Vec<u64>,
    pub volume_omap_nodes: Vec<Vec<u64>>,
    pub file_tree_nodes: Vec<Vec<u64>>,
    pub file_tree_roots: Vec<u64>,
    pub extents: Vec<(String, u64, u64)>,
}

pub struct Image {
    pub size: u64,
    pub blocks: BTreeMap<u64, Vec<u8>>,
    pub block_size: usize,
}

impl Image {
    pub fn to_vec(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; self.size as usize];
        for (&block, data) in &self.blocks {
            let at = block as usize * self.block_size;
            bytes[at..at + data.len()].copy_from_slice(data);
        }
        bytes
    }

    pub fn write_sparse(&self, path: &std::path::Path) {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::File::create(path).unwrap();
        file.set_len(self.size).unwrap();
        for (&block, data) in &self.blocks {
            file.seek(SeekFrom::Start(block * self.block_size as u64))
                .unwrap();
            file.write_all(data).unwrap();
        }
    }
}

pub fn fletcher64(words: &[u8]) -> u64 {
    let modulus = 0xFFFF_FFFFu64;
    let (mut low, mut high) = (0u64, 0u64);
    for word in words.chunks_exact(4) {
        low = (low + u64::from(u32::from_le_bytes(word.try_into().unwrap()))) % modulus;
        high = (high + low) % modulus;
    }
    let check_low = modulus - (low + high) % modulus;
    let check_high = modulus - (low + check_low) % modulus;
    check_high << 32 | check_low
}

pub fn seal(block: &mut [u8]) {
    let sum = fletcher64(&block[8..]);
    block[..8].copy_from_slice(&sum.to_le_bytes());
}

fn put16(block: &mut [u8], at: usize, value: u16) {
    block[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(block: &mut [u8], at: usize, value: u32) {
    block[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(block: &mut [u8], at: usize, value: u64) {
    block[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn header(block: &mut [u8], oid: u64, kind: u32, subtype: u32) {
    put64(block, 8, oid);
    put64(block, 0x10, XID);
    put32(block, 0x18, kind);
    put32(block, 0x1C, subtype);
}

type Record = (Vec<u8>, Vec<u8>);
type Level = (Vec<Vec<Record>>, Vec<(u64, u64)>);

struct InodeFields {
    private: u64,
    links: u32,
    flags: u64,
    stream: Option<u64>,
    uncompressed: u64,
}

impl InodeFields {
    fn plain(id: u64) -> InodeFields {
        InodeFields {
            private: id,
            links: 0,
            flags: 0,
            stream: None,
            uncompressed: 0,
        }
    }
}

fn key_header(id: u64, kind: u64) -> Vec<u8> {
    (id | kind << 60).to_le_bytes().to_vec()
}

fn terminated(name: &str) -> Vec<u8> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn name_hash(name: &str) -> u32 {
    crc32fast::hash(name.as_bytes()) & 0x3F_FFFF
}

struct Allocator {
    next: u64,
}

impl Allocator {
    fn take(&mut self, count: u64) -> u64 {
        let start = self.next;
        self.next += count;
        start
    }
}

struct Tree {
    nodes: Vec<(u64, u64, Vec<u8>)>,
    root: u64,
    root_address: u64,
}

struct Builder<'c> {
    container: &'c Container,
    blocks: BTreeMap<u64, Vec<u8>>,
    allocator: Allocator,
    data: Allocator,
    next_oid: u64,
    layout: Layout,
}

impl Builder<'_> {
    fn block(&self) -> Vec<u8> {
        vec![0u8; self.container.block_size]
    }

    fn put_block(&mut self, address: u64, block: Vec<u8>) {
        self.blocks.insert(address, block);
    }

    fn write_data(&mut self, bytes: &[u8]) -> (u64, u64) {
        let size = self.container.block_size as u64;
        let count = (bytes.len() as u64).div_ceil(size).max(1);
        let start = self.data.take(count);
        for index in 0..count {
            let mut block = self.block();
            let from = (index * size) as usize;
            let to = (from + size as usize).min(bytes.len());
            if from < to {
                block[..to - from].copy_from_slice(&bytes[from..to]);
            }
            self.put_block(start + index, block);
        }
        (start, count * size)
    }

    fn node(
        &self,
        records: &[Record],
        level: u16,
        root: Option<(u64, u64, bool)>,
        fixed: Option<(usize, usize)>,
    ) -> Vec<u8> {
        let size = self.container.block_size;
        let mut block = self.block();
        let mut flags = 0u16;
        let mut value_end = size;
        if root.is_some() {
            flags |= 1;
            value_end -= 0x28;
        }
        if level == 0 {
            flags |= 2;
        }
        if fixed.is_some() {
            flags |= 4;
        }
        put16(&mut block, 0x20, flags);
        put16(&mut block, 0x22, level);
        put32(&mut block, 0x24, records.len() as u32);
        let entry = if fixed.is_some() { 4 } else { 8 };
        let table = (records.len() * entry).div_ceil(16) * 16;
        put16(&mut block, 0x28, 0);
        put16(&mut block, 0x2A, table as u16);
        let key_area = 0x38 + table;
        let mut key_at = 0usize;
        let mut value_at = 0usize;
        for (index, (key, value)) in records.iter().enumerate() {
            block[key_area + key_at..key_area + key_at + key.len()].copy_from_slice(key);
            value_at += value.len();
            assert!(
                key_area + key_at + key.len() <= value_end - value_at,
                "B-tree node overflows; lower records_per_leaf"
            );
            block[value_end - value_at..value_end - value_at + value.len()].copy_from_slice(value);
            let toc = 0x38 + index * entry;
            if fixed.is_some() {
                put16(&mut block, toc, key_at as u16);
                put16(&mut block, toc + 2, value_at as u16);
            } else {
                put16(&mut block, toc, key_at as u16);
                put16(&mut block, toc + 2, key.len() as u16);
                put16(&mut block, toc + 4, value_at as u16);
                put16(&mut block, toc + 6, value.len() as u16);
            }
            key_at += key.len();
            if fixed.is_none() {
                key_at = key_at.div_ceil(8) * 8;
                value_at = value_at.div_ceil(8) * 8;
            }
        }
        put16(&mut block, 0x2C, (key_area + key_at - 0x38 - table) as u16);
        if let Some((keys, nodes, physical)) = root {
            let at = size - 0x28;
            let tree_flags = if physical { 0x10 | 0x1 | 0x2 } else { 0 };
            put32(&mut block, at, tree_flags);
            put32(&mut block, at + 4, size as u32);
            let (key_size, value_size) = fixed.unwrap_or((0, 0));
            put32(&mut block, at + 8, key_size as u32);
            put32(&mut block, at + 12, value_size as u32);
            put64(&mut block, at + 0x18, keys);
            put64(&mut block, at + 0x20, nodes);
        }
        block
    }

    fn pack(&self, records: Vec<Record>, most: usize, fixed: bool) -> Vec<Vec<Record>> {
        let capacity = self.container.block_size - 0x38 - 0x28 - 16;
        let cost = |record: &Record| {
            if fixed {
                4 + record.0.len() + record.1.len()
            } else {
                8 + record.0.len().div_ceil(8) * 8 + record.1.len().div_ceil(8) * 8
            }
        };
        let mut groups: Vec<Vec<Record>> = vec![Vec::new()];
        let mut used = 0;
        for record in records {
            let need = cost(&record);
            let group = groups.last_mut().unwrap();
            if !group.is_empty() && (group.len() == most.max(1) || used + need > capacity) {
                groups.push(Vec::new());
                used = 0;
            }
            used += need;
            groups.last_mut().unwrap().push(record);
        }
        groups
    }

    // Builds a B-tree bottom up and returns its nodes as (oid, address, block) before sealing.
    fn tree(
        &mut self,
        records: Vec<Record>,
        per_leaf: usize,
        physical: bool,
        subtype: u32,
        fixed: Option<(usize, usize)>,
    ) -> Tree {
        let per_index = self.container.children_per_index.max(2);
        let total_keys = records.len() as u64;
        let mut levels: Vec<Level> = Vec::new();
        let mut groups = self.pack(records, per_leaf, fixed.is_some());
        loop {
            let mut ids = Vec::new();
            for _ in &groups {
                let address = self.allocator.take(1);
                let oid = if physical {
                    address
                } else {
                    let oid = self.next_oid;
                    self.next_oid += 1;
                    oid
                };
                ids.push((oid, address));
            }
            let parents: Vec<Record> = groups
                .iter()
                .zip(&ids)
                .map(|(group, (oid, _))| {
                    let first_key = group
                        .first()
                        .map(|record| record.0.clone())
                        .unwrap_or_else(|| vec![0u8; fixed.map_or(8, |sizes| sizes.0)]);
                    (first_key, oid.to_le_bytes().to_vec())
                })
                .collect();
            let single = groups.len() == 1;
            levels.push((groups, ids));
            if single {
                break;
            }
            groups = self.pack(parents, per_index, fixed.is_some());
        }
        let node_count: u64 = levels.iter().map(|level| level.0.len() as u64).sum();
        let height = levels.len();
        let mut nodes = Vec::new();
        for (depth, (groups, ids)) in levels.iter().enumerate() {
            for (records, &(oid, address)) in groups.iter().zip(ids) {
                let is_root = depth == height - 1;
                let root = is_root.then_some((total_keys, node_count, physical));
                let mut block = self.node(records, depth as u16, root, fixed);
                let storage = if physical { OBJ_PHYSICAL } else { 0 };
                let kind = if is_root { BTREE } else { BTREE_NODE };
                header(&mut block, oid, storage | kind, subtype);
                nodes.push((oid, address, block));
            }
        }
        let (root, root_address) = levels[height - 1].1[0];
        Tree {
            nodes,
            root,
            root_address,
        }
    }

    fn omap(&mut self, mut entries: Vec<(u64, u64)>) -> (u64, Vec<u64>) {
        entries.sort();
        let records = entries
            .iter()
            .map(|&(oid, address)| {
                let mut key = oid.to_le_bytes().to_vec();
                key.extend(XID.to_le_bytes());
                let mut value = 0u32.to_le_bytes().to_vec();
                value.extend((self.container.block_size as u32).to_le_bytes());
                value.extend(address.to_le_bytes());
                (key, value)
            })
            .collect();
        let per_leaf = self.container.omap_entries_per_leaf;
        let omap_address = self.allocator.take(1);
        let tree = self.tree(records, per_leaf, true, OMAP, Some((16, 16)));
        let mut block = self.block();
        header(&mut block, omap_address, OBJ_PHYSICAL | OMAP, 0);
        put32(&mut block, 0x28, OBJ_PHYSICAL | BTREE);
        put32(&mut block, 0x2C, OBJ_PHYSICAL | BTREE);
        put64(&mut block, 0x30, tree.root_address);
        seal(&mut block);
        self.put_block(omap_address, block);
        let mut addresses = Vec::new();
        for (_, address, mut node) in tree.nodes {
            seal(&mut node);
            self.put_block(address, node);
            addresses.push(address);
        }
        (omap_address, addresses)
    }

    fn inode_value(&self, entry: &Entry, fields: InodeFields) -> Vec<u8> {
        let InodeFields {
            private,
            links,
            flags,
            stream,
            uncompressed,
        } = fields;
        let parent = entry.parent;
        let mut value = vec![0u8; 0x5C];
        put64(&mut value, 0, parent);
        put64(&mut value, 8, private);
        for at in [0x10, 0x18, 0x20, 0x28] {
            put64(&mut value, at, entry.modified);
        }
        put64(&mut value, 0x30, flags);
        put32(&mut value, 0x38, links);
        put32(&mut value, 0x44, if uncompressed != 0 { 0x20 } else { 0 });
        put32(&mut value, 0x48, 501);
        put32(&mut value, 0x4C, 20);
        put16(&mut value, 0x50, entry.mode);
        put64(&mut value, 0x54, uncompressed);
        let mut fields: Vec<(u8, u8, Vec<u8>)> = vec![(4, 2, terminated(&entry.name))];
        if let Some(size) = stream {
            let mut dstream = vec![0u8; 40];
            put64(&mut dstream, 0, size);
            put64(
                &mut dstream,
                8,
                size.div_ceil(self.container.block_size as u64) * self.container.block_size as u64,
            );
            put64(&mut dstream, 0x18, size);
            fields.push((8, 0x20, dstream));
        }
        value.extend((fields.len() as u16).to_le_bytes());
        let used: usize = fields
            .iter()
            .map(|field| field.2.len().div_ceil(8) * 8)
            .sum();
        value.extend((used as u16).to_le_bytes());
        for (kind, flags, data) in &fields {
            value.push(*kind);
            value.push(*flags);
            value.extend((data.len() as u16).to_le_bytes());
        }
        for (_, _, data) in &fields {
            let mut padded = data.clone();
            padded.resize(data.len().div_ceil(8) * 8, 0);
            value.extend(padded);
        }
        value
    }

    fn extent_records(
        &mut self,
        owner: u64,
        pieces: &[(u64, u64, u64)],
        path: &str,
    ) -> Vec<Record> {
        pieces
            .iter()
            .map(|&(logical, length, block)| {
                if block != 0 {
                    self.layout.extents.push((
                        path.to_owned(),
                        block,
                        length / self.container.block_size as u64,
                    ));
                }
                let mut key = key_header(owner, 8);
                key.extend(logical.to_le_bytes());
                let mut value = length.to_le_bytes().to_vec();
                value.extend(block.to_le_bytes());
                value.extend(0u64.to_le_bytes());
                (key, value)
            })
            .collect()
    }

    fn stored(&mut self, data: &[u8], fragments: usize) -> Vec<(u64, u64, u64)> {
        let size = self.container.block_size;
        if data.is_empty() {
            return Vec::new();
        }
        let blocks = data.len().div_ceil(size);
        let fragments = fragments.clamp(1, blocks);
        let per = blocks.div_ceil(fragments);
        let mut pieces = Vec::new();
        let mut logical = 0usize;
        for chunk in data.chunks(per * size) {
            let (start, length) = self.write_data(chunk);
            self.data.take(1);
            pieces.push((logical as u64, length, start));
            logical += length as usize;
        }
        pieces
    }

    fn volume_records(&mut self, volume: &Volume) -> Vec<Record> {
        let hashed = volume.case_insensitive || volume.normalization_insensitive;
        let mut records: Vec<Record> = Vec::new();
        let drec = |parent: u64, name: &str, file: u64, kind: u16, records: &mut Vec<Record>| {
            let mut key = key_header(parent, 9);
            let bytes = terminated(name);
            if hashed {
                key.extend((bytes.len() as u32 | name_hash(name) << 10).to_le_bytes());
            } else {
                key.extend((bytes.len() as u16).to_le_bytes());
            }
            key.extend(bytes);
            let mut value = file.to_le_bytes().to_vec();
            value.extend(NANOS_2026.to_le_bytes());
            value.extend(kind.to_le_bytes());
            records.push((key, value));
        };
        drec(ROOT_PARENT, "root", ROOT, 4, &mut records);
        drec(ROOT_PARENT, "private-dir", PRIVATE, 4, &mut records);
        let system = |id: u64, parent: u64, name: &str| Entry {
            id,
            parent,
            name: name.to_owned(),
            kind: Kind::Folder,
            mode: DIRECTORY | 0o755,
            modified: NANOS_2026,
            xattrs: Vec::new(),
            fragments: 1,
        };
        let root = system(ROOT, ROOT_PARENT, "root");
        let private = system(PRIVATE, ROOT_PARENT, "private-dir");
        let value = self.inode_value(&root, InodeFields::plain(ROOT));
        records.push((key_header(ROOT, 3), value));
        let value = self.inode_value(&private, InodeFields::plain(PRIVATE));
        records.push((key_header(PRIVATE, 3), value));
        let mut links: BTreeMap<u64, Vec<&Entry>> = BTreeMap::new();
        let all: Vec<&Entry> = volume
            .entries
            .iter()
            .chain(volume.private_entries.iter())
            .collect();
        for entry in &all {
            if let Kind::HardLink(inode) = entry.kind {
                links.entry(inode).or_default().push(entry);
            }
        }
        for entry in &all {
            let kind = match &entry.kind {
                Kind::Folder => 4,
                Kind::Symlink(_) => 10,
                Kind::Fifo => 1,
                _ => 8,
            };
            let file = match entry.kind {
                Kind::HardLink(inode) => inode,
                _ => entry.id,
            };
            drec(entry.parent, &entry.name, file, kind, &mut records);
            if matches!(entry.kind, Kind::HardLink(_)) {
                continue;
            }
            let siblings = links.get(&entry.id).map_or(0, |list| list.len());
            let path = entry.name.clone();
            let (stream, flags, uncompressed, private) = match &entry.kind {
                Kind::File(data) => {
                    let pieces = self.stored(data, entry.fragments);
                    records.extend(self.extent_records(entry.id, &pieces, &path));
                    (Some(data.len() as u64), 0, 0, entry.id)
                }
                Kind::Sparse { size, pieces } => {
                    let block = self.container.block_size as u64;
                    let mut extents = Vec::new();
                    let mut at = 0u64;
                    for (offset, data) in pieces {
                        assert!(offset % block == 0 && *offset >= at);
                        if *offset > at {
                            extents.push((at, offset - at, 0));
                        }
                        let (start, length) = self.write_data(data);
                        extents.push((*offset, length, start));
                        at = offset + length;
                    }
                    let end = size.div_ceil(block) * block;
                    if end > at {
                        extents.push((at, end - at, 0));
                    }
                    records.extend(self.extent_records(entry.id, &extents, &path));
                    (Some(*size), 0x200, 0, entry.id)
                }
                Kind::Clone { of, size } => (Some(*size), 0x10, 0, *of),
                Kind::Compressed { size, .. } => (None, HAS_UNCOMPRESSED_SIZE, *size, entry.id),
                _ => (None, 0, 0, entry.id),
            };
            let link_count = match entry.kind {
                Kind::Folder => all.iter().filter(|other| other.parent == entry.id).count() as u32,
                _ => 1 + siblings as u32,
            };
            let value = self.inode_value(
                entry,
                InodeFields {
                    private,
                    links: link_count,
                    flags,
                    stream,
                    uncompressed,
                },
            );
            records.push((key_header(entry.id, 3), value));
            if stream.is_some() && !matches!(entry.kind, Kind::Clone { .. }) {
                records.push((key_header(entry.id, 6), 1u32.to_le_bytes().to_vec()));
            }
            let mut xattrs = entry.xattrs.clone();
            if let Kind::Symlink(target) = &entry.kind {
                xattrs.push(Xattr {
                    name: "com.apple.fs.symlink".into(),
                    value: terminated(target),
                    stream: false,
                });
            }
            if let Kind::Compressed {
                decmpfs, resource, ..
            } = &entry.kind
            {
                xattrs.push(Xattr {
                    name: "com.apple.decmpfs".into(),
                    value: decmpfs.clone(),
                    stream: decmpfs.len() > 3800,
                });
                if let Some(resource) = resource {
                    xattrs.push(Xattr {
                        name: "com.apple.ResourceFork".into(),
                        value: resource.clone(),
                        stream: true,
                    });
                }
            }
            for xattr in &xattrs {
                let mut key = key_header(entry.id, 4);
                let name = terminated(&xattr.name);
                key.extend((name.len() as u16).to_le_bytes());
                key.extend(name);
                let mut value = Vec::new();
                if xattr.stream {
                    let stream_id = self.next_oid;
                    self.next_oid += 1;
                    value.extend(1u16.to_le_bytes());
                    value.extend(48u16.to_le_bytes());
                    value.extend(stream_id.to_le_bytes());
                    let mut dstream = vec![0u8; 40];
                    put64(&mut dstream, 0, xattr.value.len() as u64);
                    value.extend(dstream);
                    let pieces = self.stored(&xattr.value, 1);
                    let extents = self.extent_records(stream_id, &pieces, &path);
                    records.extend(extents);
                } else {
                    value.extend(2u16.to_le_bytes());
                    value.extend((xattr.value.len() as u16).to_le_bytes());
                    value.extend(&xattr.value);
                }
                records.push((key, value));
            }
            if let Some(names) = links.get(&entry.id) {
                for (index, link) in std::iter::once(*entry)
                    .chain(names.iter().copied())
                    .enumerate()
                {
                    let sibling = entry.id * 1000 + index as u64 + 1_000_000;
                    let mut key = key_header(entry.id, 5);
                    key.extend(sibling.to_le_bytes());
                    let mut value = link.parent.to_le_bytes().to_vec();
                    let name = terminated(&link.name);
                    value.extend((name.len() as u16).to_le_bytes());
                    value.extend(name);
                    records.push((key, value));
                    records.push((key_header(sibling, 12), entry.id.to_le_bytes().to_vec()));
                }
            }
        }
        records.sort_by_key(|record| sort_key(&record.0, hashed));
        records
    }
}

pub fn build(container: &Container) -> (Image, Layout) {
    let block_size = container.block_size;
    let descriptor_blocks = 8u64;
    let data_blocks = 8u64;
    let mut builder = Builder {
        container,
        blocks: BTreeMap::new(),
        allocator: Allocator {
            next: 1 + descriptor_blocks + data_blocks,
        },
        data: Allocator { next: 0 },
        next_oid: 1 << 40,
        layout: Layout {
            block_size,
            descriptor_base: 1,
            descriptor_blocks,
            data_base: 1 + descriptor_blocks,
            ..Layout::default()
        },
    };
    let volume_oids: Vec<u64> = (0..container.volumes.len() as u64)
        .map(|index| 1026 + index)
        .collect();
    let mut all_records = Vec::new();
    builder.data.next = u64::MAX / 2;
    for volume in &container.volumes {
        let records = builder.volume_records(volume);
        all_records.push(records);
    }
    let data_records: BTreeMap<u64, Vec<u8>> = std::mem::take(&mut builder.blocks);
    let mut volume_entries = Vec::new();
    for (index, volume) in container.volumes.iter().enumerate() {
        let records = std::mem::take(&mut all_records[index]);
        let tree = builder.tree(records, container.records_per_leaf, false, FSTREE, None);
        let mut fs_nodes = Vec::new();
        let mut omap_entries = Vec::new();
        for (oid, address, mut node) in tree.nodes {
            seal(&mut node);
            builder.put_block(address, node);
            omap_entries.push((oid, address));
            fs_nodes.push(address);
        }
        let (omap_address, omap_nodes) = builder.omap(omap_entries);
        let superblock_address = builder.allocator.take(1);
        let mut block = builder.block();
        header(&mut block, volume_oids[index], FS, 0);
        block[0x20..0x24].copy_from_slice(b"APSB");
        put32(&mut block, 0x24, index as u32);
        let mut incompatible = 0u64;
        if volume.case_insensitive {
            incompatible |= 1;
        }
        if volume.normalization_insensitive {
            incompatible |= 8;
        }
        if volume.sealed {
            incompatible |= 0x20;
        }
        put64(&mut block, 0x38, incompatible);
        put32(&mut block, 0x74, BTREE);
        put32(&mut block, 0x78, OBJ_PHYSICAL | BTREE);
        put32(&mut block, 0x7C, OBJ_PHYSICAL | BTREE);
        put64(&mut block, 0x80, omap_address);
        put64(&mut block, 0x88, tree.root);
        put64(&mut block, 0xB0, builder.next_oid);
        put64(&mut block, 0xB8, volume.entries.len() as u64);
        put64(&mut block, 0xD8, volume.snapshots);
        put64(&mut block, 0x108, if volume.encrypted { 0 } else { 1 });
        block[0x2C0..0x2C6].copy_from_slice(b"Writer");
        if volume.sealed {
            put64(&mut block, 0x400, builder.next_oid);
        }
        seal(&mut block);
        builder.put_block(superblock_address, block);
        builder.layout.volume_superblocks.push(superblock_address);
        builder.layout.volume_omaps.push(omap_address);
        builder.layout.volume_omap_nodes.push(omap_nodes);
        builder.layout.file_tree_nodes.push(fs_nodes);
        builder.layout.file_tree_roots.push(tree.root_address);
        volume_entries.push((volume_oids[index], superblock_address));
    }
    let (container_omap, container_nodes) = builder.omap(volume_entries);
    builder.layout.container_omap = container_omap;
    builder.layout.container_omap_nodes = container_nodes;
    let metadata_end = builder.allocator.next;
    let data_start = container.data_at.unwrap_or(metadata_end).max(metadata_end);
    let shift = data_start.wrapping_sub(u64::MAX / 2);
    for (block, data) in data_records {
        builder.blocks.insert(block.wrapping_add(shift), data);
    }
    let data_end = builder.data.next.wrapping_add(shift).max(data_start);
    let block_count = data_end + container.spare_blocks;
    builder.layout.block_count = block_count;
    for extent in &mut builder.layout.extents {
        extent.1 = extent.1.wrapping_add(shift);
    }
    rewrite_extents(&mut builder, shift);

    let superblock = |xid: u64, index: u32, volumes: bool| {
        let mut block = vec![0u8; block_size];
        header(&mut block, 1, OBJ_EPHEMERAL | NX_SUPERBLOCK, 0);
        put64(&mut block, 0x10, xid);
        block[0x20..0x24].copy_from_slice(b"NXSB");
        put32(&mut block, 0x24, block_size as u32);
        put64(&mut block, 0x28, block_count);
        put64(&mut block, 0x40, if container.fusion { 0x100 } else { 0 });
        block[0x48..0x58].copy_from_slice(&[0x5A; 16]);
        put64(&mut block, 0x58, 1 << 20);
        put64(&mut block, 0x60, xid + 1);
        put32(&mut block, 0x68, descriptor_blocks as u32);
        put32(&mut block, 0x6C, data_blocks as u32);
        put64(&mut block, 0x70, 1);
        put64(&mut block, 0x78, 1 + descriptor_blocks);
        put32(&mut block, 0x80, (index + 2) % descriptor_blocks as u32);
        put32(&mut block, 0x84, 1);
        put32(&mut block, 0x88, index);
        put32(&mut block, 0x8C, 2);
        put32(&mut block, 0x90, 0);
        put32(&mut block, 0x94, 1);
        put64(&mut block, 0x98, 0x400);
        put64(&mut block, 0xA0, container_omap);
        let max = if container.max_volumes != 0 {
            container.max_volumes
        } else {
            container.volumes.len().max(1) as u32
        };
        put32(&mut block, 0xB4, max);
        if volumes {
            for (number, oid) in volume_oids.iter().enumerate() {
                put64(&mut block, 0xB8 + 8 * number, *oid);
            }
        }
        seal(&mut block);
        block
    };
    let checkpoint_map = || {
        let mut block = vec![0u8; block_size];
        put32(&mut block, 0x20, 1);
        put32(&mut block, 0x24, 1);
        put32(&mut block, 0x28, OBJ_EPHEMERAL | SPACEMAN);
        put32(&mut block, 0x30, block_size as u32);
        put64(&mut block, 0x40, 0x400);
        put64(&mut block, 0x48, 1 + descriptor_blocks);
        block
    };
    let older = XID - 1;
    let mut old_map = checkpoint_map();
    header(&mut old_map, 1, OBJ_PHYSICAL | CHECKPOINT_MAP, 0);
    put64(&mut old_map, 0x10, older);
    seal(&mut old_map);
    builder.put_block(1, old_map);
    builder.put_block(2, superblock(older, 0, false));
    let mut map = checkpoint_map();
    header(&mut map, 3, OBJ_PHYSICAL | CHECKPOINT_MAP, 0);
    seal(&mut map);
    builder.put_block(3, map);
    let newest = superblock(XID, 2, true);
    builder.put_block(4, newest.clone());
    let mut spaceman = vec![0u8; block_size];
    header(&mut spaceman, 0x400, OBJ_EPHEMERAL | SPACEMAN, 0);
    seal(&mut spaceman);
    builder.put_block(1 + descriptor_blocks, spaceman);
    builder.layout.newest_map = 3;
    builder.layout.newest_superblock = 4;
    builder.layout.older = vec![1, 2];
    let zero = if container.stale_block_zero {
        superblock(older, 0, false)
    } else {
        newest
    };
    builder.put_block(0, zero);
    let image = Image {
        size: block_count * block_size as u64,
        blocks: builder.blocks,
        block_size,
    };
    (image, builder.layout)
}

fn rewrite_extents(builder: &mut Builder, shift: u64) {
    let size = builder.container.block_size;
    let file_trees: Vec<u64> = builder
        .layout
        .file_tree_nodes
        .iter()
        .flatten()
        .copied()
        .collect();
    for address in file_trees {
        let block = builder.blocks.get_mut(&address).unwrap();
        let flags = u16::from_le_bytes([block[0x20], block[0x21]]);
        if flags & 2 == 0 {
            continue;
        }
        let keys = u32::from_le_bytes(block[0x24..0x28].try_into().unwrap()) as usize;
        let table = u16::from_le_bytes([block[0x2A], block[0x2B]]) as usize;
        let value_end = if flags & 1 != 0 { size - 0x28 } else { size };
        for index in 0..keys {
            let toc = 0x38 + index * 8;
            let key = u16::from_le_bytes([block[toc], block[toc + 1]]) as usize;
            let value = u16::from_le_bytes([block[toc + 4], block[toc + 5]]) as usize;
            let key_at = 0x38 + table + key;
            let header = u64::from_le_bytes(block[key_at..key_at + 8].try_into().unwrap());
            if header >> 60 != 8 {
                continue;
            }
            let value_at = value_end - value;
            let physical =
                u64::from_le_bytes(block[value_at + 8..value_at + 16].try_into().unwrap());
            if physical != 0 {
                block[value_at + 8..value_at + 16]
                    .copy_from_slice(&physical.wrapping_add(shift).to_le_bytes());
            }
        }
        seal(block);
    }
}

// SPEC(APFS Reference, File-System Objects): records sort by object identifier, then type,
// then a hashed name's hash, a name's bytes or an offset, compared as numbers.
fn sort_key(key: &[u8], hashed: bool) -> (u64, u64, Vec<u8>) {
    let header = u64::from_le_bytes(key[..8].try_into().unwrap());
    let kind = header >> 60;
    let rest = &key[8..];
    let secondary = match kind {
        5 | 8 => u64::from_le_bytes(rest[..8].try_into().unwrap())
            .to_be_bytes()
            .to_vec(),
        9 if hashed => {
            let hash = u32::from_le_bytes(rest[..4].try_into().unwrap()) >> 10;
            [hash.to_be_bytes().to_vec(), rest[4..].to_vec()].concat()
        }
        4 | 9 => rest[2..].to_vec(),
        _ => rest.to_vec(),
    };
    (header & 0x0FFF_FFFF_FFFF_FFFF, kind, secondary)
}
