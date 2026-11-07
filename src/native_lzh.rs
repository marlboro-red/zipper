//! In-process extraction of LZH (LHA) archives with header levels 0 to 3 and
//! the -lh0-, -lz4-, -lh4- to -lh7- and -lhd- methods. See
//! `docs/reference/lzh-reader.md` for what it accepts and restores.
use crate::lzh::{self, Lzss, crc16};
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT, MAX_NAME};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufReader, Read, Seek},
    path::Path,
    time::SystemTime,
};

const COMMON: usize = 21;
const SHORT_FIXED: usize = 24;
const LEVEL_1_FIXED: usize = 27;
const LEVEL_2_BASE: usize = 26;
const LEVEL_3_BASE: usize = 32;
const LEVEL_3_SIZE_FIELD: u16 = 4;
const UNIX_AREA: usize = 12;
const MAX_EXTENSIONS: usize = 256;
const MAX_EXTENSION: u32 = 0xFFFF;
const COPY_BUFFER: usize = 1024 * 1024;
const TYPE_MASK: u16 = 0o170000;
const SYMLINK: u16 = 0o120000;
const FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;
const DIRECTORY_SEPARATOR: u8 = 0xFF;
const OS_9_68K: u8 = b'K';
const HEADER_CRC: u8 = 0x00;
const FILE_NAME: u8 = 0x01;
const DIRECTORY_NAME: u8 = 0x02;
const MULTI_DISK: u8 = 0x39;
const LARGE_SIZES: u8 = 0x42;
const UNIX_MODE: u8 = 0x50;
const UNIX_TIME: u8 = 0x54;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Stored,
    Lzss(Lzss),
    Directory,
}

