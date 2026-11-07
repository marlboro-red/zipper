#![allow(dead_code)]
//! A small WIM writer for tests. It writes stored resources, or chunked
//! resources whose chunks are stored or XPRESS-coded with a flat code, and
//! can put every file's data in one solid resource. Hooks change the
//! metadata before its hash is taken and the finished file afterwards, so
//! a test can build any layout.
use sha1::Digest;

pub const HEADER: usize = 0xD0;
pub const DENTRY: usize = 102;
const XPRESS_LENGTHS: usize = 256;

#[derive(Clone, Debug)]
pub enum Node {
    Dir {
        name: String,
        children: Vec<Node>,
    },
    File {
        name: String,
        data: Vec<u8>,
        attributes: u32,
        link_group: u64,
        streams: Vec<(String, Vec<u8>)>,
    },
    Reparse {
        name: String,
        tag: u32,
        data: Vec<u8>,
        directory: bool,
    },
}

pub fn dir(name: &str, children: Vec<Node>) -> Node {
    Node::Dir {
        name: name.to_owned(),
        children,
    }
}

pub fn file(name: &str, data: &[u8]) -> Node {
    Node::File {
        name: name.to_owned(),
        data: data.to_vec(),
        attributes: 0x80,
        link_group: 0,
        streams: Vec::new(),
    }
}

pub fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Reparse data of a symbolic link, as `IO_REPARSE_TAG_SYMLINK` stores it
/// after its 8-byte header.
pub fn symlink_data(target: &str, relative: bool) -> Vec<u8> {
    let name = utf16(target);
    let mut data = Vec::new();
    for value in [
        0u16,
        name.len() as u16,
        name.len() as u16,
        name.len() as u16,
    ] {
        data.extend(value.to_le_bytes());
    }
    data.extend(u32::from(relative).to_le_bytes());
    data.extend(&name);
    data.extend(&name);
    data
}

pub fn symlink(name: &str, target: &str, relative: bool) -> Node {
    Node::Reparse {
        name: name.to_owned(),
        tag: 0xA000_000C,
        data: symlink_data(target, relative),
        directory: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Stored,
    Xpress,
}

pub type MetadataHook = Box<dyn Fn(usize, &mut Vec<u8>)>;
pub type FileHook = Box<dyn Fn(&mut Vec<u8>, &Layout)>;

pub struct Options {
    pub compression: Compression,
    pub chunk_size: u32,
    pub solid: bool,
    pub metadata_hook: Option<MetadataHook>,
    pub file_hook: Option<FileHook>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            compression: Compression::Stored,
            chunk_size: 32 * 1024,
            solid: false,
            metadata_hook: None,
            file_hook: None,
        }
    }
}

/// Where the writer put each part, for hooks that damage them.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub blob_table: usize,
    pub blob_table_entries: usize,
    pub xml: usize,
    pub metadata: Vec<(usize, usize)>,
    pub data: Vec<(usize, usize)>,
}

struct Bits {
    words: Vec<u16>,
    value: u32,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            self.value = self.value << 1 | (value >> bit) & 1;
            self.count += 1;
            if self.count == 16 {
                self.words.push(self.value as u16);
                self.value = 0;
                self.count = 0;
            }
        }
    }
}

/// One XPRESS chunk with every code 9 bits long, using matches at distance
/// 1 for runs of a repeated byte.
pub fn xpress_chunk(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits {
        words: Vec::new(),
        value: 0,
        count: 0,
    };
    let mut extra: Vec<(usize, u8)> = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let run = data[at..]
            .iter()
            .take_while(|&&byte| byte == data[at])
            .count();
        if at > 0 && data[at - 1] == data[at] && run >= 3 {
            let length = run.min(3 + 14);
            bits.put(256 + (length - 3) as u32, 9);
            at += length;
            continue;
        }
        bits.put(u32::from(data[at]), 9);
        at += 1;
    }
    extra.clear();
    bits.put(256, 9);
    while bits.count != 0 {
        bits.put(0, 1);
    }
    let loaded = (bits.words.len() + 1).max(2);
    bits.words.resize(loaded, 0);
    let mut out = vec![0x99u8; XPRESS_LENGTHS];
    for word in bits.words {
        out.extend(word.to_le_bytes());
    }
    out
}

