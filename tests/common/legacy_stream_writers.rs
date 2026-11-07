#![allow(dead_code)]
use std::{io::Write, path::Path, process::Command};

pub const SZDD_SIGNATURE: [u8; 9] = *b"SZDD\x88\xf0\x27\x33\x41";
const SZDD_WINDOW: usize = 4096;
const SZDD_START: usize = SZDD_WINDOW - 16;
const SZDD_SHORTEST: usize = 3;
const SZDD_LONGEST: usize = 16;

#[derive(Clone, Copy, Debug)]
pub enum Token {
    Literal(u8),
    Copy { slot: u16, length: u8 },
}

pub fn szdd_header(replaced: u8, size: u32) -> Vec<u8> {
    let mut out = SZDD_SIGNATURE.to_vec();
    out.push(replaced);
    out.extend_from_slice(&size.to_le_bytes());
    out
}

pub fn szdd_body(tokens: &[Token], spare_flags: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for group in tokens.chunks(8) {
        let mut flags = 0u8;
        let mut codes = Vec::new();
        for (bit, token) in group.iter().enumerate() {
            match *token {
                Token::Literal(byte) => {
                    flags |= 1 << bit;
                    codes.push(byte);
                }
                Token::Copy { slot, length } => {
                    assert!((3..=18).contains(&length) && slot < 4096);
                    codes.push(slot as u8);
                    codes.push(((slot >> 8) as u8) << 4 | (length - 3));
                }
            }
        }
        if group.len() < 8 {
            flags |= spare_flags & (0xffu8 << group.len());
        }
        out.push(flags);
        out.extend(codes);
    }
    out
}

pub fn szdd_tokens(data: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let mut best = (0, 0);
        let earliest = at.saturating_sub(SZDD_WINDOW - 1);
        for from in (earliest..at).rev() {
            let mut length = 0;
            while length < SZDD_LONGEST
                && at + length < data.len()
                && data[from + length] == data[at + length]
            {
                length += 1;
            }
            if length > best.1 {
                best = (from, length);
            }
            if length == SZDD_LONGEST {
                break;
            }
        }
        if best.1 >= SZDD_SHORTEST {
            tokens.push(Token::Copy {
                slot: ((SZDD_START + best.0) % SZDD_WINDOW) as u16,
                length: best.1 as u8,
            });
            at += best.1;
        } else {
            tokens.push(Token::Literal(data[at]));
            at += 1;
        }
    }
    tokens
}

pub fn szdd(data: &[u8], replaced: u8) -> Vec<u8> {
    let mut out = szdd_header(replaced, data.len() as u32);
    out.extend(szdd_body(&szdd_tokens(data), 0));
    out
}

pub fn lzma86_header(filter: u8, properties: u8, dictionary: u32, size: u64) -> Vec<u8> {
    let mut out = vec![filter, properties];
    out.extend_from_slice(&dictionary.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out
}

fn run_stream(mut stream: liblzma::stream::Stream, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 1024);
    let mut input = data;
    loop {
        out.reserve(64 * 1024);
        let before = stream.total_in();
        let status = stream
            .process_vec(input, &mut out, liblzma::stream::Action::Finish)
            .unwrap();
        input = &input[(stream.total_in() - before) as usize..];
        if status == liblzma::stream::Status::StreamEnd {
            return out;
        }
    }
}

fn lzma_options(dictionary: u32, lc: u32, lp: u32, pb: u32) -> liblzma::stream::LzmaOptions {
    let mut options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
    options
        .dict_size(dictionary)
        .literal_context_bits(lc)
        .literal_position_bits(lp)
        .position_bits(pb);
    options
}

pub fn x86_encoded(data: &[u8]) -> Vec<u8> {
    let options = lzma_options(1 << 16, 3, 0, 2);
    let mut chain = liblzma::stream::Filters::new();
    chain.x86().lzma1(&options);
    let packed = run_stream(
        liblzma::stream::Stream::new_raw_encoder(&chain).unwrap(),
        data,
    );
    let mut only_lzma = liblzma::stream::Filters::new();
    only_lzma.lzma1(&options);
    run_stream(
        liblzma::stream::Stream::new_raw_decoder(&only_lzma).unwrap(),
        &packed,
    )
}

