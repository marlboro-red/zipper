#![allow(dead_code)]
use std::{io::Write, path::Path, process::Command};

pub const END: u16 = 0;
pub const AUDIO: u8 = 8;
pub const VIDEO: u8 = 9;
pub const SCRIPT: u8 = 18;

pub struct Random(pub u64);

impl Random {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }

    pub fn bytes(&mut self, length: usize) -> Vec<u8> {
        match self.below(3) {
            0 => (0..length).map(|_| self.next() as u8).collect(),
            1 => (0..length).map(|index| (index / 7) as u8).collect(),
            _ => b"flash tag body "
                .iter()
                .copied()
                .cycle()
                .take(length)
                .collect(),
        }
    }
}

pub fn swf_tag(code: u16, body: &[u8], long: bool) -> Vec<u8> {
    assert!(code < 1024);
    let mut out = Vec::new();
    if body.len() < 63 && !long {
        out.extend_from_slice(&(code << 6 | body.len() as u16).to_le_bytes());
    } else {
        out.extend_from_slice(&(code << 6 | 0x3F).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    }
    out.extend_from_slice(body);
    out
}

pub fn rect(width: u8) -> Vec<u8> {
    assert!(width < 32);
    let mut bits = vec![];
    for shift in (0..5).rev() {
        bits.push(width >> shift & 1);
    }
    for field in 0..4u32 {
        for shift in (0..u32::from(width)).rev() {
            bits.push(((field * 1000 + 7) >> shift.min(31) & 1) as u8);
        }
    }
    bits.resize(bits.len().div_ceil(8) * 8, 0);
    bits.chunks(8)
        .map(|byte| byte.iter().fold(0, |value, bit| value << 1 | bit))
        .collect()
}

pub fn movie_with_tail(version: u8, width: u8, tags: &[Vec<u8>], end: &[u8]) -> Vec<u8> {
    let mut body = rect(width);
    body.extend_from_slice(&0x1800u16.to_le_bytes());
    body.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    for tag in tags {
        body.extend_from_slice(tag);
    }
    body.extend_from_slice(end);
    let mut out = b"FWS".to_vec();
    out.push(version);
    out.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

pub fn movie(version: u8, width: u8, tags: &[Vec<u8>]) -> Vec<u8> {
    movie_with_tail(version, width, tags, &swf_tag(END, b"", false))
}

pub fn with_length(mut swf: Vec<u8>, length: u32) -> Vec<u8> {
    swf[4..8].copy_from_slice(&length.to_le_bytes());
    swf
}

pub fn zlib_swf(swf: &[u8], level: u32) -> Vec<u8> {
    let mut out = b"CWS".to_vec();
    out.extend_from_slice(&swf[3..8]);
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(&swf[8..]).unwrap();
    out.extend(encoder.finish().unwrap());
    out
}

pub fn lzma_swf_from_parts(swf: &[u8], properties: u8, dictionary: u32, packed: &[u8]) -> Vec<u8> {
    let mut out = b"ZWS".to_vec();
    out.extend_from_slice(&swf[3..8]);
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.push(properties);
    out.extend_from_slice(&dictionary.to_le_bytes());
    out.extend_from_slice(packed);
    out
}

pub fn lzma_swf_with_end_marker(swf: &[u8], dictionary: u32, lc: u32, lp: u32, pb: u32) -> Vec<u8> {
    let mut options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
    options
        .dict_size(dictionary)
        .literal_context_bits(lc)
        .literal_position_bits(lp)
        .position_bits(pb);
    let mut chain = liblzma::stream::Filters::new();
    chain.lzma1(&options);
    let mut stream = liblzma::stream::Stream::new_raw_encoder(&chain).unwrap();
    let mut packed = Vec::with_capacity(swf.len() + 1024);
    let mut input = &swf[8..];
    loop {
        packed.reserve(64 * 1024);
        let before = stream.total_in();
        let status = stream
            .process_vec(input, &mut packed, liblzma::stream::Action::Finish)
            .unwrap();
        input = &input[(stream.total_in() - before) as usize..];
        if status == liblzma::stream::Status::StreamEnd {
            break;
        }
    }
    let properties = ((pb * 5 + lp) * 9 + lc) as u8;
    lzma_swf_from_parts(swf, properties, dictionary, &packed)
}

pub fn lzma_swf_from_7zip(binary: &Path, scratch: &Path, swf: &[u8]) -> Vec<u8> {
    let source = scratch.join("body.bin");
    std::fs::write(&source, &swf[8..]).unwrap();
    let archive = scratch.join("body.7z");
    let _ = std::fs::remove_file(&archive);
    let run = Command::new(binary)
        .args([
            "a",
            "-t7z",
            "-mhc=off",
            "-mhe=off",
            "-mtc=off",
            "-mtm=off",
            "-mta=off",
            "-m0=LZMA:d=64k:lc=3:lp=0:pb=2",
        ])
        .arg(&archive)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let seven = std::fs::read(&archive).unwrap();
    let packed_size = u64::from_le_bytes(seven[12..20].try_into().unwrap()) as usize;
    lzma_swf_from_parts(swf, 0x5D, 1 << 16, &seven[32..32 + packed_size])
}

pub fn flv_tag(kind: u8, data: &[u8], timestamp: u32) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&timestamp.to_be_bytes()[1..]);
    out.push((timestamp >> 24) as u8);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(data);
    out.extend_from_slice(&(11 + data.len() as u32).to_be_bytes());
    out
}

pub fn audio(format: u8, low: u8, payload: &[u8], timestamp: u32) -> Vec<u8> {
    let mut data = vec![format << 4 | low & 0x0F];
    data.extend_from_slice(payload);
    flv_tag(AUDIO, &data, timestamp)
}

pub fn video(frame: u8, codec: u8, payload: &[u8], timestamp: u32) -> Vec<u8> {
    let mut data = vec![frame << 4 | codec & 0x0F];
    data.extend_from_slice(payload);
    flv_tag(VIDEO, &data, timestamp)
}

pub fn flv(flags: u8, tags: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"FLV\x01".to_vec();
    out.push(flags);
    out.extend_from_slice(&9u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    for tag in tags {
        out.extend_from_slice(tag);
    }
    out
}