fn reshdr(packed: u64, flags: u8, offset: u64, size: u64) -> [u8; 24] {
    let mut bytes = [0u8; 24];
    bytes[..8].copy_from_slice(&(packed | u64::from(flags) << 56).to_le_bytes());
    bytes[8..16].copy_from_slice(&offset.to_le_bytes());
    bytes[16..].copy_from_slice(&size.to_le_bytes());
    bytes
}

fn chunked(data: &[u8], chunk_size: usize) -> Vec<u8> {
    let chunks: Vec<Vec<u8>> = data
        .chunks(chunk_size)
        .map(|chunk| {
            let coded = xpress_chunk(chunk);
            if coded.len() < chunk.len() {
                coded
            } else {
                chunk.to_vec()
            }
        })
        .collect();
    let wide = data.len() as u64 >= 1 << 32;
    let mut out = Vec::new();
    let mut end = 0u64;
    for chunk in &chunks[..chunks.len() - 1] {
        end += chunk.len() as u64;
        if wide {
            out.extend(end.to_le_bytes());
        } else {
            out.extend((end as u32).to_le_bytes());
        }
    }
    for chunk in chunks {
        out.extend(chunk);
    }
    out
}

struct Dentry {
    name: Vec<u8>,
    attributes: u32,
    hash: [u8; 20],
    tag_or_group: u64,
    streams: Vec<(Vec<u8>, [u8; 20])>,
    children: Option<Vec<usize>>,
}

fn sha1(data: &[u8]) -> [u8; 20] {
    sha1::Sha1::digest(data).into()
}

fn dentry_bytes(dentry: &Dentry, subdir: u64) -> Vec<u8> {
    let name_bytes = dentry.name.len();
    let length = (DENTRY + if name_bytes == 0 { 0 } else { name_bytes + 2 }).next_multiple_of(8);
    let length = length.max(DENTRY + 2).next_multiple_of(8);
    let mut bytes = vec![0u8; length];
    bytes[..8].copy_from_slice(&(length as u64).to_le_bytes());
    bytes[8..12].copy_from_slice(&dentry.attributes.to_le_bytes());
    bytes[12..16].copy_from_slice(&(-1i32).to_le_bytes());
    bytes[16..24].copy_from_slice(&subdir.to_le_bytes());
    for at in [0x28, 0x30, 0x38] {
        bytes[at..at + 8].copy_from_slice(&0x01DB_0000_0000_0000u64.to_le_bytes());
    }
    bytes[0x40..0x54].copy_from_slice(&dentry.hash);
    bytes[0x58..0x60].copy_from_slice(&dentry.tag_or_group.to_le_bytes());
    bytes[0x60..0x62].copy_from_slice(&(dentry.streams.len() as u16).to_le_bytes());
    bytes[0x64..0x66].copy_from_slice(&(name_bytes as u16).to_le_bytes());
    bytes[DENTRY..DENTRY + name_bytes].copy_from_slice(&dentry.name);
    for (name, hash) in &dentry.streams {
        let length = (38 + name.len() + 2).next_multiple_of(8);
        let mut stream = vec![0u8; length];
        stream[..8].copy_from_slice(&(length as u64).to_le_bytes());
        stream[16..36].copy_from_slice(hash);
        stream[36..38].copy_from_slice(&(name.len() as u16).to_le_bytes());
        stream[38..38 + name.len()].copy_from_slice(name);
        bytes.extend(stream);
    }
    bytes
}

struct Flattened {
    dentries: Vec<Dentry>,
    blobs: Vec<Vec<u8>>,
}

