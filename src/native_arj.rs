//! In-process extraction of ARJ archives with methods 0 to 4. See
//! `docs/reference/arj-reader.md` for what it accepts and restores.
use crate::lzh::{self, Bits, Lzss, Window};
pub use crate::tree::ReadOptions;
use crate::tree::{self, MAX_NAME};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, BufReader, Read, Seek},
    path::Path,
};

const SIGNATURE: [u8; 2] = [0x60, 0xEA];
const SMALLEST_HEADER: usize = 30;
const LARGEST_HEADER: usize = 2600;
const CRC: usize = 4;
const COPY_BUFFER: usize = 1024 * 1024;
const GARBLED: u8 = 0x01;
const CONTINUES_IN_NEXT_VOLUME: u8 = 0x04;
const CONTINUES_FROM_PREVIOUS_VOLUME: u8 = 0x08;
const MAIN_HEADER_KIND: u8 = 2;
const HOST_MS_DOS: u8 = 0;
const HOST_UNIX: u8 = 2;
const HOST_WINDOWS_95: u8 = 10;
const READ_ONLY: u16 = 0x01;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;
// EXTERNAL(7-Zip ArjHandler): methods 1 to 3 decode with a 26,624-byte history.
const LZSS: Lzss = Lzss { dictionary: 26_624 };
const FASTEST_WINDOW: u32 = 1 << 15;
const SHORTEST_MATCH: u32 = 3;
const MORE_LENGTH_ONES: u32 = 6;
const DISTANCE_BASE_BITS: u32 = 9;
const MORE_DISTANCE_ONES: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Stored,
    Lzss,
    Fastest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    MainHeader,
    Other(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    host: u8,
    flags: u8,
    method: u8,
    kind: Kind,
    time: u32,
    packed: u32,
    size: u32,
    crc: u32,
    mode: u16,
    name: std::ops::Range<usize>,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn string_end(block: &[u8], start: usize) -> Field<usize> {
    block[start..]
        .iter()
        .position(|&byte| byte == 0)
        .map(|end| start + end)
        .ok_or(BadField("ARJ name or comment has no terminator"))
}

// SPEC(ARJ technical notes, basic header): 30 or more bytes of fixed fields,
// then a name and a comment that each end with a zero byte.
fn header(block: &[u8]) -> Field<Header> {
    field_check(
        (SMALLEST_HEADER..=LARGEST_HEADER).contains(&block.len()),
        "ARJ header size is outside 30 to 2600 bytes",
    )?;
    let first = usize::from(block[0]);
    field_check(
        (SMALLEST_HEADER..=block.len()).contains(&first),
        "ARJ first header size is outside its header",
    )?;
    let name_end = string_end(block, first)?;
    string_end(block, name_end + 1)?;
    Ok(Header {
        host: block[3],
        flags: block[4],
        method: block[5],
        kind: match block[6] {
            0 | 1 => Kind::File,
            3 => Kind::Directory,
            MAIN_HEADER_KIND => Kind::MainHeader,
            other => Kind::Other(other),
        },
        time: u32_at(block, 8),
        packed: u32_at(block, 12),
        size: u32_at(block, 16),
        crc: u32_at(block, 20),
        mode: u16_at(block, 26),
        name: first..name_end,
    })
}

fn method(number: u8) -> Field<Method> {
    Ok(match number {
        0 => Method::Stored,
        1..=3 => Method::Lzss,
        4 => Method::Fastest,
        _ => return Err(BadField("ARJ method is not one Zipper reads")),
    })
}

fn file_mode(header: &Header) -> u32 {
    let default = if header.kind == Kind::Directory {
        DIRECTORY_MODE
    } else {
        FILE_MODE
    };
    match header.host {
        HOST_UNIX => u32::from(header.mode) & 0o777,
        HOST_MS_DOS | HOST_WINDOWS_95
            if header.mode & READ_ONLY != 0 && header.kind != Kind::Directory =>
        {
            READ_ONLY_MODE
        }
        _ => default,
    }
}

struct Fastest<R> {
    bits: Bits<R>,
    window: Window,
    remaining: u64,
    pending: Option<(u32, u32)>,
    finished: bool,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl<R: Read> Fastest<R> {
    fn new(inner: R, packed: u64, size: u64) -> Self {
        Fastest {
            bits: Bits::new(inner, packed),
            window: Window::new(FASTEST_WINDOW),
            remaining: size,
            pending: None,
            finished: false,
        }
    }

    fn leading_ones(&mut self, most: u32) -> io::Result<u32> {
        let mut ones = 0;
        while ones < most && self.bits.bits(1)? == 1 {
            ones += 1;
        }
        Ok(ones)
    }

    fn match_length(&mut self) -> io::Result<u32> {
        let width = 1 + self.leading_ones(MORE_LENGTH_ONES)?;
        Ok((1 << width) - 1 + SHORTEST_MATCH - 1 + self.bits.bits(width)?)
    }

    fn match_distance(&mut self) -> io::Result<u32> {
        let width = DISTANCE_BASE_BITS + self.leading_ones(MORE_DISTANCE_ONES)?;
        Ok((1 << width) - (1 << DISTANCE_BASE_BITS) + self.bits.bits(width)?)
    }

    fn next(&mut self) -> io::Result<Option<u8>> {
        if self.bits.bits(1)? == 0 {
            self.remaining -= 1;
            return Ok(Some(self.window.put(self.bits.bits(8)? as u8)));
        }
        let length = self.match_length()?;
        let distance = self.match_distance()?;
        if u64::from(length) > self.remaining {
            return Err(invalid("ARJ match runs past the declared size"));
        }
        if !self.window.reaches(distance) {
            return Err(invalid("ARJ match reaches before its window"));
        }
        self.remaining -= u64::from(length);
        self.pending = Some((distance, length));
        Ok(None)
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.remaining != 0 || self.pending.is_some() {
            return Err(invalid("ARJ stream is shorter than its declared size"));
        }
        self.bits.finish()?;
        self.finished = true;
        Ok(())
    }
}

impl<R: Read> Read for Fastest<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < output.len() {
            if let Some((distance, left)) = &mut self.pending {
                let run = (*left as usize).min(output.len() - written);
                for byte in &mut output[written..written + run] {
                    *byte = self.window.repeat(*distance);
                }
                written += run;
                *left -= run as u32;
                if *left == 0 {
                    self.pending = None;
                }
                continue;
            }
            if self.remaining == 0 {
                break;
            }
            if let Some(byte) = self.next()? {
                output[written] = byte;
                written += 1;
            }
        }
        if self.remaining == 0 && self.pending.is_none() {
            self.finish()?;
        }
        Ok(written)
    }
}

