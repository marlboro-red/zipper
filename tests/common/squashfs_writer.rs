#![allow(dead_code)]
use std::io::Write;

pub const BLOCK: usize = 4096;
const METADATA: usize = 8192;
const STORED_METADATA: u16 = 0x8000;
const STORED_DATA: u32 = 1 << 24;
const NO_FRAGMENT: u32 = u32::MAX;

#[derive(Clone)]
pub enum Kind {
    File(Vec<u8>),
    Folder,
    Link(String),
    Fifo,
    Again(usize),
    Tail {
        fragment: u32,
        offset: u32,
        size: u32,
    },
}

#[derive(Clone)]
pub struct Entry {
    pub parent: usize,
    pub name: Vec<u8>,
    pub kind: Kind,
    pub mode: u16,
    pub modified: u32,
}

pub const ROOT: usize = usize::MAX;

pub fn file(parent: usize, name: &str, data: &[u8]) -> Entry {
    Entry {
        parent,
        name: name.as_bytes().to_vec(),
        kind: Kind::File(data.to_vec()),
        mode: 0o644,
        modified: 1_600_000_000,
    }
}

pub fn folder(parent: usize, name: &str) -> Entry {
    Entry {
        parent,
        name: name.as_bytes().to_vec(),
        kind: Kind::Folder,
        mode: 0o755,
        modified: 1_600_000_100,
    }
}

pub fn link(parent: usize, name: &str, target: &str) -> Entry {
    Entry {
        parent,
        name: name.as_bytes().to_vec(),
        kind: Kind::Link(target.to_owned()),
        mode: 0o777,
        modified: 1_600_000_200,
    }
}

pub fn tail(parent: usize, name: &str, fragment: u32, offset: u32, size: u32) -> Entry {
    Entry {
        parent,
        name: name.as_bytes().to_vec(),
        kind: Kind::Tail {
            fragment,
            offset,
            size,
        },
        mode: 0o644,
        modified: 1_600_000_000,
    }
}

pub fn again(parent: usize, name: &str, earlier: usize) -> Entry {
    Entry {
        parent,
        name: name.as_bytes().to_vec(),
        kind: Kind::Again(earlier),
        mode: 0,
        modified: 0,
    }
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub compress: bool,
}

pub struct Built {
    pub bytes: Vec<u8>,
    pub inode_at: Vec<usize>,
    pub root_inode_at: usize,
    pub listing_at: Vec<Option<usize>>,
    pub data_at: Vec<Option<usize>>,
    pub inode_table: usize,
    pub directory_table: usize,
}

fn metadata_position(stream: usize) -> (u32, u16) {
    (
        (stream / METADATA * (METADATA + 2)) as u32,
        (stream % METADATA) as u16,
    )
}

fn metadata_disk(start: usize, stream: usize) -> usize {
    start + stream / METADATA * (METADATA + 2) + 2 + stream % METADATA
}

