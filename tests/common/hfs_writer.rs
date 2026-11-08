#![allow(dead_code)]
use std::collections::BTreeMap;

pub const ROOT_PARENT: u32 = 1;
pub const ROOT: u32 = 2;
const EXTENTS_FILE: u32 = 3;
const CATALOG_FILE: u32 = 4;
const ATTRIBUTES_FILE: u32 = 8;
pub const FIRST_USER_ID: u32 = 16;
pub const HFS_EPOCH_2026: u32 = 3_845_000_000;
pub const REGULAR: u16 = 0o100_000;
pub const DIRECTORY: u16 = 0o040_000;
pub const SYMLINK: u16 = 0o120_000;

#[derive(Clone, Debug)]
pub enum Kind {
    Folder,
    File { data: Vec<u8>, resource: Vec<u8> },
    HardLink(u32),
    Raw { record: Vec<u8> },
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u32,
    pub parent: u32,
    pub name: Vec<u16>,
    pub kind: Kind,
    pub mode: u16,
    pub modified: u32,
    pub finder: Option<([u8; 4], [u8; 4])>,
}

pub fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

pub fn folder(id: u32, parent: u32, name: &str) -> Entry {
    Entry {
        id,
        parent,
        name: units(name),
        kind: Kind::Folder,
        mode: DIRECTORY | 0o755,
        modified: HFS_EPOCH_2026,
        finder: None,
    }
}

pub fn file(id: u32, parent: u32, name: &str, data: &[u8]) -> Entry {
    Entry {
        id,
        parent,
        name: units(name),
        kind: Kind::File {
            data: data.to_vec(),
            resource: Vec::new(),
        },
        mode: REGULAR | 0o644,
        modified: HFS_EPOCH_2026 + id,
        finder: None,
    }
}

pub fn symlink(id: u32, parent: u32, name: &str, target: &str) -> Entry {
    let mut entry = file(id, parent, name, target.as_bytes());
    entry.mode = SYMLINK | 0o755;
    entry.finder = Some((*b"slnk", *b"rhap"));
    entry
}

pub fn hard_link(id: u32, parent: u32, name: &str, inode: u32) -> Entry {
    Entry {
        id,
        parent,
        name: units(name),
        kind: Kind::HardLink(inode),
        mode: REGULAR | 0o444,
        modified: HFS_EPOCH_2026,
        finder: Some((*b"hlnk", *b"hfs+")),
    }
}

pub fn private_folder(id: u32) -> Entry {
    let mut name = vec![0u16; 4];
    name.extend("HFS+ Private Data".encode_utf16());
    Entry {
        id,
        parent: ROOT,
        name,
        kind: Kind::Folder,
        mode: DIRECTORY,
        modified: HFS_EPOCH_2026,
        finder: None,
    }
}

#[derive(Clone, Debug)]
pub struct Attribute {
    pub id: u32,
    pub name: String,
    pub value: AttributeValue,
}

#[derive(Clone, Debug)]
pub enum AttributeValue {
    Inline(Vec<u8>),
    Fork(Vec<u8>),
    Extents,
}

pub fn decmpfs_header(method: u32, size: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"fpmc".to_vec();
    bytes.extend(method.to_le_bytes());
    bytes.extend(size.to_le_bytes());
    bytes.extend(payload);
    bytes
}

pub fn lzvn_fork(chunks: &[Vec<u8>]) -> Vec<u8> {
    let table = (chunks.len() + 1) * 4;
    let mut offsets = vec![table as u32];
    for chunk in chunks {
        offsets.push(offsets.last().unwrap() + chunk.len() as u32);
    }
    let mut fork: Vec<u8> = offsets
        .iter()
        .flat_map(|offset| offset.to_le_bytes())
        .collect();
    for chunk in chunks {
        fork.extend(chunk);
    }
    fork
}