struct Item {
    name: String,
    header: Header,
    method: Method,
    data: u64,
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
        let mut entries = Entries {
            reader: BufReader::with_capacity(COPY_BUFFER, file),
            length,
            next: 0,
            block: Vec::new(),
        };
        let main = entries.basic()?.context("ARJ archive has no main header")?;
        let main = header(&main)?;
        ensure!(
            main.kind == Kind::MainHeader,
            "ARJ main header has file type {:?}",
            main.kind
        );
        ensure!(
            main.flags & CONTINUES_IN_NEXT_VOLUME == 0,
            "multi-volume ARJ archives are not supported"
        );
        entries.skip_extended()?;
        entries.next = entries.reader.stream_position()?;
        Ok(entries)
    }

    fn exact(&mut self, buffer: &mut [u8], what: &str) -> Result<()> {
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated ARJ {what}"))
    }

    fn checked_block(&mut self, size: usize, what: &str) -> Result<Vec<u8>> {
        let mut block = std::mem::take(&mut self.block);
        block.resize(size + CRC, 0);
        self.exact(&mut block, what)?;
        let stored = u32_at(&block, size);
        block.truncate(size);
        ensure!(
            crc32fast::hash(&block) == stored,
            "ARJ {what} fails its CRC-32 check"
        );
        Ok(block)
    }

    fn basic(&mut self) -> Result<Option<Vec<u8>>> {
        let mut start = [0u8; 4];
        self.exact(&mut start, "header")?;
        ensure!(start[..2] == SIGNATURE, "ARJ header has no signature");
        let size = usize::from(u16_at(&start, 2));
        if size == 0 {
            return Ok(None);
        }
        ensure!(
            (SMALLEST_HEADER..=LARGEST_HEADER).contains(&size),
            "ARJ header size {size} is outside 30 to 2600 bytes"
        );
        self.checked_block(size, "header").map(Some)
    }

    fn skip_extended(&mut self) -> Result<()> {
        loop {
            let mut size = [0u8; 2];
            self.exact(&mut size, "extended header")?;
            let size = usize::from(u16::from_le_bytes(size));
            if size == 0 {
                return Ok(());
            }
            let block = self.checked_block(size, "extended header")?;
            self.block = block;
        }
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step = i64::try_from(i128::from(offset) - i128::from(here))
            .context("ARJ entry offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Item>> {
        self.seek_to(self.next)?;
        let Some(block) = self.basic()? else {
            return Ok(None);
        };
        let found = header(&block)?;
        let name = block[found.name.clone()].to_vec();
        self.block = block;
        self.skip_extended()?;
        ensure!(
            found.flags & GARBLED == 0,
            "ARJ entry is encrypted, which Zipper does not read"
        );
        ensure!(
            found.flags & (CONTINUES_IN_NEXT_VOLUME | CONTINUES_FROM_PREVIOUS_VOLUME) == 0,
            "ARJ entry spans volumes, which Zipper does not read"
        );
        ensure!(
            name.len() as u64 <= MAX_NAME,
            "ARJ name is longer than {MAX_NAME} bytes"
        );
        let mut name =
            String::from_utf8(name).map_err(|_| anyhow::anyhow!("ARJ name is not UTF-8"))?;
        // SPEC(ARJ technical notes, PATHSYM_FLAG): an untranslated name keeps MS-DOS's `\`.
        name = name.replace('\\', "/");
        let data = self.reader.stream_position()?;
        let end = data
            .checked_add(u64::from(found.packed))
            .filter(|&end| end <= self.length)
            .context("truncated ARJ entry")?;
        self.next = end;
        let method = match found.kind {
            Kind::File => method(found.method)?,
            Kind::Directory => {
                ensure!(
                    found.packed == 0 && found.size == 0,
                    "ARJ folder entry {name:?} carries data"
                );
                Method::Stored
            }
            other => anyhow::bail!(
                "ARJ entry {name:?} has file type {other:?}, which Zipper does not read"
            ),
        };
        if method == Method::Stored {
            ensure!(
                found.packed == found.size,
                "ARJ stored entry {name:?} packs {} bytes for {}",
                found.packed,
                found.size
            );
        }
        Ok(Some(Item {
            name,
            header: found,
            method,
            data,
        }))
    }
}

