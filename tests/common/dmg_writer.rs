#![allow(dead_code)]
use std::io::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chunk {
    Zero,
    Ignore,
    Raw,
    Adc,
    Zlib,
    Bzip2,
    Lzfse,
    Xz,
}

impl Chunk {
    pub const ALL: [Chunk; 8] = [
        Chunk::Zero,
        Chunk::Ignore,
        Chunk::Raw,
        Chunk::Adc,
        Chunk::Zlib,
        Chunk::Bzip2,
        Chunk::Lzfse,
        Chunk::Xz,
    ];

    fn kind(self) -> u32 {
        match self {
            Chunk::Zero => 0,
            Chunk::Raw => 1,
            Chunk::Ignore => 2,
            Chunk::Adc => 0x8000_0004,
            Chunk::Zlib => 0x8000_0005,
            Chunk::Bzip2 => 0x8000_0006,
            Chunk::Lzfse => 0x8000_0007,
            Chunk::Xz => 0x8000_0008,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Partition {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Plist,
    ResourceFork,
}

pub type Encoder = Box<dyn Fn(&[u8]) -> Vec<u8>>;

pub struct Options {
    pub chunk_sectors: u64,
    pub methods: Vec<Chunk>,
    pub zeros: Vec<Chunk>,
    pub form: Form,
    pub data_checksum: bool,
    pub lzfse: Option<Encoder>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            chunk_sectors: 64,
            methods: vec![Chunk::Zlib],
            zeros: vec![Chunk::Zero, Chunk::Ignore],
            form: Form::Plist,
            data_checksum: false,
            lzfse: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub koly: usize,
    pub xml: (usize, usize),
    pub resource: (usize, usize),
    pub tables: Vec<(usize, usize)>,
    pub chunks: Vec<(Chunk, usize, usize)>,
}

pub fn adc(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut literals: Vec<u8> = Vec::new();
    let flush = |out: &mut Vec<u8>, literals: &mut Vec<u8>| {
        for run in literals.chunks(128) {
            out.push(0x80 + (run.len() - 1) as u8);
            out.extend(run);
        }
        literals.clear();
    };
    let mut at = 0;
    while at < data.len() {
        let mut best = (0usize, 0usize);
        for distance in [1usize, 2, 3, 4, 7, 16, 100, 1000, 30000] {
            if distance > at {
                break;
            }
            let mut length = 0;
            while length < 67
                && at + length < data.len()
                && data[at + length] == data[at + length - distance]
            {
                length += 1;
            }
            if length > best.0 {
                best = (length, distance);
            }
        }
        if best.0 >= 4 {
            flush(&mut out, &mut literals);
            let distance = best.1 - 1;
            out.push(0x40 + (best.0 - 4) as u8);
            out.push((distance >> 8) as u8);
            out.push(distance as u8);
            at += best.0;
        } else {
            literals.push(data[at]);
            at += 1;
        }
    }
    flush(&mut out, &mut literals);
    out
}

pub fn lzvn(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut literals: Vec<u8> = Vec::new();
    let flush = |out: &mut Vec<u8>, literals: &mut Vec<u8>| {
        for run in literals.chunks(271) {
            if run.len() < 16 {
                out.push(0xE0 + run.len() as u8);
            } else {
                out.push(0xE0);
                out.push((run.len() - 16) as u8);
            }
            out.extend(run);
        }
        literals.clear();
    };
    while at < data.len() {
        let mut best = (0usize, 0usize);
        for distance in [1usize, 2, 3, 8, 64, 500, 4000, 60000] {
            if distance > at {
                break;
            }
            let mut length = 0;
            while length < 10
                && at + length < data.len()
                && data[at + length] == data[at + length - distance]
            {
                length += 1;
            }
            if length > best.0 {
                best = (length, distance);
            }
        }
        if best.0 >= 3 {
            flush(&mut out, &mut literals);
            out.push((((best.0 - 3) << 3) | 7) as u8);
            out.push(best.1 as u8);
            out.push((best.1 >> 8) as u8);
            at += best.0;
        } else {
            literals.push(data[at]);
            at += 1;
        }
    }
    flush(&mut out, &mut literals);
    out.push(0x06);
    out.extend([0; 7]);
    out
}

pub fn lzfse_of_lzvn_blocks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for block in data.chunks(1 << 16) {
        let packed = lzvn(block);
        out.extend(b"bvxn");
        out.extend((block.len() as u32).to_le_bytes());
        out.extend((packed.len() as u32).to_le_bytes());
        out.extend(packed);
    }
    out.extend(b"bvx$");
    out
}

fn compress(method: Chunk, data: &[u8], options: &Options) -> Vec<u8> {
    match method {
        Chunk::Zero | Chunk::Ignore => Vec::new(),
        Chunk::Raw => data.to_vec(),
        Chunk::Adc => adc(data),
        Chunk::Zlib => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
        Chunk::Bzip2 => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
        Chunk::Lzfse => match &options.lzfse {
            Some(encode) => encode(data),
            None => lzfse_of_lzvn_blocks(data),
        },
        Chunk::Xz => {
            let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (index, group) in bytes.chunks(3).enumerate() {
        if index > 0 && index % 18 == 0 {
            out.push_str("\n\t\t\t\t");
        }
        let value = group
            .iter()
            .fold(0u32, |value, &byte| value << 8 | u32::from(byte))
            << (8 * (3 - group.len()));
        for digit in 0..4 {
            if digit <= group.len() {
                out.push(DIGITS[(value >> (18 - 6 * digit) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn checksum(kind: u32, crc: Option<u32>) -> Vec<u8> {
    let mut bytes = Vec::new();
    match crc {
        Some(crc) => {
            bytes.extend(kind.to_be_bytes());
            bytes.extend(32u32.to_be_bytes());
            bytes.extend(crc.to_be_bytes());
        }
        None => bytes.extend([0; 8]),
    }
    bytes.resize(136, 0);
    bytes
}

pub fn mish(
    start_sector: u64,
    sectors: u64,
    data_offset: u64,
    crc: Option<u32>,
    blocks: &[(u32, u64, u64, u64, u64)],
) -> Vec<u8> {
    let mut table = b"mish".to_vec();
    table.extend(1u32.to_be_bytes());
    table.extend(start_sector.to_be_bytes());
    table.extend(sectors.to_be_bytes());
    table.extend(data_offset.to_be_bytes());
    table.extend(0x208u32.to_be_bytes());
    table.extend(0u32.to_be_bytes());
    table.resize(0x40, 0);
    table.extend(checksum(2, crc));
    table.extend((blocks.len() as u32).to_be_bytes());
    for &(kind, start, count, packed, packed_length) in blocks {
        table.extend(kind.to_be_bytes());
        table.extend(0u32.to_be_bytes());
        for value in [start, count, packed, packed_length] {
            table.extend(value.to_be_bytes());
        }
    }
    table
}

fn plist(partitions: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>resource-fork</key>\n\t<dict>\n\t\t<key>blkx</key>\n\t\t<array>\n",
    );
    for (index, (name, table)) in partitions.iter().enumerate() {
        xml.push_str(&format!(
            "\t\t\t<dict>\n\t\t\t\t<key>Attributes</key>\n\t\t\t\t<string>0x0050</string>\n\t\t\t\t<key>CFName</key>\n\t\t\t\t<string>{name}</string>\n\t\t\t\t<key>Data</key>\n\t\t\t\t<data>\n\t\t\t\t{}\n\t\t\t\t</data>\n\t\t\t\t<key>ID</key>\n\t\t\t\t<string>{}</string>\n\t\t\t\t<key>Name</key>\n\t\t\t\t<string>{name}</string>\n\t\t\t</dict>\n",
            base64(table),
            index as i64 - 1
        ));
    }
    xml.push_str("\t\t</array>\n\t</dict>\n</dict>\n</plist>\n");
    xml.into_bytes()
}

fn resource_fork(partitions: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut offsets = Vec::new();
    for (_, table) in partitions {
        offsets.push(data.len() as u32);
        data.extend((table.len() as u32).to_be_bytes());
        data.extend(table);
    }
    let mut names = Vec::new();
    let mut name_offsets = Vec::new();
    for (name, _) in partitions {
        name_offsets.push(names.len() as u16);
        names.push(name.len() as u8);
        names.extend(name.as_bytes());
    }
    let references = 0x1E + 8;
    let list = references - 0x1C;
    let names_at = references + 12 * partitions.len();
    let footer_size = names_at + names.len();
    let header = 0x100u32;
    let footer = header + data.len() as u32;
    let mut fork = Vec::new();
    for value in [header, footer, data.len() as u32, footer_size as u32] {
        fork.extend(value.to_be_bytes());
    }
    fork.resize(0x100, 0);
    fork.extend(&data);
    let mut map = fork[..16].to_vec();
    map.resize(0x18, 0);
    map.extend(0x1Cu16.to_be_bytes());
    map.extend((names_at as u16).to_be_bytes());
    map.extend(0u16.to_be_bytes());
    map.extend(b"blkx");
    map.extend(((partitions.len() - 1) as u16).to_be_bytes());
    map.extend((list as u16).to_be_bytes());
    for (index, offset) in offsets.iter().enumerate() {
        map.extend((index as u16).to_be_bytes());
        map.extend(name_offsets[index].to_be_bytes());
        map.extend((0x5000_0000 | offset).to_be_bytes());
        map.extend(0u32.to_be_bytes());
    }
    map.extend(&names);
    fork.extend(map);
    fork
}

pub fn write(partitions: &[Partition], options: &Options) -> (Vec<u8>, Layout) {
    let mut layout = Layout::default();
    let mut image = Vec::new();
    let mut tables = Vec::new();
    let mut sector = 0u64;
    let mut chunk_index = 0usize;
    let mut checksums = Vec::new();
    for partition in partitions {
        let data_offset = image.len() as u64;
        let mut blocks = Vec::new();
        let mut crc = crc32fast::Hasher::new();
        let chunk = (options.chunk_sectors * 512) as usize;
        let mut start = 0u64;
        for piece in partition.data.chunks(chunk) {
            let zero = piece.iter().all(|&byte| byte == 0) && !options.zeros.is_empty();
            let method = if zero {
                options.zeros[chunk_index % options.zeros.len()]
            } else {
                options.methods[chunk_index % options.methods.len()]
            };
            chunk_index += 1;
            let at = image.len() - data_offset as usize;
            let packed = compress(method, piece, options);
            layout.chunks.push((method, image.len(), packed.len()));
            match method {
                Chunk::Ignore => {}
                Chunk::Zero => crc.update(&vec![0u8; piece.len()]),
                _ => crc.update(piece),
            }
            let sectors = piece.len().div_ceil(512) as u64;
            blocks.push((
                method.kind(),
                start,
                sectors,
                at as u64,
                packed.len() as u64,
            ));
            image.extend(packed);
            start += sectors;
        }
        blocks.push((
            0xFFFF_FFFF,
            start,
            0,
            (image.len() - data_offset as usize) as u64,
            0,
        ));
        let crc = crc.finalize();
        checksums.push(crc);
        tables.push((
            partition.name.clone(),
            mish(sector, start, data_offset, Some(crc), &blocks),
        ));
        sector += start;
    }
    let data_length = image.len() as u64;
    let data_crc = crc32fast::hash(&image);
    let mut master = crc32fast::Hasher::new();
    for crc in &checksums {
        master.update(&crc.to_be_bytes());
    }
    let (xml_at, xml_length, resource_at, resource_length) = match options.form {
        Form::Plist => {
            let xml = plist(&tables);
            let at = image.len();
            image.extend(&xml);
            (at as u64, xml.len() as u64, 0, 0)
        }
        Form::ResourceFork => {
            let fork = resource_fork(&tables);
            let at = image.len();
            image.extend(&fork);
            (0, 0, at as u64, fork.len() as u64)
        }
    };
    layout.xml = (xml_at as usize, xml_length as usize);
    layout.resource = (resource_at as usize, resource_length as usize);
    let mut koly = b"koly".to_vec();
    koly.extend(4u32.to_be_bytes());
    koly.extend(512u32.to_be_bytes());
    koly.extend(1u32.to_be_bytes());
    koly.extend(0u64.to_be_bytes());
    for value in [0, data_length, resource_at, resource_length] {
        koly.extend(value.to_be_bytes());
    }
    koly.extend(1u32.to_be_bytes());
    koly.extend(1u32.to_be_bytes());
    koly.extend([0x5A; 16]);
    koly.extend(checksum(2, options.data_checksum.then_some(data_crc)));
    for value in [xml_at, xml_length] {
        koly.extend(value.to_be_bytes());
    }
    koly.resize(0x160, 0);
    koly.extend(checksum(2, Some(master.finalize())));
    koly.resize(0x1E8, 0);
    koly.extend(1u32.to_be_bytes());
    koly.extend(sector.to_be_bytes());
    koly.resize(512, 0);
    layout.koly = image.len();
    image.extend(koly);
    (image, layout)
}
