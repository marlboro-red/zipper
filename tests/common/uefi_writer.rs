#![allow(dead_code)]

pub const FFS2: &str = "8C8CE578-8A3D-4F1C-9935-896185C32DD3";
pub const FFS3: &str = "5473C07A-3DCB-4DCA-BD6F-1E9689E7349A";
pub const LZMA: &str = "EE4E5898-3914-4259-9D6E-DC7BD79403CF";
pub const TIANO: &str = "A31280AD-481E-41B6-95E8-127F4C984779";
pub const CRC32: &str = "FC1BCDB0-7D31-49AA-936A-A4600D9DD083";
pub const PAD: &str = "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF";
pub const CAPSULE: &str = "539182B9-ABB5-4391-B69A-E3A943F72FCC";
pub const EFI_CAPSULE: &str = "3B6686BD-0D76-4030-B70E-B5519E2FC5A0";

pub fn guid(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let byte = |at: usize| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).unwrap();
    let mut out = [0u8; 16];
    for (to, from) in [3, 2, 1, 0, 5, 4, 7, 6].into_iter().enumerate() {
        out[to] = byte(from);
    }
    for (at, slot) in out.iter_mut().enumerate().skip(8) {
        *slot = byte(at);
    }
    out
}

pub fn named(first: u32, rest: u64) -> String {
    format!(
        "{first:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
        (rest >> 48) as u16,
        (rest >> 32) as u16,
        (rest >> 16) as u16 ^ 0x8000,
        rest & 0xFFFF_FFFF_FFFF
    )
}

pub fn section(kind: u8, body: &[u8]) -> Vec<u8> {
    let size = (body.len() + 4) as u32;
    let mut out = size.to_le_bytes()[..3].to_vec();
    out.push(kind);
    out.extend_from_slice(body);
    out
}

pub fn sections(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.resize(out.len().next_multiple_of(4), 0);
        out.extend_from_slice(item);
    }
    out
}

pub fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

pub fn ui(name: &str) -> Vec<u8> {
    section(0x15, &utf16(name))
}

pub fn version(text: &str) -> Vec<u8> {
    let mut body = 1u16.to_le_bytes().to_vec();
    body.extend(utf16(text));
    section(0x14, &body)
}

pub fn pe(data: &[u8]) -> Vec<u8> {
    section(0x10, data)
}

pub fn raw(data: &[u8]) -> Vec<u8> {
    section(0x19, data)
}

pub fn guided(guid_text: &str, data: &[u8], attributes: u16, extra: &[u8]) -> Vec<u8> {
    let mut body = guid(guid_text).to_vec();
    body.extend_from_slice(&((4 + 16 + 4 + extra.len()) as u16).to_le_bytes());
    body.extend_from_slice(&attributes.to_le_bytes());
    body.extend_from_slice(extra);
    body.extend_from_slice(data);
    section(0x02, &body)
}

pub fn lzma_alone(data: &[u8]) -> Vec<u8> {
    let options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
    let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
    let mut packed = Vec::new();
    let mut encoder = liblzma::write::XzEncoder::new_stream(&mut packed, stream);
    std::io::Write::write_all(&mut encoder, data).unwrap();
    encoder.finish().unwrap();
    packed[5..13].copy_from_slice(&(data.len() as u64).to_le_bytes());
    packed
}

pub fn lzma_section(inner: &[u8]) -> Vec<u8> {
    guided(LZMA, &lzma_alone(inner), 1, &[])
}

pub fn crc_section(inner: &[u8]) -> Vec<u8> {
    guided(CRC32, inner, 2, &crc32fast::hash(inner).to_le_bytes())
}

pub fn compression(inner: &[u8], kind: u8, packed: Option<&[u8]>) -> Vec<u8> {
    let mut body = (inner.len() as u32).to_le_bytes().to_vec();
    body.push(kind);
    body.extend_from_slice(packed.unwrap_or(inner));
    section(0x01, &body)
}