type Raw<'a> = std::io::Take<&'a mut BufReader<ArchiveFile>>;

enum Data<'a> {
    Stored(Raw<'a>),
    Lzss(Box<lzh::Decoder<Raw<'a>>>),
    Fastest(Box<Fastest<Raw<'a>>>),
}

struct Checked<'a> {
    data: Data<'a>,
    crc: crc32fast::Hasher,
    read: u64,
}

impl Read for Checked<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = match &mut self.data {
            Data::Stored(data) => data.read(buffer)?,
            Data::Lzss(data) => data.read(buffer)?,
            Data::Fastest(data) => data.read(buffer)?,
        };
        self.crc.update(&buffer[..read]);
        self.read += read as u64;
        Ok(read)
    }
}

impl Checked<'_> {
    fn finish(mut self, item: &Item) -> Result<()> {
        let ended = match &mut self.data {
            Data::Stored(_) => Ok(()),
            Data::Lzss(decoder) => decoder.finish(),
            Data::Fastest(decoder) => decoder.finish(),
        };
        ended.with_context(|| format!("ARJ entry {:?} is damaged", item.name))?;
        ensure!(
            self.read == u64::from(item.header.size),
            "truncated ARJ entry {:?}",
            item.name
        );
        ensure!(
            self.crc.finalize() == item.header.crc,
            "ARJ entry {:?} fails its CRC-32 check",
            item.name
        );
        Ok(())
    }
}