fn stored_metadata(stream: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for piece in stream.chunks(METADATA) {
        out.extend_from_slice(&(STORED_METADATA | piece.len() as u16).to_le_bytes());
        out.extend_from_slice(piece);
    }
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn target_of(entries: &[Entry], index: usize) -> usize {
    match entries[index].kind {
        Kind::Again(ROOT) => entries.len(),
        Kind::Again(earlier) => target_of(entries, earlier),
        _ => index,
    }
}

fn basic_kind(kind: &Kind) -> u16 {
    match kind {
        Kind::Folder => 1,
        Kind::File(_) | Kind::Tail { .. } => 2,
        Kind::Link(_) => 3,
        Kind::Fifo => 6,
        Kind::Again(_) => unreachable!(),
    }
}

pub fn build(entries: &[Entry], options: Options) -> Built {
    build_with_fragments(entries, options, &[])
}

pub fn build_with_fragments(entries: &[Entry], options: Options, fragments: &[Vec<u8>]) -> Built {
    let count = entries.len();
    let folder_of = |index: usize| -> Vec<usize> {
        let mut children: Vec<usize> = (0..count)
            .filter(|child| entries[*child].parent == index)
            .collect();
        children.sort_by(|a, b| entries[*a].name.cmp(&entries[*b].name));
        children
    };
    let mut links = vec![1u32; count + 1];
    for entry in entries {
        if let Kind::Again(earlier) = entry.kind
            && earlier != ROOT
        {
            links[target_of(entries, earlier)] += 1;
        }
        if let Kind::Again(ROOT) = entry.kind {
            links[count] += 1;
        }
    }
    let mut data = vec![0u8; 96];
    let mut data_at = vec![None; count];
    let mut block_words: Vec<Vec<u32>> = vec![Vec::new(); count];
    for (index, entry) in entries.iter().enumerate() {
        if let Kind::File(content) = &entry.kind {
            data_at[index] = Some(data.len());
            for piece in content.chunks(BLOCK) {
                let packed = zlib(piece);
                if options.compress && packed.len() < piece.len() {
                    block_words[index].push(packed.len() as u32);
                    data.extend_from_slice(&packed);
                } else {
                    block_words[index].push(STORED_DATA | piece.len() as u32);
                    data.extend_from_slice(piece);
                }
            }
        }
    }
    let mut fragment_entries = Vec::new();
    for fragment in fragments {
        let start = data.len() as u64;
        let packed = zlib(fragment);
        let word = if options.compress && packed.len() < fragment.len() {
            data.extend_from_slice(&packed);
            packed.len() as u32
        } else {
            data.extend_from_slice(fragment);
            STORED_DATA | fragment.len() as u32
        };
        fragment_entries.extend_from_slice(&start.to_le_bytes());
        fragment_entries.extend_from_slice(&word.to_le_bytes());
        fragment_entries.extend_from_slice(&0u32.to_le_bytes());
    }
    let inode_length = |index: usize| -> usize {
        match &entries[index].kind {
            Kind::Folder => 32,
            Kind::File(_) if links[index] > 1 => 56 + 4 * block_words[index].len(),
            Kind::File(_) => 32 + 4 * block_words[index].len(),
            Kind::Link(target) => 24 + target.len(),
            Kind::Fifo => 20,
            Kind::Tail { .. } => 32,
            Kind::Again(_) => 0,
        }
    };
    let mut inode_total = 0;
    let inode_stream_at: Vec<usize> = (0..=count)
        .map(|index| {
            let at = inode_total;
            inode_total += if index == count {
                32
            } else {
                inode_length(index)
            };
            at
        })
        .collect();
    let slot = |index: usize| if index == ROOT { count } else { index };
    let reference_of = |index: usize| -> (u32, u16) {
        let index = if index == ROOT {
            count
        } else {
            target_of(entries, index)
        };
        metadata_position(inode_stream_at[index])
    };
    let kind_of = |index: usize| -> u16 {
        let target = target_of(entries, index);
        if target == count {
            1
        } else {
            basic_kind(&entries[target].kind)
        }
    };
    let mut listings = Vec::new();
    let mut listing_stream_at = vec![None; count + 1];
    let mut listing_length = vec![0usize; count + 1];
    let folders: Vec<usize> = std::iter::once(ROOT)
        .chain((0..count).filter(|index| matches!(entries[*index].kind, Kind::Folder)))
        .collect();
    for &folder in &folders {
        let children = folder_of(folder);
        listing_stream_at[slot(folder)] = Some(listings.len());
        let start = listings.len();
        for run in children.chunks(256) {
            let (block, _) = reference_of(run[0]);
            listings.extend_from_slice(&(run.len() as u32 - 1).to_le_bytes());
            listings.extend_from_slice(&block.to_le_bytes());
            listings.extend_from_slice(&1u32.to_le_bytes());
            for &child in run {
                let (_, offset) = reference_of(child);
                let kind = kind_of(child);
                listings.extend_from_slice(&offset.to_le_bytes());
                listings.extend_from_slice(&0i16.to_le_bytes());
                listings.extend_from_slice(&kind.to_le_bytes());
                listings.extend_from_slice(&(entries[child].name.len() as u16 - 1).to_le_bytes());
                listings.extend_from_slice(&entries[child].name);
            }
        }
        listing_length[slot(folder)] = listings.len() - start;
    }
    let mut inodes = Vec::with_capacity(inode_total);
    let push16 = |out: &mut Vec<u8>, value: u16| out.extend_from_slice(&value.to_le_bytes());
    let push32 = |out: &mut Vec<u8>, value: u32| out.extend_from_slice(&value.to_le_bytes());
    for index in 0..=count {
        let (kind, mode, modified) = if index == count {
            (Kind::Folder, 0o755, 1_600_000_300)
        } else {
            let entry = &entries[index];
            (entry.kind.clone(), entry.mode, entry.modified)
        };
        if matches!(kind, Kind::Again(_)) {
            continue;
        }
        assert_eq!(inodes.len(), inode_stream_at[index]);
        let long_file = matches!(kind, Kind::File(_)) && links[index] > 1;
        push16(&mut inodes, if long_file { 9 } else { basic_kind(&kind) });
        push16(&mut inodes, mode);
        push16(&mut inodes, 0);
        push16(&mut inodes, 0);
        push32(&mut inodes, modified);
        push32(&mut inodes, index as u32 + 1);
        match kind {
            Kind::Folder => {
                let (start, offset) = metadata_position(listing_stream_at[index].unwrap());
                push32(&mut inodes, start);
                push32(&mut inodes, 2);
                push16(&mut inodes, (listing_length[index] + 3) as u16);
                push16(&mut inodes, offset);
                let parent = if index == count {
                    count as u32 + 2
                } else {
                    slot(entries[index].parent) as u32 + 1
                };
                push32(&mut inodes, parent);
            }
            Kind::File(content) => {
                let start = data_at[index].unwrap() as u64;
                if long_file {
                    inodes.extend_from_slice(&start.to_le_bytes());
                    inodes.extend_from_slice(&(content.len() as u64).to_le_bytes());
                    inodes.extend_from_slice(&0u64.to_le_bytes());
                    push32(&mut inodes, links[index]);
                    push32(&mut inodes, NO_FRAGMENT);
                    push32(&mut inodes, 0);
                    push32(&mut inodes, u32::MAX);
                } else {
                    push32(&mut inodes, start as u32);
                    push32(&mut inodes, NO_FRAGMENT);
                    push32(&mut inodes, 0);
                    push32(&mut inodes, content.len() as u32);
                }
                for word in &block_words[index] {
                    push32(&mut inodes, *word);
                }
            }
            Kind::Link(target) => {
                push32(&mut inodes, 1);
                push32(&mut inodes, target.len() as u32);
                inodes.extend_from_slice(target.as_bytes());
            }
            Kind::Fifo => push32(&mut inodes, 1),
            Kind::Tail {
                fragment,
                offset,
                size,
            } => {
                push32(&mut inodes, 96);
                push32(&mut inodes, fragment);
                push32(&mut inodes, offset);
                push32(&mut inodes, size);
            }
            Kind::Again(_) => unreachable!(),
        }
    }
    let inode_table = data.len();
    let mut image = data;
    image.extend_from_slice(&stored_metadata(&inodes));
    let directory_table = image.len();
    image.extend_from_slice(&stored_metadata(&listings));
    let mut fragment_blocks = Vec::new();
    for piece in fragment_entries.chunks(METADATA) {
        fragment_blocks.push(image.len() as u64);
        image.extend_from_slice(&stored_metadata(piece));
    }
    let fragment_table = image.len();
    for block in fragment_blocks {
        image.extend_from_slice(&block.to_le_bytes());
    }
    let id_block = image.len();
    image.extend_from_slice(&stored_metadata(&0u32.to_le_bytes()));
    let id_table = image.len();
    image.extend_from_slice(&(id_block as u64).to_le_bytes());
    let bytes_used = image.len();
    image.resize(bytes_used.next_multiple_of(4096), 0);
    let (root_block, root_offset) = metadata_position(inode_stream_at[count]);
    let mut superblock = Vec::with_capacity(96);
    superblock.extend_from_slice(b"hsqs");
    let inode_count = entries
        .iter()
        .filter(|entry| !matches!(entry.kind, Kind::Again(_)))
        .count()
        + 1;
    push32(&mut superblock, inode_count as u32);
    push32(&mut superblock, 1_600_000_000);
    push32(&mut superblock, BLOCK as u32);
    push32(&mut superblock, fragments.len() as u32);
    push16(&mut superblock, 1);
    push16(&mut superblock, 12);
    push16(&mut superblock, 0x0B);
    push16(&mut superblock, 1);
    push16(&mut superblock, 4);
    push16(&mut superblock, 0);
    superblock
        .extend_from_slice(&((u64::from(root_block) << 16) | u64::from(root_offset)).to_le_bytes());
    for value in [
        bytes_used as u64,
        id_table as u64,
        u64::MAX,
        inode_table as u64,
        directory_table as u64,
        fragment_table as u64,
        u64::MAX,
    ] {
        superblock.extend_from_slice(&value.to_le_bytes());
    }
    image[..96].copy_from_slice(&superblock);
    let inode_at = (0..count)
        .map(|index| metadata_disk(inode_table, inode_stream_at[target_of(entries, index)]))
        .collect();
    let listing_at = (0..count)
        .map(|index| listing_stream_at[index].map(|at| metadata_disk(directory_table, at)))
        .collect();
    Built {
        bytes: image,
        inode_at,
        root_inode_at: metadata_disk(inode_table, inode_stream_at[count]),
        listing_at,
        data_at,
        inode_table,
        directory_table,
    }
}

pub struct Cram {
    pub bytes: Vec<u8>,
    pub inode_at: Vec<usize>,
    pub data_at: Vec<Option<usize>>,
}

const CRAMFS_MAGIC: u32 = 0x28CD_3D45;
const S_IFDIR: u16 = 0o040_000;
const S_IFREG: u16 = 0o100_000;
const S_IFLNK: u16 = 0o120_000;
const S_IFIFO: u16 = 0o010_000;

fn cram_inode(mode: u16, size: u32, name_words: u32, offset: u32, big: bool) -> [u8; 12] {
    let words = if big {
        [
            u32::from(mode) << 16,
            size << 8,
            (name_words << 26) | (offset >> 2),
        ]
    } else {
        [u32::from(mode), size, name_words | ((offset >> 2) << 6)]
    };
    let mut out = [0u8; 12];
    for (index, word) in words.iter().enumerate() {
        let bytes = if big {
            word.to_be_bytes()
        } else {
            word.to_le_bytes()
        };
        out[index * 4..index * 4 + 4].copy_from_slice(&bytes);
    }
    out
}

pub fn build_cramfs(entries: &[Entry], big: bool) -> Cram {
    let count = entries.len();
    let children_of = |index: usize| -> Vec<usize> {
        let mut children: Vec<usize> = (0..count)
            .filter(|child| entries[*child].parent == index)
            .collect();
        children.sort_by(|a, b| entries[*a].name.cmp(&entries[*b].name));
        children
    };
    let name_bytes = |index: usize| entries[index].name.len().next_multiple_of(4);
    let mut order = vec![ROOT];
    let mut at = 0;
    while at < order.len() {
        let folder = order[at];
        at += 1;
        for child in children_of(folder) {
            if matches!(entries[child].kind, Kind::Folder) {
                order.push(child);
            }
        }
    }
    let mut listing_at = std::collections::HashMap::new();
    let mut listing_size = std::collections::HashMap::new();
    let mut inode_at = vec![0usize; count];
    let mut position = 76;
    for &folder in &order {
        let children = children_of(folder);
        listing_at.insert(folder, if children.is_empty() { 0 } else { position });
        let start = position;
        for child in children {
            inode_at[child] = position;
            position += 12 + name_bytes(child);
        }
        listing_size.insert(folder, position - start);
    }
    let mut data = Vec::new();
    let mut data_at = vec![None; count];
    for (index, entry) in entries.iter().enumerate() {
        let content = match &entry.kind {
            Kind::File(content) => content.clone(),
            Kind::Link(target) => target.as_bytes().to_vec(),
            _ => continue,
        };
        if content.is_empty() {
            continue;
        }
        let start = position + data.len();
        data_at[index] = Some(start);
        let blocks = content.len().div_ceil(BLOCK);
        let mut table = Vec::new();
        let mut packed = Vec::new();
        let mut end = start + 4 * blocks;
        for piece in content.chunks(BLOCK) {
            let block = zlib(piece);
            end += block.len();
            packed.extend_from_slice(&block);
            let word = end as u32;
            table.extend_from_slice(&if big {
                word.to_be_bytes()
            } else {
                word.to_le_bytes()
            });
        }
        data.extend_from_slice(&table);
        data.extend_from_slice(&packed);
        while (position + data.len()) % 4 != 0 {
            data.push(0);
        }
    }
    let mut image = vec![0u8; position];
    image.extend_from_slice(&data);
    let total = image.len().next_multiple_of(4096);
    image.resize(total, 0);
    let root = cram_inode(
        S_IFDIR | 0o755,
        listing_size[&ROOT] as u32,
        0,
        listing_at[&ROOT] as u32,
        big,
    );
    image[64..76].copy_from_slice(&root);
    for (index, entry) in entries.iter().enumerate() {
        let (mode, size, offset) = match &entry.kind {
            Kind::Folder => (
                S_IFDIR | entry.mode,
                listing_size[&index] as u32,
                listing_at[&index] as u32,
            ),
            Kind::File(content) => (
                S_IFREG | entry.mode,
                content.len() as u32,
                data_at[index].unwrap_or(0) as u32,
            ),
            Kind::Link(target) => (
                S_IFLNK | entry.mode,
                target.len() as u32,
                data_at[index].unwrap() as u32,
            ),
            Kind::Fifo => (S_IFIFO | entry.mode, 0, 0),
            Kind::Tail { .. } => unreachable!("CramFS has no fragment blocks"),
            Kind::Again(earlier) => {
                let earlier = if *earlier == ROOT {
                    76 - 12
                } else {
                    inode_at[*earlier]
                };
                let copied: [u8; 12] = image[earlier..earlier + 12].try_into().unwrap();
                let mut copied = copied;
                let name_words = (name_bytes(index) / 4) as u32;
                let field = if big {
                    let word = u32::from_be_bytes(copied[8..12].try_into().unwrap());
                    ((word & 0x03FF_FFFF) | (name_words << 26)).to_be_bytes()
                } else {
                    let word = u32::from_le_bytes(copied[8..12].try_into().unwrap());
                    ((word & !0x3F) | name_words).to_le_bytes()
                };
                copied[8..12].copy_from_slice(&field);
                let at = inode_at[index];
                image[at..at + 12].copy_from_slice(&copied);
                image[at + 12..at + 12 + entry.name.len()].copy_from_slice(&entry.name);
                continue;
            }
        };
        let at = inode_at[index];
        let inode = cram_inode(mode, size, (name_bytes(index) / 4) as u32, offset, big);
        image[at..at + 12].copy_from_slice(&inode);
        image[at + 12..at + 12 + entry.name.len()].copy_from_slice(&entry.name);
    }
    let put = |image: &mut Vec<u8>, at: usize, word: u32| {
        let bytes = if big {
            word.to_be_bytes()
        } else {
            word.to_le_bytes()
        };
        image[at..at + 4].copy_from_slice(&bytes);
    };
    put(&mut image, 0, CRAMFS_MAGIC);
    put(&mut image, 4, total as u32);
    put(&mut image, 8, 3);
    image[16..32].copy_from_slice(b"Compressed ROMFS");
    put(&mut image, 36, 0);
    put(&mut image, 40, (data.len() / BLOCK) as u32);
    put(&mut image, 44, count as u32 + 1);
    image[48..54].copy_from_slice(b"zipper");
    seal_cramfs(&mut image, big);
    Cram {
        bytes: image,
        inode_at,
        data_at,
    }
}

pub fn seal_cramfs(image: &mut [u8], big: bool) {
    image[32..36].fill(0);
    let crc = crc32fast::hash(image);
    let bytes = if big {
        crc.to_be_bytes()
    } else {
        crc.to_le_bytes()
    };
    image[32..36].copy_from_slice(&bytes);
}
