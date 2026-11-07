#![allow(dead_code)]
use super::lzh_encoder::{self, Random, Shape, Token, crc16};

#[derive(Clone, Debug)]
pub struct Entry {
    pub level: u8,
    pub method: [u8; 5],
    pub name: Vec<u8>,
    pub packed: Vec<u8>,
    pub size: u32,
    pub crc: u16,
    pub time: u32,
    pub mode: Option<u16>,
    pub extensions: Vec<(u8, Vec<u8>)>,
    pub header_crc: bool,
    pub os: u8,
}

pub const STORED: [u8; 5] = *b"-lh0-";
pub const DIRECTORY: [u8; 5] = *b"-lhd-";

pub fn method(digit: u8) -> [u8; 5] {
    [b'-', b'l', b'h', digit, b'-']
}

pub fn dictionary(method: [u8; 5]) -> usize {
    match method[3] {
        b'4' => 1 << 12,
        b'5' => 1 << 13,
        b'6' => 1 << 15,
        _ => 1 << 16,
    }
}

pub fn position_count_bits(method: [u8; 5]) -> u32 {
    if dictionary(method) <= 1 << 14 { 4 } else { 5 }
}

impl Entry {
    pub fn stored(level: u8, name: &str, data: &[u8]) -> Entry {
        Entry {
            level,
            method: STORED,
            name: name.as_bytes().to_vec(),
            packed: data.to_vec(),
            size: data.len() as u32,
            crc: crc16(data),
            time: 0x5A21_6000,
            mode: None,
            extensions: Vec::new(),
            header_crc: level >= 2,
            os: b'U',
        }
    }

    pub fn directory(level: u8, name: &str) -> Entry {
        Entry {
            method: DIRECTORY,
            ..Entry::stored(level, name, b"")
        }
    }

    pub fn compressed(level: u8, name: &str, data: &[u8], method: [u8; 5], seed: u64) -> Entry {
        let tokens = lzh_encoder::parse(data, dictionary(method), 3, 256);
        assert_eq!(lzh_encoder::replay(&tokens), data);
        Entry::from_tokens(level, name, &tokens, method, seed)
    }

    pub fn from_tokens(
        level: u8,
        name: &str,
        tokens: &[Token],
        method: [u8; 5],
        seed: u64,
    ) -> Entry {
        let data = lzh_encoder::replay(tokens);
        let mut random = Random(seed | 1);
        let blocks: Vec<usize> = (0..64)
            .map(|_| match random.below(4) {
                0 => 1 + random.below(8),
                1 => 1 + random.below(400),
                _ => 1 + random.below(20_000),
            })
            .collect();
        let shape = Shape {
            single_when_possible: true,
        };
        let packed = lzh_encoder::encode(tokens, position_count_bits(method), &blocks, shape);
        Entry {
            method,
            packed,
            size: data.len() as u32,
            crc: crc16(&data),
            ..Entry::stored(level, name, b"")
        }
    }
}

fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&byte| byte == b'/') {
        Some(slash) => (&name[..=slash], &name[slash + 1..]),
        None => (&[], name),
    }
}

fn extension(kind: u8, contents: &[u8], size_field: usize) -> Vec<u8> {
    let mut block = vec![kind];
    block.extend_from_slice(contents);
    block.extend_from_slice(&[0; 4][..size_field]);
    block
}

fn chain(blocks: &mut [Vec<u8>], size_field: usize) -> usize {
    for index in 0..blocks.len() {
        let next = blocks.get(index + 1).map_or(0, Vec::len);
        let at = blocks[index].len() - size_field;
        blocks[index][at..].copy_from_slice(&(next as u32).to_le_bytes()[..size_field]);
    }
    blocks.first().map_or(0, Vec::len)
}

fn common(entry: &Entry, packed: u32, attribute: u8) -> Vec<u8> {
    let mut bytes = vec![0, 0];
    bytes.extend_from_slice(&entry.method);
    bytes.extend_from_slice(&packed.to_le_bytes());
    bytes.extend_from_slice(&entry.size.to_le_bytes());
    bytes.extend_from_slice(&entry.time.to_le_bytes());
    bytes.extend_from_slice(&[attribute, entry.level]);
    bytes
}

