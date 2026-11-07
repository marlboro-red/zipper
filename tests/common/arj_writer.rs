#![allow(dead_code)]
use super::lzh_encoder::{self, Random, Shape, Token};

pub const HOST_MS_DOS: u8 = 0;
pub const HOST_UNIX: u8 = 2;
pub const BINARY: u8 = 0;
pub const TEXT: u8 = 1;
pub const DIRECTORY: u8 = 3;
pub const HISTORY: usize = 26_624;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Vec<u8>,
    pub comment: Vec<u8>,
    pub method: u8,
    pub kind: u8,
    pub flags: u8,
    pub host: u8,
    pub mode: u16,
    pub time: u32,
    pub packed: Vec<u8>,
    pub size: u32,
    pub crc: u32,
    pub first_size: u8,
    pub extended: Vec<Vec<u8>>,
}

impl Entry {
    pub fn stored(name: &str, data: &[u8]) -> Entry {
        Entry {
            name: name.as_bytes().to_vec(),
            comment: Vec::new(),
            method: 0,
            kind: BINARY,
            flags: 0x10,
            host: HOST_MS_DOS,
            mode: 0x20,
            time: 0x5A21_6000,
            packed: data.to_vec(),
            size: data.len() as u32,
            crc: crc32fast::hash(data),
            first_size: 30,
            extended: Vec::new(),
        }
    }

    pub fn directory(name: &str) -> Entry {
        Entry {
            kind: DIRECTORY,
            mode: 0x10,
            ..Entry::stored(name, b"")
        }
    }

    pub fn compressed(name: &str, data: &[u8], method: u8, seed: u64) -> Entry {
        let tokens = if method == 4 {
            lzh_encoder::parse(data, 15_872, 3, 256)
        } else {
            lzh_encoder::parse(data, HISTORY, 3, 256)
        };
        assert_eq!(lzh_encoder::replay(&tokens), data);
        Entry::from_tokens(name, &tokens, method, seed)
    }

    pub fn from_tokens(name: &str, tokens: &[Token], method: u8, seed: u64) -> Entry {
        let data = lzh_encoder::replay(tokens);
        let packed = if method == 4 {
            lzh_encoder::encode_fastest(tokens)
        } else {
            let mut random = Random(seed | 1);
            let blocks: Vec<usize> = (0..64)
                .map(|_| match random.below(3) {
                    0 => 1 + random.below(50),
                    _ => 1 + random.below(30_000),
                })
                .collect();
            let shape = Shape {
                single_when_possible: true,
            };
            lzh_encoder::encode(tokens, 5, &blocks, shape)
        };
        Entry {
            method,
            packed,
            size: data.len() as u32,
            crc: crc32fast::hash(&data),
            ..Entry::stored(name, b"")
        }
    }
}

fn block(header: &[u8], extended: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0x60, 0xEA];
    out.extend_from_slice(&(header.len() as u16).to_le_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(&crc32fast::hash(header).to_le_bytes());
    for extra in extended {
        out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        out.extend_from_slice(extra);
        out.extend_from_slice(&crc32fast::hash(extra).to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

pub fn main_header(flags: u8) -> Vec<u8> {
    let mut header = vec![0u8; 34];
    header[0] = 34;
    header[1] = 11;
    header[2] = 1;
    header[3] = HOST_MS_DOS;
    header[4] = flags;
    header[6] = 2;
    header[8..12].copy_from_slice(&0x5A21_6000u32.to_le_bytes());
    header[12..16].copy_from_slice(&0x5A21_6000u32.to_le_bytes());
    header.extend_from_slice(b"TEST.ARJ\0\0");
    block(&header, &[])
}

pub fn entry_header(entry: &Entry) -> Vec<u8> {
    let mut header = vec![0u8; usize::from(entry.first_size)];
    header[0] = entry.first_size;
    header[1] = 11;
    header[2] = 1;
    header[3] = entry.host;
    header[4] = entry.flags;
    header[5] = entry.method;
    header[6] = entry.kind;
    header[8..12].copy_from_slice(&entry.time.to_le_bytes());
    header[12..16].copy_from_slice(&(entry.packed.len() as u32).to_le_bytes());
    header[16..20].copy_from_slice(&entry.size.to_le_bytes());
    header[20..24].copy_from_slice(&entry.crc.to_le_bytes());
    header[26..28].copy_from_slice(&entry.mode.to_le_bytes());
    header.extend_from_slice(&entry.name);
    header.push(0);
    header.extend_from_slice(&entry.comment);
    header.push(0);
    block(&header, &entry.extended)
}

pub fn entry_bytes(entry: &Entry) -> Vec<u8> {
    let mut out = entry_header(entry);
    out.extend_from_slice(&entry.packed);
    out
}

pub const END: [u8; 4] = [0x60, 0xEA, 0, 0];

pub fn archive(entries: &[Entry]) -> Vec<u8> {
    let mut out = main_header(0x10);
    for entry in entries {
        out.extend(entry_bytes(entry));
    }
    out.extend_from_slice(&END);
    out
}