fn flatten(node: &Node, out: &mut Flattened) -> usize {
    let mut blob = |data: &[u8]| -> [u8; 20] {
        if data.is_empty() {
            return [0; 20];
        }
        out.blobs.push(data.to_vec());
        sha1(data)
    };
    let dentry = match node {
        Node::Dir { name, children: _ } => Dentry {
            name: utf16(name),
            attributes: 0x10,
            hash: [0; 20],
            tag_or_group: 0,
            streams: Vec::new(),
            children: Some(Vec::new()),
        },
        Node::File {
            name,
            data,
            attributes,
            link_group,
            streams,
        } => {
            let hash = blob(data);
            let streams = streams
                .iter()
                .map(|(name, data)| (utf16(name), blob(data)))
                .collect();
            Dentry {
                name: utf16(name),
                attributes: *attributes,
                hash,
                tag_or_group: *link_group,
                streams,
                children: None,
            }
        }
        Node::Reparse {
            name,
            tag,
            data,
            directory,
        } => {
            let hash = blob(data);
            Dentry {
                name: utf16(name),
                attributes: 0x400 | if *directory { 0x10 } else { 0 },
                hash: [0; 20],
                tag_or_group: u64::from(*tag),
                streams: vec![(Vec::new(), hash), (Vec::new(), [0; 20])],
                children: None,
            }
        }
    };
    let index = out.dentries.len();
    out.dentries.push(dentry);
    if let Node::Dir { children, .. } = node {
        let indices: Vec<usize> = children.iter().map(|child| flatten(child, out)).collect();
        out.dentries[index].children = Some(indices);
    }
    index
}

fn metadata(root: &[Node]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut flat = Flattened {
        dentries: Vec::new(),
        blobs: Vec::new(),
    };
    let top = flatten(&dir("", root.to_vec()), &mut flat);
    let mut meta = vec![8, 0, 0, 0, 0, 0, 0, 0];
    let mut list_of = vec![0u64; flat.dentries.len()];
    let mut order = vec![top];
    let mut lists = Vec::new();
    let mut next = 0;
    while next < order.len() {
        let index = order[next];
        next += 1;
        if let Some(children) = &flat.dentries[index].children
            && !children.is_empty()
        {
            lists.push((index, children.clone()));
            order.extend(children);
        }
    }
    let size_of = |index: usize, flat: &Flattened| dentry_bytes(&flat.dentries[index], 0).len();
    let mut at = 8 + size_of(top, &flat) + 8;
    for (owner, children) in &lists {
        list_of[*owner] = at as u64;
        at += children
            .iter()
            .map(|&child| size_of(child, &flat))
            .sum::<usize>()
            + 8;
    }
    meta.extend(dentry_bytes(&flat.dentries[top], list_of[top]));
    meta.extend([0u8; 8]);
    for (_, children) in &lists {
        for &child in children {
            meta.extend(dentry_bytes(&flat.dentries[child], list_of[child]));
        }
        meta.extend([0u8; 8]);
    }
    (meta, flat.blobs)
}

fn xml(images: usize) -> Vec<u8> {
    let mut text = String::from("<WIM>");
    for index in 1..=images {
        text.push_str(&format!(
            "<IMAGE INDEX=\"{index}\"><NAME>{index}</NAME></IMAGE>"
        ));
    }
    text.push_str("</WIM>");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(utf16(&text));
    bytes
}