fn extended_blocks(entry: &Entry, size_field: usize, with_names: bool) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    if entry.header_crc {
        blocks.push(extension(0x00, &[0, 0], size_field));
    }
    if with_names {
        let (folder, file) = split_name(&entry.name);
        blocks.push(extension(0x01, file, size_field));
        if !folder.is_empty() {
            let folder: Vec<u8> = folder
                .iter()
                .map(|&b| if b == b'/' { 0xFF } else { b })
                .collect();
            blocks.push(extension(0x02, &folder, size_field));
        }
    }
    if let Some(mode) = entry.mode {
        blocks.push(extension(0x50, &mode.to_le_bytes(), size_field));
    }
    for (kind, contents) in &entry.extensions {
        blocks.push(extension(*kind, contents, size_field));
    }
    blocks
}

pub fn entry_bytes(entry: &Entry) -> Vec<u8> {
    let mut out = match entry.level {
        0 | 1 => short_header(entry),
        _ => long_header(entry),
    };
    out.extend_from_slice(&entry.packed);
    out
}

fn short_header(entry: &Entry) -> Vec<u8> {
    let name: Vec<u8> = entry
        .name
        .iter()
        .map(|&b| if b == b'/' { b'\\' } else { b })
        .collect();
    let mut extended = if entry.level == 1 {
        extended_blocks(entry, 2, false)
    } else {
        Vec::new()
    };
    let first = chain(&mut extended, 2);
    let extended: Vec<u8> = extended.concat();
    let packed = entry.packed.len() as u32
        + if entry.level == 1 {
            extended.len() as u32
        } else {
            0
        };
    let mut header = common(entry, packed, 0x20);
    header.push(name.len() as u8);
    header.extend_from_slice(&name);
    header.extend_from_slice(&entry.crc.to_le_bytes());
    if entry.level == 0 {
        if let Some(mode) = entry.mode {
            header.extend_from_slice(b"U\0");
            header.extend_from_slice(&entry.time.to_le_bytes());
            header.extend_from_slice(&mode.to_le_bytes());
            header.extend_from_slice(&[0; 4]);
        }
    } else {
        header.push(entry.os);
        header.extend_from_slice(&(first as u16).to_le_bytes());
    }
    header[0] = (header.len() - 2) as u8;
    header[1] = header[2..].iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
    header.extend_from_slice(&extended);
    header
}

fn long_header(entry: &Entry) -> Vec<u8> {
    let size_field = if entry.level == 3 { 4 } else { 2 };
    let mut blocks = extended_blocks(entry, size_field, true);
    let first = chain(&mut blocks, size_field);
    let mut header = common(entry, entry.packed.len() as u32, 0x20);
    header.extend_from_slice(&entry.crc.to_le_bytes());
    header.push(entry.os);
    if entry.level == 3 {
        header[0..2].copy_from_slice(&4u16.to_le_bytes());
        header.extend_from_slice(&[0; 4]);
        header.extend_from_slice(&(first as u32).to_le_bytes());
    } else {
        header.extend_from_slice(&(first as u16).to_le_bytes());
    }
    let crc_at = header.len() + 1;
    header.extend(blocks.concat());
    let mut total = header.len();
    if entry.level == 2 && total & 0xFF == 0 {
        header.push(0);
        total += 1;
    }
    if entry.level == 3 {
        header[24..28].copy_from_slice(&(total as u32).to_le_bytes());
    } else {
        header[0..2].copy_from_slice(&(total as u16).to_le_bytes());
    }
    if entry.header_crc {
        let crc = crc16(&header);
        header[crc_at..crc_at + 2].copy_from_slice(&crc.to_le_bytes());
    }
    header
}

pub fn archive(entries: &[Entry]) -> Vec<u8> {
    let mut out: Vec<u8> = entries.iter().flat_map(entry_bytes).collect();
    out.push(0);
    out
}