pub fn zlib_fork(chunks: &[Vec<u8>]) -> Vec<u8> {
    let table = chunks.len() * 8;
    let mut resource = Vec::new();
    resource.extend((chunks.len() as u32).to_le_bytes());
    let mut offset = 4 + table as u32;
    for chunk in chunks {
        resource.extend(offset.to_le_bytes());
        resource.extend((chunk.len() as u32).to_le_bytes());
        offset += chunk.len() as u32;
    }
    for chunk in chunks {
        resource.extend(chunk);
    }
    let data_size = 4 + resource.len() as u32;
    let map_at = 0x100 + data_size;
    let mut fork = Vec::new();
    for value in [0x100u32, map_at, data_size, 50] {
        fork.extend(value.to_be_bytes());
    }
    fork.resize(0x100, 0);
    fork.extend((resource.len() as u32).to_be_bytes());
    fork.extend(resource);
    let mut map = vec![0u8; 50];
    map[..16].copy_from_slice(&fork[..16]);
    map[24..26].copy_from_slice(&28u16.to_be_bytes());
    map[26..28].copy_from_slice(&50u16.to_be_bytes());
    map[30..34].copy_from_slice(b"cmpf");
    map[36..38].copy_from_slice(&10u16.to_be_bytes());
    map[38..40].copy_from_slice(&1u16.to_be_bytes());
    map[40..42].copy_from_slice(&0xFFFFu16.to_be_bytes());
    fork.extend(map);
    fork
}

#[derive(Clone, Debug)]
pub struct Volume {
    pub hfsx: bool,
    pub block_size: u32,
    pub node_size: u16,
    pub name: String,
    pub entries: Vec<Entry>,
    pub attributes: Vec<Attribute>,
    pub records_per_leaf: usize,
    pub fragment: bool,
    pub counts: Option<(u32, u32)>,
    pub extra_blocks: u32,
}

impl Volume {
    pub fn new(name: &str) -> Volume {
        Volume {
            hfsx: false,
            block_size: 4096,
            node_size: 4096,
            name: name.to_owned(),
            entries: Vec::new(),
            attributes: Vec::new(),
            records_per_leaf: 1000,
            fragment: false,
            counts: None,
            extra_blocks: 16,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub header: usize,
    pub extents_file: usize,
    pub catalog_file: usize,
    pub catalog_leaves: Vec<usize>,
    pub attributes_file: usize,
    pub attribute_leaves: Vec<usize>,
    pub extents_leaves: Vec<usize>,
    pub forks: BTreeMap<u32, Vec<(u32, u32)>>,
    pub node_size: usize,
    pub block_size: usize,
}

fn be16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend(value.to_be_bytes());
}

fn be32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_be_bytes());
}

fn fork_record(size: u64, extents: &[(u32, u32)]) -> Vec<u8> {
    let mut bytes = size.to_be_bytes().to_vec();
    be32(&mut bytes, 0);
    be32(&mut bytes, extents.iter().map(|extent| extent.1).sum());
    for index in 0..8 {
        let (start, count) = extents.get(index).copied().unwrap_or((0, 0));
        be32(&mut bytes, start);
        be32(&mut bytes, count);
    }
    bytes
}

fn name_key(parent: u32, name: &[u16]) -> Vec<u8> {
    let mut key = Vec::new();
    be16(&mut key, (6 + name.len() * 2) as u16);
    be32(&mut key, parent);
    be16(&mut key, name.len() as u16);
    for unit in name {
        be16(&mut key, *unit);
    }
    key
}

fn node(kind: u8, height: u8, next: u32, records: &[Vec<u8>], size: usize) -> Vec<u8> {
    let mut node = Vec::with_capacity(size);
    be32(&mut node, next);
    be32(&mut node, 0);
    node.push(kind);
    node.push(height);
    be16(&mut node, records.len() as u16);
    be16(&mut node, 0);
    let mut offsets = Vec::new();
    for record in records {
        offsets.push(node.len() as u16);
        node.extend(record);
        if node.len() % 2 == 1 {
            node.push(0);
        }
    }
    offsets.push(node.len() as u16);
    assert!(
        node.len() + offsets.len() * 2 <= size,
        "records do not fit a node"
    );
    node.resize(size - offsets.len() * 2, 0);
    for offset in offsets.iter().rev() {
        be16(&mut node, *offset);
    }
    node
}