/// Writes a WIM with one image per entry of `images`.
pub fn write(images: &[Vec<Node>], options: &Options) -> Vec<u8> {
    let mut out = vec![0u8; HEADER];
    let mut layout = Layout::default();
    let mut table = Vec::new();
    let mut blobs: Vec<(Vec<u8>, u32)> = Vec::new();
    let mut seen: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    let mut metas = Vec::new();
    for (number, image) in images.iter().enumerate() {
        let (mut meta, data) = metadata(image);
        if let Some(hook) = &options.metadata_hook {
            hook(number, &mut meta);
        }
        metas.push(meta);
        for blob in data {
            match seen.get(&blob) {
                Some(&index) => blobs[index].1 += 1,
                None => {
                    seen.insert(blob.clone(), blobs.len());
                    blobs.push((blob, 1));
                }
            }
        }
    }
    let compressed = options.compression == Compression::Xpress;
    let put = |out: &mut Vec<u8>, data: &[u8], metadata: bool| -> [u8; 24] {
        let offset = out.len() as u64;
        let flags = u8::from(metadata) * 2 + u8::from(compressed && !data.is_empty()) * 4;
        let stored = if compressed && !data.is_empty() {
            chunked(data, options.chunk_size as usize)
        } else {
            data.to_vec()
        };
        out.extend(&stored);
        reshdr(stored.len() as u64, flags, offset, data.len() as u64)
    };
    if options.solid {
        let total: Vec<u8> = blobs.iter().flat_map(|(data, _)| data.clone()).collect();
        let offset = out.len();
        let chunk = options.chunk_size as usize;
        let chunks: Vec<&[u8]> = total.chunks(chunk).collect();
        out.extend((total.len() as u64).to_le_bytes());
        out.extend(options.chunk_size.to_le_bytes());
        let method: u32 = if options.chunk_size <= 1 << 16 { 1 } else { 3 };
        out.extend(method.to_le_bytes());
        for piece in &chunks {
            out.extend((piece.len() as u32).to_le_bytes());
        }
        for piece in &chunks {
            out.extend(*piece);
        }
        let packed = (out.len() - offset) as u64;
        table.extend(reshdr(packed, 0x10, offset as u64, 1 << 32));
        table.extend(1u16.to_le_bytes());
        table.extend(1u32.to_le_bytes());
        table.extend([0u8; 20]);
        layout.data.push((offset, packed as usize));
        let mut within = 0u64;
        for (data, references) in &blobs {
            table.extend(reshdr(data.len() as u64, 0x10, within, 0));
            table.extend(1u16.to_le_bytes());
            table.extend(references.to_le_bytes());
            table.extend(sha1(data));
            within += data.len() as u64;
        }
    } else {
        for (data, references) in &blobs {
            let start = out.len();
            table.extend(put(&mut out, data, false));
            layout.data.push((start, out.len() - start));
            table.extend(1u16.to_le_bytes());
            table.extend(references.to_le_bytes());
            table.extend(sha1(data));
        }
    }
    for meta in &metas {
        let start = out.len();
        table.extend(put(&mut out, meta, true));
        layout.metadata.push((start, out.len() - start));
        table.extend(1u16.to_le_bytes());
        table.extend(1u32.to_le_bytes());
        table.extend(sha1(meta));
    }
    layout.blob_table = out.len();
    layout.blob_table_entries = table.len() / 50;
    let table_offset = out.len() as u64;
    out.extend(&table);
    layout.xml = out.len();
    let xml = xml(images.len());
    let xml_offset = out.len() as u64;
    out.extend(&xml);
    out[..8].copy_from_slice(b"MSWIM\0\0\0");
    out[8..12].copy_from_slice(&(HEADER as u32).to_le_bytes());
    let solid_version = options.solid;
    let version: u32 = if solid_version { 0xE00 } else { 0x10D00 };
    out[12..16].copy_from_slice(&version.to_le_bytes());
    let flags: u32 = if compressed || solid_version {
        0x2 | 1 << 17
    } else {
        0
    };
    out[16..20].copy_from_slice(&flags.to_le_bytes());
    out[20..24].copy_from_slice(&options.chunk_size.to_le_bytes());
    out[40..42].copy_from_slice(&1u16.to_le_bytes());
    out[42..44].copy_from_slice(&1u16.to_le_bytes());
    out[44..48].copy_from_slice(&(images.len() as u32).to_le_bytes());
    out[48..72].copy_from_slice(&reshdr(
        table.len() as u64,
        2,
        table_offset,
        table.len() as u64,
    ));
    out[72..96].copy_from_slice(&reshdr(xml.len() as u64, 0, xml_offset, xml.len() as u64));
    if let Some(hook) = &options.file_hook {
        hook(&mut out, &layout);
    }
    out
}

/// Rewrites the hash of every metadata resource after a test changed the
/// stored metadata bytes, so the change reaches the directory parser.
pub fn rehash_metadata(wim: &mut [u8], layout: &Layout) {
    let table = layout.blob_table;
    for entry in 0..layout.blob_table_entries {
        let at = table + entry * 50;
        if wim[at + 7] & 2 == 0 {
            continue;
        }
        let offset = u64::from_le_bytes(wim[at + 8..at + 16].try_into().unwrap()) as usize;
        let size = u64::from_le_bytes(wim[at + 16..at + 24].try_into().unwrap()) as usize;
        let hash = sha1(&wim[offset..offset + size]);
        wim[at + 30..at + 50].copy_from_slice(&hash);
    }
}