fn parse(tree: &mut tree::Tree, entries: &mut Entries) -> Result<()> {
    while let Some(item) = entries.next()? {
        tree.check()?;
        let mode = file_mode(&item.header);
        let modified =
            crate::dos_local_time((item.header.time >> 16) as u16, item.header.time as u16);
        if item.header.kind == Kind::Directory {
            tree.directory(&item.name, mode, modified)?;
            continue;
        }
        entries.seek_to(item.data)?;
        let packed = u64::from(item.header.packed);
        let size = u64::from(item.header.size);
        let raw = (&mut entries.reader).take(packed);
        let data = match item.method {
            Method::Stored => Data::Stored(raw),
            Method::Lzss => Data::Lzss(Box::new(lzh::Decoder::new(raw, packed, size, LZSS))),
            Method::Fastest => Data::Fastest(Box::new(Fastest::new(raw, packed, size))),
        };
        let mut checked = Checked {
            data,
            crc: crc32fast::Hasher::new(),
            read: 0,
        };
        tree.file(&item.name, size, mode, modified, &mut checked)?;
        checked.finish(&item)?;
    }
    Ok(())
}

/// Returns whether the file starts with an ARJ main header whose CRC-32
/// matches.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut head = Vec::with_capacity(4 + LARGEST_HEADER + CRC);
    crate::volumes::open(path)?
        .take((4 + LARGEST_HEADER + CRC) as u64)
        .read_to_end(&mut head)?;
    if head.len() < 4 || head[..2] != SIGNATURE {
        return Ok(false);
    }
    let size = usize::from(u16_at(&head, 2));
    let Some(block) = head.get(4..4 + size + CRC) else {
        return Ok(false);
    };
    let (block, stored) = block.split_at(size);
    Ok(
        header(block).is_ok_and(|main| main.kind == Kind::MainHeader)
            && crc32fast::hash(block) == u32_at(stored, 0),
    )
}

/// Verify and optionally extract an ARJ archive into a new directory. It
/// reads methods 0 to 4, checks each header's and each file's CRC-32, and
/// restores DOS times, read-only attributes and the Unix modes an ARJ for
/// Unix header carries. Encrypted entries and multi-volume archives are
/// refused.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut entries = Entries::open(path)?;
    tree::extract("ARJ", path, destination, options, |tree| {
        parse(tree, &mut entries)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn block(first: u8, name: &[u8], comment: &[u8], size: usize) -> Vec<u8> {
        let mut block = vec![0u8; usize::from(first)];
        block[0] = first;
        block[6] = 0;
        block[5] = 1;
        block[12..16].copy_from_slice(&7u32.to_le_bytes());
        block.extend_from_slice(name);
        block.push(0);
        block.extend_from_slice(comment);
        block.push(0);
        block.resize(size.max(block.len()), 0);
        block
    }

    #[test]
    fn headers_read_their_fields_and_refuse_unended_strings() {
        let read = header(&block(30, b"a.txt", b"", 0)).unwrap();
        assert_eq!(
            (read.name.clone(), read.method, read.packed),
            (30..35, 1, 7)
        );
        assert_eq!(read.kind, Kind::File);
        assert!(header(&block(29, b"a", b"", 0)).is_err());
        let mut unended = block(30, b"a", b"b", 0);
        unended.pop();
        assert!(header(&unended).is_err());
        assert!(header(&block(30, b"a", b"", 2601)).is_err());
        assert!(header(&block(34, b"a", b"", 0)).is_ok());
    }

    #[test]
    fn modes_follow_the_host() {
        let mut found = header(&block(30, b"a", b"", 0)).unwrap();
        assert_eq!(file_mode(&found), FILE_MODE);
        found.mode = READ_ONLY;
        assert_eq!(file_mode(&found), READ_ONLY_MODE);
        found.host = HOST_UNIX;
        found.mode = 0o100755;
        assert_eq!(file_mode(&found), 0o755);
        found.host = 4;
        assert_eq!(file_mode(&found), FILE_MODE);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const LONGEST: usize = 34;

    #[kani::proof]
    #[kani::unwind(36)]
    fn headers_never_panic_and_keep_the_name_inside() {
        let bytes: [u8; LONGEST] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= LONGEST);
        if let Ok(found) = header(&bytes[..length]) {
            assert!(found.name.start >= SMALLEST_HEADER);
            assert!(found.name.end < length);
            assert_eq!(bytes[found.name.end], 0);
        }
    }

    #[kani::proof]
    fn methods_are_exactly_zero_to_four() {
        let number: u8 = kani::any();
        assert_eq!(method(number).is_ok(), number <= 4);
    }
}