// SPEC(LHA header.doc, methods): -lh4- to -lh7- use 4, 8, 32 and 64 KiB windows.
fn method(id: &[u8; 5]) -> Field<Method> {
    Ok(match id {
        b"-lh0-" | b"-lz4-" => Method::Stored,
        b"-lhd-" => Method::Directory,
        [b'-', b'l', b'h', digit @ b'4'..=b'7', b'-'] => Method::Lzss(Lzss {
            dictionary: match digit {
                b'4' => 1 << 12,
                b'5' => 1 << 13,
                b'6' => 1 << 15,
                _ => 1 << 16,
            },
        }),
        _ => return Err(BadField("LZH method is not one Zipper reads")),
    })
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Common {
    method: [u8; 5],
    packed: u32,
    size: u32,
    time: u32,
    level: u8,
}

// SPEC(LHa for UNIX header.c, level 0 to 3): the first 21 bytes share one layout.
fn common(bytes: &[u8; COMMON]) -> Common {
    Common {
        method: bytes[2..7].try_into().unwrap(),
        packed: u32_at(bytes, 7),
        size: u32_at(bytes, 11),
        time: u32_at(bytes, 15),
        level: bytes[20],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Short {
    name_end: usize,
    crc: u16,
    unix: Option<(u32, u16)>,
    next: u16,
}

// SPEC(LHA header.doc, level 0 and 1): a size, a sum, the common fields, a
// name and the CRC; level 1 ends with the OS and the first extension's size.
fn short_header(header: &[u8]) -> Field<Short> {
    field_check(
        header.len() >= SHORT_FIXED && header.len() == usize::from(header[0]) + 2,
        "LZH header is shorter than its fixed fields",
    )?;
    let sum = header[2..]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    field_check(sum == header[1], "LZH header checksum does not match")?;
    let name_end = 22 + usize::from(header[21]);
    field_check(
        name_end + 2 <= header.len(),
        "LZH name runs past its header",
    )?;
    let crc = u16_at(header, name_end);
    let rest = &header[name_end + 2..];
    Ok(match header[20] {
        0 => Short {
            name_end,
            crc,
            unix: (rest.len() >= UNIX_AREA && rest[0] == b'U')
                .then(|| (u32_at(rest, 2), u16_at(rest, 6))),
            next: 0,
        },
        _ => {
            field_check(
                header.len() == name_end - 22 + LEVEL_1_FIXED,
                "LZH level 1 header has bytes after its OS byte",
            )?;
            Short {
                name_end,
                crc,
                unix: None,
                next: u16_at(header, header.len() - 2),
            }
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Long {
    total: u32,
    crc: u16,
    next: u32,
}

// SPEC(LHA header.doc, level 2): a 2-byte total size, the common fields, the
// CRC, the OS and the size of the first extended header.
fn level_2(bytes: &[u8; LEVEL_2_BASE]) -> Field<Long> {
    let written = u32::from(u16_at(bytes, 0));
    // EXTERNAL(LHA for OS-9/68k): it leaves the 2-byte size field out of the
    // total, and Lhasa adds it back for headers whose OS byte is `K`.
    let total = if bytes[23] == OS_9_68K {
        written + 2
    } else {
        written
    };
    field_check(
        total >= LEVEL_2_BASE as u32,
        "LZH level 2 header is shorter than its fixed fields",
    )?;
    Ok(Long {
        total,
        crc: u16_at(bytes, 21),
        next: u32::from(u16_at(bytes, 24)),
    })
}

// SPEC(LHa for UNIX header.c, level 3): a size field width of 4, the common
// fields, the CRC, the OS, a 4-byte total size and first extension size.
fn level_3(bytes: &[u8; LEVEL_3_BASE]) -> Field<Long> {
    field_check(
        u16_at(bytes, 0) == LEVEL_3_SIZE_FIELD,
        "LZH level 3 header does not use 4-byte sizes",
    )?;
    let total = u32_at(bytes, 24);
    field_check(
        total >= LEVEL_3_BASE as u32,
        "LZH level 3 header is shorter than its fixed fields",
    )?;
    Ok(Long {
        total,
        crc: u16_at(bytes, 21),
        next: u32_at(bytes, 28),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Extension<'a> {
    HeaderCrc(u16),
    Name(&'a [u8]),
    Directory(&'a [u8]),
    Mode(u16),
    Time(u32),
    Other,
}

// SPEC(LHa for UNIX header.c, extended header): a type byte, its contents and the
// size of the next extended header, 2 bytes wide or 4 at level 3.
fn extension(block: &[u8], size_field: usize) -> Field<(Extension<'_>, u32)> {
    field_check(
        block.len() > size_field,
        "LZH extended header is shorter than its fields",
    )?;
    let (head, size) = block.split_at(block.len() - size_field);
    let next = if size_field == 2 {
        u32::from(u16_at(size, 0))
    } else {
        u32_at(size, 0)
    };
    let contents = &head[1..];
    let at_least = |count: usize| {
        field_check(
            contents.len() >= count,
            "LZH extended header is shorter than its value",
        )
    };
    let found = match head[0] {
        HEADER_CRC => {
            at_least(2)?;
            Extension::HeaderCrc(u16_at(contents, 0))
        }
        FILE_NAME => Extension::Name(contents),
        DIRECTORY_NAME => Extension::Directory(contents),
        UNIX_MODE => {
            at_least(2)?;
            Extension::Mode(u16_at(contents, 0))
        }
        UNIX_TIME => {
            at_least(4)?;
            Extension::Time(u32_at(contents, 0))
        }
        LARGE_SIZES => return Err(BadField("LZH 64-bit sizes are not supported")),
        MULTI_DISK => {
            return Err(BadField(
                "LZH archives split across disks are not supported",
            ));
        }
        _ => Extension::Other,
    };
    Ok((found, next))
}

fn until_nul(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|&byte| byte == 0)
        .map_or(bytes, |end| &bytes[..end])
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    File(Method),
    Directory,
    Symlink(String),
}

#[derive(Debug)]
struct Item {
    name: String,
    kind: Kind,
    data: u64,
    packed: u64,
    size: u64,
    crc: u16,
    mode: Option<u16>,
    modified: Option<SystemTime>,
}

#[derive(Default)]
struct Names {
    file: Option<Vec<u8>>,
    directory: Option<Vec<u8>>,
    mode: Option<u16>,
    time: Option<u32>,
    header_crc: Option<u16>,
}

impl Names {
    fn take(&mut self, found: Extension) -> Result<()> {
        let short = |bytes: &[u8]| {
            let bytes = until_nul(bytes);
            ensure!(
                bytes.len() as u64 <= MAX_NAME,
                "LZH name is longer than {MAX_NAME} bytes"
            );
            Ok(bytes.to_vec())
        };
        match found {
            Extension::Name(name) => {
                ensure!(self.file.is_none(), "LZH entry has two file names");
                self.file = Some(short(name)?);
            }
            Extension::Directory(name) => {
                ensure!(self.directory.is_none(), "LZH entry has two folder names");
                self.directory = Some(short(name)?);
            }
            Extension::HeaderCrc(crc) => {
                ensure!(self.header_crc.is_none(), "LZH entry has two header CRCs");
                self.header_crc = Some(crc);
            }
            Extension::Mode(mode) => self.mode = Some(mode),
            Extension::Time(time) => self.time = Some(time),
            Extension::Other => {}
        }
        Ok(())
    }

    // EXTERNAL(7-Zip LzhHandler): the folder name, 0xFF between folders, joins the
    // file name, `\` separates folders everywhere, and one trailing separator goes.
    fn full_name(&self, basic: &[u8]) -> Result<String> {
        let mut name: Vec<u8> = self
            .directory
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|&byte| {
                if byte == DIRECTORY_SEPARATOR {
                    b'/'
                } else {
                    byte
                }
            })
            .collect();
        if name
            .last()
            .is_some_and(|&last| last != b'/' && last != b'\\')
        {
            name.push(b'/');
        }
        name.extend_from_slice(self.file.as_deref().unwrap_or(until_nul(basic)));
        ensure!(
            name.len() as u64 <= MAX_NAME,
            "LZH name is longer than {MAX_NAME} bytes"
        );
        for byte in &mut name {
            if *byte == b'\\' {
                *byte = b'/';
            }
        }
        if name.last() == Some(&b'/') {
            name.pop();
        }
        String::from_utf8(name)
            .map_err(|_| anyhow::anyhow!("LZH name is not UTF-8; {BACKEND_HINT}"))
    }
}

fn unix_time(seconds: u32) -> Option<SystemTime> {
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(u64::from(seconds)))
}

struct Entries {
    reader: BufReader<ArchiveFile>,
    length: u64,
    next: u64,
    block: Vec<u8>,
}

impl Entries {
    fn open(path: &Path) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        Ok(Entries {
            reader: BufReader::with_capacity(COPY_BUFFER, file),
            length,
            next: 0,
            block: Vec::new(),
        })
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step = i64::try_from(i128::from(offset) - i128::from(here))
            .context("LZH entry offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    fn exact(&mut self, buffer: &mut [u8], what: &str) -> Result<()> {
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated LZH {what}"))
    }

    fn extensions(
        &mut self,
        mut next: u32,
        size_field: usize,
        names: &mut Names,
        crc: &mut u16,
    ) -> Result<u64> {
        let mut total = 0u64;
        let mut count = 0;
        while next != 0 {
            count += 1;
            ensure!(
                count <= MAX_EXTENSIONS,
                "LZH entry has more than {MAX_EXTENSIONS} extended headers"
            );
            ensure!(
                next <= MAX_EXTENSION,
                "LZH extended header is larger than 64 KiB"
            );
            let mut block = std::mem::take(&mut self.block);
            block.resize(next as usize, 0);
            self.exact(&mut block, "extended header")?;
            total += u64::from(next);
            let (found, following) = extension(&block, size_field)?;
            names.take(found)?;
            if matches!(found, Extension::HeaderCrc(_)) {
                block[1] = 0;
                block[2] = 0;
            }
            *crc = crc16(*crc, &block);
            self.block = block;
            next = following;
        }
        Ok(total)
    }

    fn next(&mut self) -> Result<Option<Item>> {
        self.seek_to(self.next)?;
        if self.next == self.length {
            return Ok(None);
        }
        let mut head = [0u8; LEVEL_3_BASE];
        self.exact(&mut head[..1], "header")?;
        if head[0] == 0 {
            return Ok(None);
        }
        self.exact(&mut head[1..COMMON], "header")?;
        let fields = common(head[..COMMON].try_into().unwrap());
        let found = method(&fields.method).map_err(|_| {
            anyhow::anyhow!(
                "LZH method {} is not one Zipper reads",
                fields.method.escape_ascii()
            )
        })?;
        let mut names = Names::default();
        let mut crc = 0;
        let (basic, file_crc, packed, modified) = match fields.level {
            0 | 1 => {
                let mut header = [0u8; 257];
                let total = usize::from(head[0]) + 2;
                ensure!(
                    total >= SHORT_FIXED,
                    "LZH header is shorter than its fixed fields"
                );
                header[..COMMON].copy_from_slice(&head[..COMMON]);
                self.exact(&mut header[COMMON..total], "header")?;
                let short = short_header(&header[..total])?;
                if let Some((time, mode)) = short.unix {
                    names.time = Some(time);
                    names.mode = Some(mode);
                }
                let extended = self.extensions(u32::from(short.next), 2, &mut names, &mut crc)?;
                let packed = u64::from(fields.packed)
                    .checked_sub(extended)
                    .context("LZH extended headers are larger than the entry")?;
                let basic = header[22..short.name_end].to_vec();
                let time = match (names.time, fields.level) {
                    (Some(time), _) => unix_time(time),
                    _ => crate::dos_local_time((fields.time >> 16) as u16, fields.time as u16),
                };
                (basic, short.crc, packed, time)
            }
            2 | 3 => {
                let (long, size_field, base) = if fields.level == 2 {
                    self.exact(&mut head[COMMON..LEVEL_2_BASE], "header")?;
                    (
                        level_2(head[..LEVEL_2_BASE].try_into().unwrap())?,
                        2,
                        LEVEL_2_BASE,
                    )
                } else {
                    self.exact(&mut head[COMMON..LEVEL_3_BASE], "header")?;
                    (level_3(&head)?, 4, LEVEL_3_BASE)
                };
                crc = crc16(0, &head[..base]);
                let extended = self.extensions(long.next, size_field, &mut names, &mut crc)?;
                let padding = u64::from(long.total)
                    .checked_sub(base as u64 + extended)
                    .context("LZH extended headers pass the header size")?;
                // SPEC(LHa for UNIX header.c): a level 2 header pads its size
                // by one byte when its low byte would be zero; level 3 never pads.
                ensure!(
                    padding == 0 || (fields.level == 2 && padding == 1),
                    "LZH header size leaves {padding} bytes after its extended headers"
                );
                if padding == 1 {
                    let mut pad = [0u8; 1];
                    self.exact(&mut pad, "header")?;
                    crc = crc16(crc, &pad);
                }
                if let Some(stored) = names.header_crc {
                    ensure!(stored == crc, "LZH header CRC does not match");
                }
                let time = names.time.unwrap_or(fields.time);
                (
                    Vec::new(),
                    long.crc,
                    u64::from(fields.packed),
                    unix_time(time),
                )
            }
            level => bail!("LZH header level {level} is not supported"),
        };
        let data = self.reader.stream_position()?;
        let end = data
            .checked_add(packed)
            .filter(|&end| end <= self.length)
            .context("truncated LZH entry")?;
        self.next = end;
        let mut name = names.full_name(&basic)?;
        let size = u64::from(fields.size);
        let kind = match found {
            Method::Directory => {
                ensure!(
                    packed == 0 && size == 0,
                    "LZH folder entry {name:?} carries data"
                );
                if names.mode.is_some_and(|mode| mode & TYPE_MASK == SYMLINK) {
                    let (link, target) = name
                        .split_once('|')
                        .with_context(|| format!("LZH symbolic link {name:?} names no target"))?;
                    ensure!(
                        !link.is_empty() && !target.is_empty(),
                        "LZH symbolic link {name:?} has an empty name or target"
                    );
                    let kind = Kind::Symlink(target.to_owned());
                    name.truncate(link.len());
                    kind
                } else {
                    Kind::Directory
                }
            }
            Method::Stored => {
                ensure!(
                    packed == size,
                    "LZH stored entry {name:?} packs {packed} bytes for {size}"
                );
                Kind::File(found)
            }
            Method::Lzss(_) => Kind::File(found),
        };
        Ok(Some(Item {
            name,
            kind,
            data,
            packed,
            size,
            crc: file_crc,
            mode: names.mode,
            modified,
        }))
    }
}

enum Data<'a> {
    Stored(std::io::Take<&'a mut BufReader<ArchiveFile>>),
    Lzss(Box<lzh::Decoder<std::io::Take<&'a mut BufReader<ArchiveFile>>>>),
}

struct Checked<'a> {
    data: Data<'a>,
    crc: u16,
    read: u64,
}

impl Read for Checked<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = match &mut self.data {
            Data::Stored(data) => data.read(buffer)?,
            Data::Lzss(data) => data.read(buffer)?,
        };
        self.crc = crc16(self.crc, &buffer[..read]);
        self.read += read as u64;
        Ok(read)
    }
}

impl Checked<'_> {
    fn finish(mut self, item: &Item) -> Result<()> {
        if let Data::Lzss(decoder) = &mut self.data {
            decoder
                .finish()
                .with_context(|| format!("LZH entry {:?} is damaged", item.name))?;
        }
        ensure!(
            self.read == item.size,
            "truncated LZH entry {:?}",
            item.name
        );
        ensure!(
            self.crc == item.crc,
            "LZH entry {:?} fails its CRC-16 check",
            item.name
        );
        Ok(())
    }
}

fn parse(tree: &mut tree::Tree, entries: &mut Entries) -> Result<()> {
    while let Some(item) = entries.next()? {
        tree.check()?;
        let permissions = item.mode.map(|mode| u32::from(mode) & 0o777);
        match &item.kind {
            Kind::Directory => tree.directory(
                &item.name,
                permissions.unwrap_or(DIRECTORY_MODE),
                item.modified,
            )?,
            Kind::Symlink(target) => tree.symlink(&item.name, target.clone())?,
            Kind::File(method) => {
                entries.seek_to(item.data)?;
                let raw = (&mut entries.reader).take(item.packed);
                let data = match method {
                    Method::Lzss(lzss) => Data::Lzss(Box::new(lzh::Decoder::new(
                        raw,
                        item.packed,
                        item.size,
                        *lzss,
                    ))),
                    _ => Data::Stored(raw),
                };
                let mut checked = Checked {
                    data,
                    crc: 0,
                    read: 0,
                };
                tree.file(
                    &item.name,
                    item.size,
                    permissions.unwrap_or(FILE_MODE),
                    item.modified,
                    &mut checked,
                )?;
                checked.finish(&item)?;
            }
        }
    }
    Ok(())
}

/// Returns whether the file starts with an LZH entry header: a method such
/// as `-lh5-` at offset 2 and a header level of 0 to 3.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut head = Vec::with_capacity(COMMON);
    crate::volumes::open(path)?
        .take(COMMON as u64)
        .read_to_end(&mut head)?;
    Ok(head.len() == COMMON
        && head[0] != 0
        && matches!(&head[2..7], [b'-', b'l', b'h' | b'z', _, b'-'])
        && head[20] <= 3)
}

/// Verify and optionally extract an LZH archive into a new directory. It
/// reads header levels 0 to 3 and the stored, -lh4- to -lh7- and folder
/// methods, checks each file's CRC-16 and each header's checksum or CRC,
/// and restores Unix modes, times and symbolic links where the headers
/// carry them.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut entries = Entries::open(path)?;
    tree::extract("LZH", path, destination, options, |tree| {
        parse(tree, &mut entries)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn short(level: u8, name: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut header = vec![0, 0];
        header.extend_from_slice(b"-lh0-");
        header.extend_from_slice(&[0; 12]);
        header.extend_from_slice(&[0x20, level, name.len() as u8]);
        header.extend_from_slice(name);
        header.extend_from_slice(&[0x34, 0x12]);
        header.extend_from_slice(tail);
        header[0] = (header.len() - 2) as u8;
        header[1] = header[2..].iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
        header
    }

    #[test]
    fn short_headers_read_their_name_crc_and_unix_area() {
        let header = short(0, b"a.txt", b"");
        let read = short_header(&header).unwrap();
        assert_eq!((read.name_end, read.crc, read.unix), (27, 0x1234, None));
        let header = short(0, b"a", b"U\0\x01\0\0\0\xed\x41\0\0\0\0");
        assert_eq!(short_header(&header).unwrap().unix, Some((1, 0o40755)));
        let header = short(1, b"a", b"U\x05\x00");
        assert_eq!(short_header(&header).unwrap().next, 5);
        let mut bad = header.clone();
        bad[1] ^= 1;
        assert!(short_header(&bad).is_err());
        assert!(short_header(&short(1, b"a", b"U\x05\x00\x00")).is_err());
        let mut long_name = short(0, b"ab", b"");
        long_name[21] = 9;
        assert!(short_header(&long_name).is_err());
    }

    #[test]
    fn extensions_read_each_known_type_and_refuse_short_values() {
        assert_eq!(
            extension(b"\x01name\x05\x00", 2).unwrap(),
            (Extension::Name(b"name"), 5)
        );
        assert_eq!(
            extension(b"\x50\xa4\x81\x00\x00\x00\x00", 4).unwrap(),
            (Extension::Mode(0o100644), 0)
        );
        assert_eq!(
            extension(b"\x54\x01\x00\x00\x00\x00\x00", 2).unwrap(),
            (Extension::Time(1), 0)
        );
        assert!(extension(b"\x54\x01\x00\x00\x00", 2).is_err());
        assert!(extension(b"\x00\x01\x00\x00", 2).is_err());
        assert!(extension(b"\x42\x00\x00", 2).is_err());
        assert!(extension(b"\x01\x00", 2).is_err());
    }

    #[test]
    fn methods_map_to_their_windows() {
        assert_eq!(method(b"-lh0-").unwrap(), Method::Stored);
        assert_eq!(method(b"-lz4-").unwrap(), Method::Stored);
        assert_eq!(method(b"-lhd-").unwrap(), Method::Directory);
        for (id, bits) in [
            (b"-lh4-", 12),
            (b"-lh5-", 13),
            (b"-lh6-", 15),
            (b"-lh7-", 16),
        ] {
            assert_eq!(
                method(id).unwrap(),
                Method::Lzss(Lzss {
                    dictionary: 1 << bits
                })
            );
        }
        for id in [
            b"-lh1-", b"-lh2-", b"-lh3-", b"-lzs-", b"-lz5-", b"-lhx-", b"-pm2-",
        ] {
            assert!(method(id).is_err(), "{id:?}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const LONGEST: usize = 30;

    #[kani::proof]
    #[kani::unwind(31)]
    fn short_headers_never_panic_and_keep_the_name_and_crc_inside() {
        let bytes: [u8; LONGEST] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= LONGEST);
        if let Ok(short) = short_header(&bytes[..length]) {
            assert!(short.name_end + 2 <= length);
            assert!(short.name_end >= 22);
            if bytes[20] != 0 {
                assert_eq!(length, short.name_end + 5);
                assert!(short.unix.is_none());
            }
        }
    }

    #[kani::proof]
    fn long_headers_never_panic_and_cover_their_fixed_fields() {
        let level_two: [u8; LEVEL_2_BASE] = kani::any();
        if let Ok(long) = level_2(&level_two) {
            assert!(long.total >= LEVEL_2_BASE as u32 && long.total <= 0xFFFF + 2);
            assert!(long.next <= 0xFFFF);
        }
        let level_three: [u8; LEVEL_3_BASE] = kani::any();
        if let Ok(long) = level_3(&level_three) {
            assert!(long.total >= LEVEL_3_BASE as u32);
        }
        let _ = common(level_three[..COMMON].try_into().unwrap());
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn extensions_never_panic_and_their_values_lie_inside() {
        let bytes: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= 8);
        let wide: bool = kani::any();
        let size_field = if wide { 4 } else { 2 };
        if let Ok((found, next)) = extension(&bytes[..length], size_field) {
            assert!(length > size_field);
            if !wide {
                assert!(next <= 0xFFFF);
            }
            match found {
                Extension::Name(name) | Extension::Directory(name) => {
                    assert_eq!(name.len(), length - 1 - size_field)
                }
                Extension::Time(_) => assert!(length >= 5 + size_field),
                Extension::Mode(_) | Extension::HeaderCrc(_) => {
                    assert!(length >= 3 + size_field)
                }
                Extension::Other => {}
            }
        }
    }

    #[kani::proof]
    fn every_method_id_maps_to_a_window_the_decoder_takes() {
        let id: [u8; 5] = kani::any();
        if let Ok(Method::Lzss(lzss)) = method(&id) {
            assert!(lzss.dictionary >= 1 << 12 && lzss.dictionary <= 1 << 16);
            assert!(lzss.dictionary.is_power_of_two());
        }
    }
}