fn header_node(
    size: usize,
    nodes: u32,
    first_leaf: u32,
    last_leaf: u32,
    leaf_records: u32,
    key: u16,
    compare: u8,
) -> Vec<u8> {
    let mut record = Vec::new();
    be16(&mut record, u16::from(first_leaf != 0));
    be32(&mut record, first_leaf);
    be32(&mut record, leaf_records);
    be32(&mut record, first_leaf);
    be32(&mut record, last_leaf);
    be16(&mut record, size as u16);
    be16(&mut record, key);
    be32(&mut record, nodes);
    be32(&mut record, 0);
    be16(&mut record, 0);
    be32(&mut record, size as u32);
    record.push(0);
    record.push(compare);
    be32(&mut record, 6);
    record.resize(106, 0);
    let user = vec![0u8; 128];
    let map_size = size - 14 - 106 - 128 - 8;
    let mut map = vec![0u8; map_size];
    for bit in 0..nodes as usize {
        if bit / 8 < map.len() {
            map[bit / 8] |= 0x80 >> (bit % 8);
        }
    }
    node(1, 0, 0, &[record, user, map], size)
}

fn header_and_leaf_chain(
    leaves: &[Vec<Vec<u8>>],
    size: usize,
    key: u16,
    compare: u8,
) -> (Vec<u8>, usize) {
    let count = leaves.len() as u32;
    let total = 1 + count;
    let records: u32 = leaves.iter().map(|leaf| leaf.len() as u32).sum();
    let mut file = header_node(
        size,
        total,
        if count == 0 { 0 } else { 1 },
        count,
        records,
        key,
        compare,
    );
    for (index, leaf) in leaves.iter().enumerate() {
        let next = if index + 1 == leaves.len() {
            0
        } else {
            index as u32 + 2
        };
        file.extend(node(0xFF, 1, next, leaf, size));
    }
    (file, total as usize)
}

fn split(records: Vec<Vec<u8>>, per_leaf: usize, size: usize) -> Vec<Vec<Vec<u8>>> {
    let mut leaves: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut used = 0usize;
    for record in records {
        let need = record.len() + record.len() % 2 + 2;
        let full = leaves
            .last()
            .is_none_or(|leaf| leaf.len() >= per_leaf || 14 + used + need + 2 > size);
        if full {
            leaves.push(Vec::new());
            used = 0;
        }
        used += need;
        leaves.last_mut().unwrap().push(record);
    }
    leaves
}

type PlacedFork = (u64, Vec<(u32, u32)>, Vec<u8>);

struct Allocator {
    next: u32,
    block_size: u32,
}

impl Allocator {
    fn take(&mut self, bytes: usize, fragment: bool) -> Vec<(u32, u32)> {
        let blocks = bytes.div_ceil(self.block_size as usize) as u32;
        if blocks == 0 {
            return Vec::new();
        }
        let extents = if fragment {
            (0..blocks)
                .map(|index| (self.next + index * 2, 1))
                .collect()
        } else {
            vec![(self.next, blocks)]
        };
        self.next += if fragment { blocks * 2 } else { blocks };
        extents
    }
}

fn put(image: &mut Vec<u8>, block_size: u32, extents: &[(u32, u32)], data: &[u8]) {
    let mut at = 0usize;
    for &(start, count) in extents {
        let offset = start as usize * block_size as usize;
        let length = (count as usize * block_size as usize).min(data.len() - at);
        if image.len() < offset + count as usize * block_size as usize {
            image.resize(offset + count as usize * block_size as usize, 0);
        }
        image[offset..offset + length].copy_from_slice(&data[at..at + length]);
        at += length;
    }
}

