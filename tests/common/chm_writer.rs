#![allow(dead_code)]
use crate::lzx_encoder;
use std::ops::Range;

pub const BLOCK: usize = 32 * 1024;
pub const CHM_LZX_GUID: [u8; 16] = [
    0x40, 0x89, 0xC2, 0x7F, 0x31, 0x9D, 0xD0, 0x11, 0x9B, 0x27, 0x00, 0xA0, 0xC9, 0x1E, 0x9C, 0x7C,
];
pub const HELP2_LZX_GUID: [u8; 16] = [
    0xC6, 0x07, 0x90, 0x0A, 0x76, 0x40, 0xD3, 0x11, 0x87, 0x89, 0x00, 0x00, 0xF8, 0x10, 0x57, 0x54,
];
pub const DES_GUID: [u8; 16] = [
    0xA2, 0xE4, 0xF6, 0x67, 0xBF, 0x60, 0xD3, 0x11, 0x85, 0x40, 0x00, 0xC0, 0x4F, 0x58, 0xC3, 0xCF,
];
const CHM_HEADER: usize = 0x60;
const HELP2_HEADER: usize = 0x160;
const SECTION_ZERO: usize = 0x18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Chm,
    Help2,
}

#[derive(Clone, Debug)]
pub struct File {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub compressed: bool,
}

pub fn file(name: &str, data: &[u8], compressed: bool) -> File {
    File {
        name: name.as_bytes().to_vec(),
        data: data.to_vec(),
        compressed,
    }
}

pub fn dir(name: &str) -> File {
    File {
        name: name.as_bytes().to_vec(),
        data: Vec::new(),
        compressed: false,
    }
}

#[derive(Clone, Debug)]
pub struct RawEntry {
    pub name: Vec<u8>,
    pub section: u64,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub form: Form,
    pub window_bits: u8,
    pub interval_bits: u32,
    pub lzx: lzx_encoder::Options,
    pub chunk_size: usize,
    pub index_chunk: bool,
    pub extra_reset_entry: bool,
    pub zero_count: bool,
    pub transforms: Option<Vec<[u8; 16]>>,
    pub extra: Vec<RawEntry>,
}

