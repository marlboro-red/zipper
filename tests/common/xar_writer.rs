#![allow(dead_code)]
use sha2::Digest;
use std::io::Write;

pub const TIME: &str = "2023-11-14T22:13:20Z";
pub const TIME_SECONDS: u64 = 1_700_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Encoding {
    Stored,
    Zlib,
    Bzip2,
    Xz,
    LzmaAlone,
    LzmaAsXz,
}

impl Encoding {
    pub const ALL: [Encoding; 6] = [
        Encoding::Stored,
        Encoding::Zlib,
        Encoding::Bzip2,
        Encoding::Xz,
        Encoding::LzmaAlone,
        Encoding::LzmaAsXz,
    ];

    fn style(self) -> &'static str {
        match self {
            Encoding::Stored => "application/octet-stream",
            Encoding::Zlib => "application/x-gzip",
            Encoding::Bzip2 => "application/x-bzip2",
            Encoding::Xz => "application/x-xz",
            Encoding::LzmaAlone | Encoding::LzmaAsXz => "application/x-lzma",
        }
    }

    pub fn encode(self, data: &[u8]) -> Vec<u8> {
        match self {
            Encoding::Stored => data.to_vec(),
            Encoding::Zlib => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(data).unwrap();
                encoder.finish().unwrap()
            }
            Encoding::Bzip2 => {
                let mut encoder =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                encoder.write_all(data).unwrap();
                encoder.finish().unwrap()
            }
            Encoding::Xz | Encoding::LzmaAsXz => {
                let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
                encoder.write_all(data).unwrap();
                encoder.finish().unwrap()
            }
            Encoding::LzmaAlone => {
                let options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
                let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
                let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
                encoder.write_all(data).unwrap();
                encoder.finish().unwrap()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sum {
    None,
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Sum {
    pub fn style(self) -> &'static str {
        match self {
            Sum::None => "none",
            Sum::Md5 => "md5",
            Sum::Sha1 => "sha1",
            Sum::Sha256 => "sha256",
            Sum::Sha512 => "sha512",
        }
    }

    pub fn code(self) -> u32 {
        match self {
            Sum::None => 0,
            Sum::Sha1 => 1,
            Sum::Md5 => 2,
            Sum::Sha256 => 3,
            Sum::Sha512 => 4,
        }
    }

    pub fn digest(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Sum::None => Vec::new(),
            Sum::Md5 => md5::Md5::digest(bytes).to_vec(),
            Sum::Sha1 => sha1::Sha1::digest(bytes).to_vec(),
            Sum::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
            Sum::Sha512 => sha2::Sha512::digest(bytes).to_vec(),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub struct Heap {
    pub bytes: Vec<u8>,
    pub toc_sum: Sum,
    pub file_sum: Sum,
}

impl Heap {
    pub fn new(toc_sum: Sum, file_sum: Sum) -> Heap {
        Heap {
            bytes: vec![0; toc_sum.digest(b"").len()],
            toc_sum,
            file_sum,
        }
    }

    pub fn store_data_element(&mut self, encoding: Encoding, data: &[u8]) -> String {
        let archived = encoding.encode(data);
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(&archived);
        self.data_element_at(encoding, offset, &archived, data)
    }

    pub fn data_element_at(
        &self,
        encoding: Encoding,
        offset: usize,
        archived: &[u8],
        data: &[u8],
    ) -> String {
        let sums = if self.file_sum == Sum::None {
            String::new()
        } else {
            format!(
                "<archived-checksum style=\"{style}\">{}</archived-checksum><extracted-checksum style=\"{style}\">{}</extracted-checksum>",
                hex(&self.file_sum.digest(archived)),
                hex(&self.file_sum.digest(data)),
                style = self.file_sum.style()
            )
        };
        format!(
            "<data>{sums}<encoding style=\"{}\"/><size>{}</size><offset>{offset}</offset><length>{}</length></data>",
            encoding.style(),
            data.len(),
            archived.len()
        )
    }
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn file(id: u32, name: &str, mode: &str, data: &str) -> String {
    format!(
        "<file id=\"{id}\"><name>{}</name><type>file</type><mode>{mode}</mode><mtime>{TIME}</mtime>{data}</file>",
        escape(name)
    )
}

pub fn directory(id: u32, name: &str, children: &str) -> String {
    format!(
        "<file id=\"{id}\"><name>{}</name><type>directory</type><mode>0755</mode><mtime>{TIME}</mtime>{children}</file>",
        escape(name)
    )
}

pub fn symlink(id: u32, name: &str, target: &str) -> String {
    format!(
        "<file id=\"{id}\"><name>{}</name><type>symlink</type><link type=\"file\">{}</link><mode>0755</mode></file>",
        escape(name),
        escape(target)
    )
}

pub fn original(id: u32, name: &str, data: &str) -> String {
    format!(
        "<file id=\"{id}\"><name>{}</name><type link=\"original\">hardlink</type><mode>0644</mode>{data}</file>",
        escape(name)
    )
}

pub fn hard_link(id: u32, name: &str, target: u32) -> String {
    format!(
        "<file id=\"{id}\"><name>{}</name><type link=\"{target}\">hardlink</type><mode>0644</mode></file>",
        escape(name)
    )
}

pub fn toc_with_checksum_element(heap: &Heap, files: &str) -> String {
    let checksum = if heap.toc_sum == Sum::None {
        String::new()
    } else {
        format!(
            "<checksum style=\"{}\"><size>{}</size><offset>0</offset></checksum>",
            heap.toc_sum.style(),
            heap.toc_sum.digest(b"").len()
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xar>\n <toc>{checksum}<creation-time>2023-11-14T22:13:20</creation-time>{files}</toc>\n</xar>\n"
    )
}

pub fn zlib(bytes: &[u8]) -> Vec<u8> {
    Encoding::Zlib.encode(bytes)
}

pub fn assemble(toc: &str, mut heap: Heap) -> Vec<u8> {
    let compressed = zlib(toc.as_bytes());
    let code = heap.toc_sum.code();
    assemble_raw(&compressed, toc.len() as u64, &mut heap, code)
}

pub fn assemble_raw(compressed: &[u8], uncompressed: u64, heap: &mut Heap, code: u32) -> Vec<u8> {
    let sum = heap.toc_sum.digest(compressed);
    heap.bytes[..sum.len()].copy_from_slice(&sum);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"xar!");
    bytes.extend_from_slice(&28u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&(compressed.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&uncompressed.to_be_bytes());
    bytes.extend_from_slice(&code.to_be_bytes());
    bytes.extend_from_slice(compressed);
    bytes.extend_from_slice(&heap.bytes);
    bytes
}

pub fn xar(toc_sum: Sum, file_sum: Sum, build: impl FnOnce(&mut Heap) -> String) -> Vec<u8> {
    let mut heap = Heap::new(toc_sum, file_sum);
    let files = build(&mut heap);
    let toc = toc_with_checksum_element(&heap, &files);
    assemble(&toc, heap)
}