pub fn tiano_section(inner: &[u8]) -> Vec<u8> {
    guided(TIANO, inner, 1, &[])
}

pub fn fv_section(volume: &[u8]) -> Vec<u8> {
    section(0x17, volume)
}

pub fn ffs_with(name: &str, kind: u8, body: &[u8], attributes: u8, state: u8) -> Vec<u8> {
    let size = (body.len() + 24) as u32;
    let mut head = guid(name).to_vec();
    head.extend_from_slice(&[0, 0, kind, attributes]);
    head.extend_from_slice(&size.to_le_bytes()[..3]);
    head.push(0);
    let sum = head.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    head[16] = sum.wrapping_neg();
    head[17] = if attributes & 0x40 != 0 {
        body.iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg()
    } else {
        0xAA
    };
    head[23] = !state;
    head.extend_from_slice(body);
    head
}

pub fn ffs(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
    ffs_with(name, kind, body, 0, 0x07)
}

pub struct Volume<'a> {
    pub files: Vec<Vec<u8>>,
    pub file_system: &'a str,
    pub extended_name: Option<&'a str>,
    pub length: Option<usize>,
    pub block: usize,
}

impl Default for Volume<'_> {
    fn default() -> Self {
        Volume {
            files: Vec::new(),
            file_system: FFS2,
            extended_name: None,
            length: None,
            block: 4096,
        }
    }
}

pub fn volume(files: Vec<Vec<u8>>) -> Vec<u8> {
    build(&Volume {
        files,
        ..Volume::default()
    })
}

pub fn build(layout: &Volume) -> Vec<u8> {
    let header_length = 72;
    let mut files = layout.files.clone();
    if let Some(name) = layout.extended_name {
        let mut extended = guid(name).to_vec();
        extended.extend_from_slice(&20u32.to_le_bytes());
        files.insert(0, ffs(PAD, 0xF0, &extended));
    }
    let mut body = Vec::new();
    for file in &files {
        body.resize(
            (header_length + body.len()).next_multiple_of(8) - header_length,
            0xFF,
        );
        body.extend_from_slice(file);
    }
    let length = layout
        .length
        .unwrap_or((header_length + body.len()).next_multiple_of(layout.block));
    let mut head = vec![0u8; 16];
    head.extend_from_slice(&guid(layout.file_system));
    head.extend_from_slice(&(length as u64).to_le_bytes());
    head.extend_from_slice(b"_FVH");
    head.extend_from_slice(&0x0004_FEFFu32.to_le_bytes());
    head.extend_from_slice(&(header_length as u16).to_le_bytes());
    head.extend_from_slice(&[0, 0]);
    let extended_at: u16 = if layout.extended_name.is_some() {
        96
    } else {
        0
    };
    head.extend_from_slice(&extended_at.to_le_bytes());
    head.extend_from_slice(&[0, 2]);
    head.extend_from_slice(&((length / layout.block) as u32).to_le_bytes());
    head.extend_from_slice(&(layout.block as u32).to_le_bytes());
    head.extend_from_slice(&[0; 8]);
    fix_volume_checksum(&mut head);
    head.extend(body);
    head.resize(length.max(head.len()), 0xFF);
    head
}

pub fn fix_volume_checksum(volume: &mut [u8]) {
    let length = usize::from(u16::from_le_bytes([volume[48], volume[49]])).min(volume.len()) & !1;
    volume[50..52].fill(0);
    let sum = volume[..length].chunks_exact(2).fold(0u16, |sum, pair| {
        sum.wrapping_add(u16::from_le_bytes([pair[0], pair[1]]))
    });
    volume[50..52].copy_from_slice(&sum.wrapping_neg().to_le_bytes());
}

pub fn capsule(guid_text: &str, payload: &[u8], header_size: u32) -> Vec<u8> {
    let mut out = guid(guid_text).to_vec();
    out.extend_from_slice(&header_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(header_size + payload.len() as u32).to_le_bytes());
    out.resize(header_size as usize, 0);
    out.extend_from_slice(payload);
    out
}