impl Options {
    pub fn plain(form: Form) -> Self {
        Options {
            form,
            window_bits: 16,
            interval_bits: 1,
            lzx: lzx_encoder::Options::plain(16),
            chunk_size: if form == Form::Chm { 4096 } else { 8192 },
            index_chunk: false,
            extra_reset_entry: true,
            zero_count: false,
            transforms: None,
            extra: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub header: Range<usize>,
    pub section_zero: Range<usize>,
    pub directory_header: Range<usize>,
    pub chunks: Vec<Range<usize>>,
    pub system: Vec<(String, Range<usize>)>,
    pub blocks: Vec<Range<usize>>,
}

fn encoded(mut value: u64, out: &mut Vec<u8>) {
    let mut groups = vec![(value & 0x7F) as u8];
    value >>= 7;
    while value > 0 {
        groups.push((value & 0x7F) as u8 | 0x80);
        value >>= 7;
    }
    out.extend(groups.iter().rev());
}

fn entry_bytes(entry: &RawEntry) -> Vec<u8> {
    let mut out = Vec::new();
    encoded(entry.name.len() as u64, &mut out);
    out.extend_from_slice(&entry.name);
    encoded(entry.section, &mut out);
    encoded(entry.offset, &mut out);
    encoded(entry.size, &mut out);
    out
}

fn utf16_name(name: &str, out: &mut Vec<u8>) {
    let units: Vec<u16> = name.encode_utf16().collect();
    out.extend((units.len() as u16).to_le_bytes());
    for unit in units {
        out.extend(unit.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
}

pub fn guid_string(guid: &[u8; 16]) -> String {
    let hex = |bytes: Vec<u8>| -> String { bytes.iter().map(|b| format!("{b:02X}")).collect() };
    let reversed = |range: Range<usize>| hex(guid[range].iter().rev().copied().collect());
    format!(
        "{{{}-{}-{}-{}-{}}}",
        reversed(0..4),
        reversed(4..6),
        reversed(6..8),
        hex(guid[8..10].to_vec()),
        hex(guid[10..16].to_vec())
    )
}

pub fn reset_entries(uncompressed: u64, starts: &[u64], compressed: u64, extra: bool) -> Vec<u64> {
    let blocks = (uncompressed + BLOCK as u64 + 1) / BLOCK as u64;
    let wanted = blocks + u64::from(extra);
    let mut entries: Vec<u64> = starts.to_vec();
    while (entries.len() as u64) < wanted {
        entries.push(compressed);
    }
    entries.truncate(wanted as usize);
    entries
}

pub fn reset_table(uncompressed: u64, compressed: u64, entries: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(2u32.to_le_bytes());
    out.extend((entries.len() as u32).to_le_bytes());
    out.extend(8u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend(uncompressed.to_le_bytes());
    out.extend(compressed.to_le_bytes());
    out.extend((BLOCK as u64).to_le_bytes());
    for entry in entries {
        out.extend(entry.to_le_bytes());
    }
    out
}

pub fn lzx_control(interval_bits: u32, window_bits: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [
        6u32,
        u32::from_le_bytes(*b"LZXC"),
        2,
        1 << interval_bits,
        1 << (window_bits - 15),
        1,
        0,
    ] {
        out.extend(value.to_le_bytes());
    }
    out
}

pub fn compress(data: &[u8], options: &Options) -> (Vec<u8>, Vec<u64>) {
    if data.is_empty() {
        return (Vec::new(), vec![0]);
    }
    let mut padded = data.to_vec();
    padded.resize(data.len().div_ceil(BLOCK) * BLOCK, 0);
    let mut lzx = options.lzx.clone();
    lzx.window_bits = options.window_bits;
    lzx.reset_interval = Some(1 << options.interval_bits);
    let chunks = lzx_encoder::encode(&padded, &lzx);
    let mut stream = Vec::new();
    let mut starts = Vec::new();
    for chunk in chunks {
        starts.push(stream.len() as u64);
        stream.extend_from_slice(&chunk.bytes);
    }
    (stream, starts)
}

struct Content {
    bytes: Vec<u8>,
    entries: Vec<RawEntry>,
    system: Vec<(String, Range<usize>)>,
}

impl Content {
    fn add(&mut self, name: &str, data: &[u8]) -> u64 {
        let offset = self.bytes.len() as u64;
        self.bytes.extend_from_slice(data);
        self.entries.push(RawEntry {
            name: name.as_bytes().to_vec(),
            section: 0,
            offset,
            size: data.len() as u64,
        });
        self.system
            .push((name.to_owned(), offset as usize..self.bytes.len()));
        offset
    }
}

pub fn write(files: &[File], options: &Options) -> (Vec<u8>, Layout) {
    let mut section_data = Vec::new();
    let mut entries = vec![RawEntry {
        name: b"/".to_vec(),
        section: 0,
        offset: 0,
        size: 0,
    }];
    let mut content = Content {
        bytes: Vec::new(),
        entries: Vec::new(),
        system: Vec::new(),
    };
    for file in files {
        let directory = file.name.ends_with(b"/");
        let (section, offset) = if directory {
            (0, 0)
        } else if file.compressed {
            let offset = section_data.len() as u64;
            section_data.extend_from_slice(&file.data);
            (1, offset)
        } else {
            let offset = content.bytes.len() as u64;
            content.bytes.extend_from_slice(&file.data);
            (0, offset)
        };
        entries.push(RawEntry {
            name: file.name.clone(),
            section,
            offset,
            size: file.data.len() as u64,
        });
    }
    let (stream, starts) = compress(&section_data, options);
    let uncompressed = section_data.len() as u64;
    let compressed = stream.len() as u64;
    let mut names = Vec::new();
    names.extend(0u16.to_le_bytes());
    names.extend(2u16.to_le_bytes());
    utf16_name("Uncompressed", &mut names);
    utf16_name("MSCompressed", &mut names);
    let words = (names.len() / 2) as u16;
    names[..2].copy_from_slice(&words.to_le_bytes());
    content.add("::DataSpace/NameList", &names);
    let storage = "::DataSpace/Storage/MSCompressed/";
    let guid = match options.form {
        Form::Chm => CHM_LZX_GUID,
        Form::Help2 => HELP2_LZX_GUID,
    };
    let transforms = options.transforms.clone().unwrap_or(vec![guid]);
    let control: Vec<u8> = transforms
        .iter()
        .flat_map(|transform| {
            if *transform == CHM_LZX_GUID || *transform == HELP2_LZX_GUID {
                lzx_control(options.interval_bits, options.window_bits)
            } else {
                0u32.to_le_bytes().to_vec()
            }
        })
        .collect();
    content.add(&format!("{storage}ControlData"), &control);
    content.add(&format!("{storage}SpanInfo"), &uncompressed.to_le_bytes());
    let list: Vec<u8> = match options.form {
        Form::Chm => guid_string(&guid)
            .encode_utf16()
            .take(19)
            .flat_map(u16::to_le_bytes)
            .collect(),
        Form::Help2 => transforms.iter().flatten().copied().collect(),
    };
    content.add(&format!("{storage}Transform/List"), &list);
    let entries_table = reset_entries(uncompressed, &starts, compressed, options.extra_reset_entry);
    content.add(
        &format!(
            "{storage}Transform/{}/InstanceData/ResetTable",
            guid_string(&guid)
        ),
        &reset_table(uncompressed, compressed, &entries_table),
    );
    let stream_at = content.add(&format!("{storage}Content"), &stream);
    entries.extend(content.entries.iter().cloned());
    entries.extend(options.extra.iter().cloned());
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let form = options.form;
    let header_len = if form == Form::Chm { 20 } else { 0x30 };
    let chunk_size = options.chunk_size;
    let mut listings: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
    let mut used = header_len;
    for entry in &entries {
        let bytes = entry_bytes(entry);
        if used + bytes.len() + 2 > chunk_size && !listings.last().unwrap().is_empty() {
            listings.push(Vec::new());
            used = header_len;
        }
        used += bytes.len();
        listings.last_mut().unwrap().push(bytes);
    }
    let count = listings.len();
    let mut directory_chunks = Vec::new();
    for (number, listing) in listings.iter().enumerate() {
        let mut chunk = Vec::new();
        let body: usize = listing.iter().map(Vec::len).sum();
        let quickref = chunk_size - header_len - body;
        let previous = if number == 0 {
            -1i64
        } else {
            number as i64 - 1
        };
        let next = if number + 1 == count {
            -1i64
        } else {
            number as i64 + 1
        };
        match form {
            Form::Chm => {
                chunk.extend(b"PMGL");
                chunk.extend((quickref as u32).to_le_bytes());
                chunk.extend(0u32.to_le_bytes());
                chunk.extend((previous as i32).to_le_bytes());
                chunk.extend((next as i32).to_le_bytes());
            }
            Form::Help2 => {
                chunk.extend(b"AOLL");
                chunk.extend((quickref as u32).to_le_bytes());
                chunk.extend((number as u64).to_le_bytes());
                chunk.extend(previous.to_le_bytes());
                chunk.extend(next.to_le_bytes());
                chunk.extend(0u64.to_le_bytes());
                chunk.extend(1u32.to_le_bytes());
                chunk.extend(0u32.to_le_bytes());
            }
        }
        for bytes in listing {
            chunk.extend_from_slice(bytes);
        }
        chunk.resize(chunk_size - 2, 0);
        let stored = if options.zero_count {
            0
        } else {
            listing.len() as u16
        };
        chunk.extend(stored.to_le_bytes());
        directory_chunks.push(chunk);
    }
    if options.index_chunk {
        let mut chunk = Vec::new();
        chunk.extend(if form == Form::Chm { b"PMGI" } else { b"AOLI" });
        chunk.extend(((chunk_size - 8) as u32).to_le_bytes());
        chunk.resize(chunk_size, 0);
        directory_chunks.push(chunk);
    }
    let chunks = directory_chunks.len() as u32;
    let mut directory = Vec::new();
    match form {
        Form::Chm => {
            directory.extend(b"ITSP");
            for value in [1u32, 0x54, 0x0A, chunk_size as u32, 2, 1, u32::MAX, 0] {
                directory.extend(value.to_le_bytes());
            }
            directory.extend((count as u32 - 1).to_le_bytes());
            directory.extend(u32::MAX.to_le_bytes());
            directory.extend(chunks.to_le_bytes());
            directory.extend(0x409u32.to_le_bytes());
            directory.extend([0x6A, 0x92, 0x02, 0x5D, 0x2E, 0x21, 0xD0, 0x11]);
            directory.extend([0x9D, 0xF9, 0x00, 0xA0, 0xC9, 0x22, 0xE6, 0xEC]);
            for value in [0x54u32, u32::MAX, u32::MAX, u32::MAX] {
                directory.extend(value.to_le_bytes());
            }
        }
        Form::Help2 => {
            directory.extend(b"IFCM");
            for value in [
                1u32,
                chunk_size as u32,
                0x10_0000,
                u32::MAX,
                u32::MAX,
                chunks,
                0,
            ] {
                directory.extend(value.to_le_bytes());
            }
        }
    }
    let directory_header = directory.len();
    let mut chunk_ranges = Vec::new();
    for chunk in &directory_chunks {
        chunk_ranges.push(directory.len()..directory.len() + chunk.len());
        directory.extend_from_slice(chunk);
    }

    let header_size = if form == Form::Chm {
        CHM_HEADER
    } else {
        HELP2_HEADER
    };
    let section_zero_at = header_size;
    let directory_at = section_zero_at + SECTION_ZERO;
    let content_at = directory_at + directory.len();
    let total = (content_at + content.bytes.len()) as u64;
    let mut out = Vec::new();
    match form {
        Form::Chm => {
            out.extend(b"ITSF");
            for value in [3u32, 0x60, 1, 0x1234_5678, 0x409] {
                out.extend(value.to_le_bytes());
            }
            out.extend([0u8; 32]);
            for value in [
                section_zero_at as u64,
                SECTION_ZERO as u64,
                directory_at as u64,
                directory.len() as u64,
                content_at as u64,
            ] {
                out.extend(value.to_le_bytes());
            }
        }
        Form::Help2 => {
            out.extend(b"ITOLITLS");
            for value in [1u32, 0x28, 5, 0x98 + 0x50] {
                out.extend(value.to_le_bytes());
            }
            out.extend([0xC1, 0x07, 0x90, 0x0A, 0x76, 0x40, 0xD3, 0x11]);
            out.extend([0x87, 0x89, 0x00, 0x00, 0xF8, 0x10, 0x57, 0x54]);
            for (offset, size) in [
                (section_zero_at, SECTION_ZERO),
                (directory_at, directory.len()),
                (content_at, 0),
                (content_at, 0),
                (content_at, 0),
            ] {
                out.extend((offset as u64).to_le_bytes());
                out.extend((size as u64).to_le_bytes());
            }
            out.extend(2u32.to_le_bytes());
            out.extend(0x98u32.to_le_bytes());
            for value in [u64::MAX, 0, count as u64 - 1, 0] {
                out.extend(value.to_le_bytes());
            }
            for value in [chunk_size as u32, 2, 0, 1] {
                out.extend(value.to_le_bytes());
            }
            out.extend(0u64.to_le_bytes());
            out.extend((entries.len() as u64).to_le_bytes());
            for value in [u64::MAX, 0, 0, 0] {
                out.extend(value.to_le_bytes());
            }
            for value in [0x200u32, 2, 0, 1] {
                out.extend(value.to_le_bytes());
            }
            out.extend(0u64.to_le_bytes());
            out.extend((count as u64).to_le_bytes());
            out.extend(0x10_0000u32.to_le_bytes());
            out.extend(0x2_0000u32.to_le_bytes());
            out.extend(0u64.to_le_bytes());
            assert_eq!(out.len(), 0x110);
            out.extend(b"CAOL");
            out.extend(2u32.to_le_bytes());
            out.extend(0x50u32.to_le_bytes());
            out.extend(b"HH");
            out.extend(0u16.to_le_bytes());
            for value in [
                0x43EDu32,
                chunk_size as u32,
                0x200,
                0x10_0000,
                0x2_0000,
                0,
                0,
                0,
            ] {
                out.extend(value.to_le_bytes());
            }
            out.extend(b"ITSF");
            for value in [4u32, 0x20, 1] {
                out.extend(value.to_le_bytes());
            }
            out.extend((content_at as u64).to_le_bytes());
            out.extend(0x1234_5678u32.to_le_bytes());
            out.extend(0x409u32.to_le_bytes());
        }
    }
    assert_eq!(out.len(), header_size);
    out.extend(0x01FEu32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(total.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend_from_slice(&directory);
    out.extend_from_slice(&content.bytes);
    let shift = |range: &Range<usize>, by: usize| range.start + by..range.end + by;
    let block_ranges = (0..starts.len())
        .map(|index| {
            let start = starts[index] as usize;
            let end = starts.get(index + 1).map_or(stream.len(), |&s| s as usize);
            shift(&(start..end), content_at + stream_at as usize)
        })
        .collect();
    let layout = Layout {
        header: 0..header_size,
        section_zero: section_zero_at..directory_at,
        directory_header: directory_at..directory_at + directory_header,
        chunks: chunk_ranges
            .iter()
            .map(|range| shift(range, directory_at))
            .collect(),
        system: content
            .system
            .iter()
            .map(|(name, range)| (name.clone(), shift(range, content_at)))
            .collect(),
        blocks: block_ranges,
    };
    (out, layout)
}
