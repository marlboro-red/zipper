#![allow(dead_code)]
use crate::{lzx_encoder, quantum_encoder};

pub const BLOCK: usize = 32 * 1024;
pub const READ_ONLY: u16 = 0x01;
pub const DIRECTORY: u16 = 0x10;
pub const NAME_IS_UTF8: u16 = 0x80;
pub const DATE: u16 = (45 << 9) | (6 << 5) | 15;
pub const TIME: u16 = (13 << 11) | (37 << 5) | 21;

#[derive(Clone, Debug)]
pub enum Method {
    Stored,
    Mszip,
    Quantum(quantum_encoder::Options),
    Lzx(lzx_encoder::Options),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub packed: Vec<u8>,
    pub size: usize,
}

#[derive(Clone, Debug)]
pub struct Folder {
    pub type_compress: u16,
    pub blocks: Vec<Block>,
    pub declared_blocks: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct File {
    pub name: Vec<u8>,
    pub folder: u16,
    pub offset: u32,
    pub size: u32,
    pub date: u16,
    pub time: u16,
    pub attributes: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Cabinet {
    pub folders: Vec<Folder>,
    pub files: Vec<File>,
    pub reserve: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    pub previous: Option<(String, String)>,
    pub next: Option<(String, String)>,
    pub checksums: bool,
    pub trailing: Vec<u8>,
}

pub fn mszip_blocks(data: &[u8]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut history: &[u8] = &[];
    for chunk in data.chunks(BLOCK) {
        let mut compress = flate2::Compress::new(flate2::Compression::new(6), false);
        if !history.is_empty() {
            compress.set_dictionary(history).unwrap();
        }
        let mut packed = vec![0u8; chunk.len() + 1024];
        let status = compress
            .compress(chunk, &mut packed, flate2::FlushCompress::Finish)
            .unwrap();
        assert_eq!(status, flate2::Status::StreamEnd);
        packed.truncate(compress.total_out() as usize);
        packed.splice(0..0, *b"CK");
        blocks.push(Block {
            packed,
            size: chunk.len(),
        });
        history = &chunk[chunk.len().saturating_sub(BLOCK)..];
    }
    blocks
}

pub fn folder(method: &Method, data: &[u8]) -> Folder {
    match method {
        Method::Stored => Folder {
            type_compress: 0,
            blocks: data
                .chunks(BLOCK)
                .map(|chunk| Block {
                    packed: chunk.to_vec(),
                    size: chunk.len(),
                })
                .collect(),
            declared_blocks: None,
        },
        Method::Mszip => Folder {
            type_compress: 1,
            blocks: mszip_blocks(data),
            declared_blocks: None,
        },
        Method::Quantum(options) => Folder {
            type_compress: 2 | (4 << 4) | (u16::from(options.window_bits) << 8),
            blocks: quantum_encoder::encode(data, options)
                .into_iter()
                .map(|chunk| Block {
                    packed: chunk.bytes,
                    size: chunk.size,
                })
                .collect(),
            declared_blocks: None,
        },
        Method::Lzx(options) => Folder {
            type_compress: 3 | (u16::from(options.window_bits) << 8),
            blocks: if data.is_empty() {
                Vec::new()
            } else {
                lzx_encoder::encode(data, options)
                    .into_iter()
                    .map(|chunk| Block {
                        packed: chunk.bytes,
                        size: chunk.size,
                    })
                    .collect()
            },
            declared_blocks: None,
        },
    }
}

pub fn file(name: &str, folder: u16, offset: u32, size: u32) -> File {
    File {
        name: name.replace('/', "\\").into_bytes(),
        folder,
        offset,
        size,
        date: DATE,
        time: TIME,
        attributes: 0x20,
    }
}

pub fn one_folder_in_order(files: &[(&str, &[u8])], method: &Method) -> Cabinet {
    let mut data = Vec::new();
    let mut records = Vec::new();
    for (name, contents) in files {
        records.push(file(name, 0, data.len() as u32, contents.len() as u32));
        data.extend_from_slice(contents);
    }
    Cabinet {
        folders: vec![folder(method, &data)],
        files: records,
        checksums: true,
        ..Cabinet::default()
    }
}

fn block_checksum(bytes: &[u8]) -> u32 {
    let mut sum = 0u32;
    let whole = bytes.len() / 4 * 4;
    for word in bytes[..whole].chunks(4) {
        sum ^= u32::from_le_bytes(word.try_into().unwrap());
    }
    let mut tail = 0u32;
    for &byte in &bytes[whole..] {
        tail = (tail << 8) | u32::from(byte);
    }
    sum ^ tail
}

impl Cabinet {
    pub fn build(&self) -> Vec<u8> {
        let (header_reserve, folder_reserve, data_reserve) =
            self.reserve.clone().unwrap_or_default();
        let mut flags = 0u16;
        let mut head = Vec::new();
        if self.previous.is_some() {
            flags |= 1;
        }
        if self.next.is_some() {
            flags |= 2;
        }
        if self.reserve.is_some() {
            flags |= 4;
            head.extend_from_slice(&(header_reserve.len() as u16).to_le_bytes());
            head.push(folder_reserve.len() as u8);
            head.push(data_reserve.len() as u8);
            head.extend_from_slice(&header_reserve);
        }
        for (cabinet, disk) in self.previous.iter().chain(&self.next) {
            for name in [cabinet, disk] {
                head.extend_from_slice(name.as_bytes());
                head.push(0);
            }
        }
        let folders_at = 36 + head.len();
        let folder_record = 8 + folder_reserve.len();
        let files_at = folders_at + self.folders.len() * folder_record;
        let mut file_table = Vec::new();
        for file in &self.files {
            file_table.extend_from_slice(&file.size.to_le_bytes());
            file_table.extend_from_slice(&file.offset.to_le_bytes());
            file_table.extend_from_slice(&file.folder.to_le_bytes());
            file_table.extend_from_slice(&file.date.to_le_bytes());
            file_table.extend_from_slice(&file.time.to_le_bytes());
            file_table.extend_from_slice(&file.attributes.to_le_bytes());
            file_table.extend_from_slice(&file.name);
            file_table.push(0);
        }
        let mut data_at = files_at + file_table.len();
        let mut folder_table = Vec::new();
        let mut data = Vec::new();
        for folder in &self.folders {
            folder_table.extend_from_slice(&(data_at as u32).to_le_bytes());
            let count = folder.declared_blocks.unwrap_or(folder.blocks.len() as u16);
            folder_table.extend_from_slice(&count.to_le_bytes());
            folder_table.extend_from_slice(&folder.type_compress.to_le_bytes());
            folder_table.extend_from_slice(&folder_reserve);
            for block in &folder.blocks {
                assert!(
                    block.packed.len() <= usize::from(u16::MAX),
                    "a CFDATA block holds at most 64 KiB"
                );
                let mut header = vec![0u8; 4];
                header.extend_from_slice(&(block.packed.len() as u16).to_le_bytes());
                header.extend_from_slice(&(block.size as u16).to_le_bytes());
                header.extend_from_slice(&data_reserve);
                if self.checksums {
                    let sum = block_checksum(&header[4..]) ^ block_checksum(&block.packed);
                    header[..4].copy_from_slice(&sum.to_le_bytes());
                }
                data_at += header.len() + block.packed.len();
                data.extend_from_slice(&header);
                data.extend_from_slice(&block.packed);
            }
        }
        let total = data_at;
        let mut bytes = b"MSCF\0\0\0\0".to_vec();
        bytes.extend_from_slice(&(total as u32).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&(files_at as u32).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&[3, 1]);
        bytes.extend_from_slice(&(self.folders.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(self.files.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&0x1234u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&head);
        bytes.extend_from_slice(&folder_table);
        bytes.extend_from_slice(&file_table);
        bytes.extend_from_slice(&data);
        assert_eq!(bytes.len(), total);
        bytes.extend_from_slice(&self.trailing);
        bytes
    }
}