pub fn lzma86_with_end_marker(
    data: &[u8],
    bcj: bool,
    sized: bool,
    lc: u32,
    lp: u32,
    pb: u32,
) -> Vec<u8> {
    let dictionary = 1 << 16;
    let filtered = if bcj {
        x86_encoded(data)
    } else {
        data.to_vec()
    };
    let mut chain = liblzma::stream::Filters::new();
    chain.lzma1(&lzma_options(dictionary, lc, lp, pb));
    let packed = run_stream(
        liblzma::stream::Stream::new_raw_encoder(&chain).unwrap(),
        &filtered,
    );
    let properties = ((pb * 5 + lp) * 9 + lc) as u8;
    let size = if sized { data.len() as u64 } else { u64::MAX };
    let mut out = lzma86_header(u8::from(bcj), properties, dictionary, size);
    out.extend(packed);
    out
}

pub fn lzma86_from_7zip(binary: &Path, scratch: &Path, data: &[u8], bcj: bool) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let source = scratch.join("payload.bin");
    std::fs::write(&source, data).unwrap();
    let archive = scratch.join("payload.7z");
    let _ = std::fs::remove_file(&archive);
    let mut command = Command::new(binary);
    command.args([
        "a", "-t7z", "-mhc=off", "-mhe=off", "-mtc=off", "-mtm=off", "-mta=off",
    ]);
    if bcj {
        command.args(["-m0=BCJ", "-m1=LZMA:d=64k:lc=3:lp=0:pb=2"]);
    } else {
        command.args(["-m0=LZMA:d=64k:lc=3:lp=0:pb=2"]);
    }
    let run = command.arg(&archive).arg(&source).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let seven = std::fs::read(&archive).unwrap();
    let packed_size = u64::from_le_bytes(seven[12..20].try_into().unwrap()) as usize;
    let mut out = lzma86_header(u8::from(bcj), 0x5d, 1 << 16, data.len() as u64);
    out.extend_from_slice(&seven[32..32 + packed_size]);
    Some(out)
}

#[derive(Clone, Copy, Debug)]
pub enum Variant {
    H,
    I { restore: u16 },
}

pub fn pmd_header(
    variant_number: u16,
    order: u16,
    memory_mib: u16,
    flags: u16,
    name: &[u8],
) -> Vec<u8> {
    let mut out = 0x84ac_af8fu32.to_le_bytes().to_vec();
    out.extend_from_slice(&0x20u32.to_le_bytes());
    let info = (order - 1) | ((memory_mib - 1) << 4) | (variant_number << 12);
    out.extend_from_slice(&info.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16 | flags << 14).to_le_bytes());
    out.extend_from_slice(&0x5a21_6c3du32.to_le_bytes());
    out.extend_from_slice(name);
    out
}

pub fn pmd(data: &[u8], variant: Variant, order: u16, memory_mib: u16, name: &[u8]) -> Vec<u8> {
    let memory = u32::from(memory_mib) << 20;
    match variant {
        Variant::H => {
            let mut out = pmd_header(7, order, memory_mib, 0, name);
            let mut encoder =
                ppmd_rust::Ppmd7aEncoder::new(Vec::new(), u32::from(order), memory).unwrap();
            encoder.write_all(data).unwrap();
            out.extend(encoder.finish(true).unwrap());
            out
        }
        Variant::I { restore } => {
            let mut out = pmd_header(8, order, memory_mib, restore, name);
            let mut encoder = ppmd_rust::Ppmd8Encoder::new(
                Vec::new(),
                u32::from(order),
                memory,
                ppmd_rust::RestoreMethod::from(restore),
            )
            .unwrap();
            encoder.write_all(data).unwrap();
            out.extend(encoder.finish(true).unwrap());
            out
        }
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8], line: usize, end: &[u8], padded: bool) -> Vec<u8> {
    let mut symbols = Vec::new();
    for chunk in data.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (at, &byte)| {
            value | u32::from(byte) << (16 - 8 * at)
        });
        for at in 0..=chunk.len() {
            symbols.push(BASE64_ALPHABET[(value >> (18 - 6 * at)) as usize & 63]);
        }
        if padded {
            symbols.extend(std::iter::repeat_n(b'=', 3 - chunk.len()));
        }
    }
    if line == 0 {
        return [symbols.as_slice(), end].concat();
    }
    let mut out = Vec::new();
    for row in symbols.chunks(line) {
        out.extend_from_slice(row);
        out.extend_from_slice(end);
    }
    out
}