pub fn build(volume: &Volume) -> (Vec<u8>, Layout) {
    let block = volume.block_size;
    let node_size = usize::from(volume.node_size);
    let mut allocator = Allocator {
        next: 1,
        block_size: block,
    };
    let mut overflow: Vec<Vec<u8>> = Vec::new();
    let mut forks: BTreeMap<(u32, bool), PlacedFork> = BTreeMap::new();
    let mut layout = Layout {
        node_size,
        block_size: block as usize,
        ..Layout::default()
    };
    for entry in &volume.entries {
        if let Kind::File { data, resource } = &entry.kind {
            for (is_resource, bytes) in [(false, data), (true, resource)] {
                let extents = allocator.take(bytes.len(), volume.fragment);
                layout
                    .forks
                    .entry(entry.id)
                    .or_default()
                    .extend(extents.iter().copied());
                forks.insert(
                    (entry.id, is_resource),
                    (bytes.len() as u64, extents, bytes.clone()),
                );
            }
        }
    }
    for ((id, is_resource), (_, extents, _)) in &forks {
        if extents.len() > 8 {
            let mut start: u32 = extents[..8].iter().map(|extent| extent.1).sum();
            for group in extents[8..].chunks(8) {
                let mut record = Vec::new();
                be16(&mut record, 10);
                record.push(if *is_resource { 0xFF } else { 0 });
                record.push(0);
                be32(&mut record, *id);
                be32(&mut record, start);
                for index in 0..8 {
                    let (s, c) = group.get(index).copied().unwrap_or((0, 0));
                    be32(&mut record, s);
                    be32(&mut record, c);
                }
                start += group.iter().map(|extent| extent.1).sum::<u32>();
                overflow.push(record);
            }
        }
    }
    let mut catalog = Vec::new();
    let mut folders = 0u32;
    let mut files = 0u32;
    let mut sorted: Vec<&Entry> = volume.entries.iter().collect();
    sorted.sort_by(|a, b| (a.parent, &a.name).cmp(&(b.parent, &b.name)));
    for entry in sorted {
        let mut record = name_key(entry.parent, &entry.name);
        let mut body = Vec::new();
        let folder = matches!(entry.kind, Kind::Folder);
        match &entry.kind {
            Kind::Raw { record: raw } => {
                record.extend(raw);
                catalog.push(record);
                continue;
            }
            Kind::Folder => folders += 1,
            _ => files += 1,
        }
        be16(&mut body, if folder { 1 } else { 2 });
        be16(&mut body, 0);
        be32(&mut body, 0);
        be32(&mut body, entry.id);
        for _ in 0..5 {
            be32(&mut body, entry.modified);
        }
        be32(&mut body, 501);
        be32(&mut body, 20);
        body.push(0);
        let compressed = volume
            .attributes
            .iter()
            .any(|attribute| attribute.id == entry.id && attribute.name == "com.apple.decmpfs");
        body.push(if compressed { 0x20 } else { 0 });
        be16(&mut body, entry.mode);
        be32(
            &mut body,
            if let Kind::HardLink(inode) = entry.kind {
                inode
            } else {
                0
            },
        );
        let (kind, creator) = entry.finder.unwrap_or(([0; 4], [0; 4]));
        body.extend(kind);
        body.extend(creator);
        body.resize(88, 0);
        if !folder {
            let fork = |is_resource| {
                forks
                    .get(&(entry.id, is_resource))
                    .map_or(fork_record(0, &[]), |(size, extents, _)| {
                        fork_record(*size, extents)
                    })
            };
            body.extend(fork(false));
            body.extend(fork(true));
        }
        record.extend(body);
        catalog.push(record);
        let mut thread = name_key(entry.id, &[]);
        be16(&mut thread, if folder { 3 } else { 4 });
        be16(&mut thread, 0);
        be32(&mut thread, entry.parent);
        be16(&mut thread, entry.name.len() as u16);
        for unit in &entry.name {
            be16(&mut thread, *unit);
        }
        catalog.push(thread);
    }
    let mut attributes = Vec::new();
    for attribute in &volume.attributes {
        let name = units(&attribute.name);
        let mut record = Vec::new();
        be16(&mut record, (12 + name.len() * 2) as u16);
        be16(&mut record, 0);
        be32(&mut record, attribute.id);
        be32(&mut record, 0);
        be16(&mut record, name.len() as u16);
        for unit in &name {
            be16(&mut record, *unit);
        }
        match &attribute.value {
            AttributeValue::Inline(data) => {
                be32(&mut record, 0x10);
                be32(&mut record, 0);
                be32(&mut record, 0);
                be32(&mut record, data.len() as u32);
                record.extend(data);
            }
            AttributeValue::Fork(data) => {
                let extents = allocator.take(data.len(), false);
                be32(&mut record, 0x20);
                be32(&mut record, 0);
                record.extend(fork_record(data.len() as u64, &extents));
                forks.insert(
                    (attribute.id | 0x8000_0000, false),
                    (data.len() as u64, extents, data.clone()),
                );
            }
            AttributeValue::Extents => {
                be32(&mut record, 0x30);
                record.resize(record.len() + 68, 0);
            }
        }
        attributes.push(record);
    }
    let compare = if volume.hfsx { 0xBC } else { 0xCF };
    let per_leaf = volume.records_per_leaf;
    let (extents_file, _) =
        header_and_leaf_chain(&split(overflow, per_leaf, node_size), node_size, 10, 0);
    let (catalog_file, catalog_nodes) = header_and_leaf_chain(
        &split(catalog, per_leaf, node_size),
        node_size,
        516,
        compare,
    );
    let attribute_leaves = split(attributes, per_leaf, node_size);
    let (attributes_file, attribute_nodes) =
        header_and_leaf_chain(&attribute_leaves, node_size, 266, 0);
    let extents_at = allocator.take(extents_file.len(), false);
    let catalog_at = allocator.take(catalog_file.len(), false);
    let attributes_at = if volume.attributes.is_empty() {
        Vec::new()
    } else {
        allocator.take(attributes_file.len(), false)
    };
    let total_blocks = allocator.next + volume.extra_blocks;
    let bitmap_blocks = (total_blocks as usize).div_ceil(8).div_ceil(block as usize) as u32;
    let bitmap_at = vec![(total_blocks - bitmap_blocks - 1, bitmap_blocks)];
    let mut image = vec![0u8; total_blocks as usize * block as usize];
    for (_, extents, data) in forks.values() {
        put(&mut image, block, extents, data);
    }
    put(&mut image, block, &extents_at, &extents_file);
    put(&mut image, block, &catalog_at, &catalog_file);
    if !attributes_at.is_empty() {
        put(&mut image, block, &attributes_at, &attributes_file);
    }
    let offset_of =
        |extents: &[(u32, u32)]| extents.first().map_or(0, |e| e.0 as usize * block as usize);
    layout.header = 1024;
    layout.extents_file = offset_of(&extents_at);
    layout.catalog_file = offset_of(&catalog_at);
    layout.attributes_file = offset_of(&attributes_at);
    layout.catalog_leaves = (1..catalog_nodes)
        .map(|n| layout.catalog_file + n * node_size)
        .collect();
    layout.attribute_leaves = (1..attribute_nodes)
        .map(|n| layout.attributes_file + n * node_size)
        .collect();
    let extents_nodes = extents_file.len() / node_size;
    layout.extents_leaves = (1..extents_nodes)
        .map(|n| layout.extents_file + n * node_size)
        .collect();
    let (files, folders) = volume.counts.unwrap_or((files, folders.saturating_sub(1)));
    let mut header = Vec::new();
    header.extend(if volume.hfsx {
        b"HX\0\x05"
    } else {
        b"H+\0\x04"
    });
    be32(&mut header, 0x100);
    header.extend(b"10.0");
    be32(&mut header, 0);
    for _ in 0..4 {
        be32(&mut header, HFS_EPOCH_2026);
    }
    be32(&mut header, files);
    be32(&mut header, folders);
    be32(&mut header, block);
    be32(&mut header, total_blocks);
    be32(&mut header, volume.extra_blocks);
    be32(&mut header, allocator.next);
    be32(&mut header, 65536);
    be32(&mut header, 65536);
    be32(&mut header, 1 << 20);
    be32(&mut header, 1);
    header.extend(1u64.to_be_bytes());
    header.resize(0x70, 0);
    header.extend(fork_record(
        u64::from(bitmap_blocks) * u64::from(block),
        &bitmap_at,
    ));
    header.extend(fork_record(extents_file.len() as u64, &extents_at));
    header.extend(fork_record(catalog_file.len() as u64, &catalog_at));
    let attributes_size = if attributes_at.is_empty() {
        0
    } else {
        attributes_file.len()
    };
    header.extend(fork_record(attributes_size as u64, &attributes_at));
    header.extend(fork_record(0, &[]));
    assert_eq!(header.len(), 512);
    image[1024..1536].copy_from_slice(&header);
    let end = image.len();
    image[end - 1024..end - 512].copy_from_slice(&header);
    (image, layout)
}
