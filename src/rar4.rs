use crate::rar_volumes::{self, Names, Part, PartCheck, Parts, Span};
use crate::rar4_lz;
use crate::tree::{self, MAX_NAME};
use crate::volumes::{ArchiveFile, Unchanged};
use crate::{BadField, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{self, BufReader, Read, Seek},
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub(crate) const SIGNATURE: [u8; 7] = *b"Rar!\x1A\x07\x00";
const COPY_BUFFER: usize = 1024 * 1024;
const LONGEST_NAME: u64 = 8192;
const BASE: usize = 7;
const MAIN_FIELDS: usize = 6;
const FILE_FIELDS: usize = 25;

const MARK_HEADER: u8 = 0x72;
const MAIN_HEADER: u8 = 0x73;
const FILE_HEADER: u8 = 0x74;
const OLD_COMMENT: u8 = 0x75;
const OLD_SIGNATURE: u8 = 0x79;
const SERVICE_HEADER: u8 = 0x7A;
const END_HEADER: u8 = 0x7B;

const LONG_BLOCK: u16 = 0x8000;
const MAIN_VOLUME: u16 = 0x0001;
const MAIN_SOLID: u16 = 0x0008;
const MAIN_NEW_NUMBERING: u16 = 0x0010;
const MAIN_ENCRYPTED: u16 = 0x0080;
const MAIN_FIRST_VOLUME: u16 = 0x0100;

const END_NEXT_VOLUME: u16 = 0x0001;
const END_DATA_CRC: u16 = 0x0002;
const END_VOLUME_NUMBER: u16 = 0x0008;

const SPLIT_BEFORE: u16 = 0x0001;
const SPLIT_AFTER: u16 = 0x0002;
const ENCRYPTED: u16 = 0x0004;
const SOLID: u16 = 0x0010;
const DIRECTORY_BITS: u16 = 0x00E0;
const LARGE: u16 = 0x0100;
const UNICODE: u16 = 0x0200;
const SALT: u16 = 0x0400;
const EXTENDED_TIME: u16 = 0x1000;

const STORED: u8 = 0x30;
const BEST: u8 = 0x35;
const RAR_2_9_ALGORITHM: u8 = 29;
const WINDOWS_DIRECTORY: u32 = 0x10;
const READ_ONLY: u32 = 0x01;
const TYPE_BITS: u32 = 0o170_000;
const SYMLINK_TYPE: u32 = 0o120_000;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Base {
    crc: u16,
    kind: u8,
    flags: u16,
    size: u16,
}

fn base(bytes: &[u8; BASE]) -> Base {
    Base {
        crc: u16::from_le_bytes([bytes[0], bytes[1]]),
        kind: bytes[2],
        flags: u16::from_le_bytes([bytes[3], bytes[4]]),
        size: u16::from_le_bytes([bytes[5], bytes[6]]),
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    Windows,
    Unix,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileHeader {
    flags: u16,
    packed: u64,
    unpacked: u64,
    host: Host,
    crc: u32,
    time: u32,
    version: u8,
    method: u8,
    attributes: u32,
    name: Range<usize>,
    fraction: Option<(u32, bool)>,
}

fn file_header(header: &[u8], flags: u16) -> Field<FileHeader> {
    field_check(
        header.len() >= BASE + FILE_FIELDS,
        "RAR 4 file header is shorter than its fields",
    )?;
    let fields = &header[BASE..];
    let mut packed = u64::from(u32_at(fields, 0));
    let mut unpacked = u64::from(u32_at(fields, 4));
    let host = match fields[8] {
        0..=2 => Host::Windows,
        3..=5 => Host::Unix,
        _ => return Err(BadField("RAR 4 file comes from an unknown host")),
    };
    let name_size = usize::from(u16::from_le_bytes([fields[19], fields[20]]));
    let mut at = BASE + FILE_FIELDS;
    if flags & LARGE != 0 {
        let high = header
            .get(at..at + 8)
            .ok_or(BadField("RAR 4 file header is shorter than its sizes"))?;
        packed |= u64::from(u32_at(high, 0)) << 32;
        unpacked |= u64::from(u32_at(high, 4)) << 32;
        at += 8;
    }
    field_check(
        name_size > 0 && name_size as u64 <= LONGEST_NAME,
        "RAR 4 name is empty or longer than 8192 bytes",
    )?;
    let name = at..at + name_size;
    field_check(name.end <= header.len(), "RAR 4 name runs past its header")?;
    at = name.end;
    if flags & SALT != 0 {
        at += 8;
        field_check(at <= header.len(), "RAR 4 salt runs past its header")?;
    }
    let fraction = if flags & EXTENDED_TIME != 0 {
        extended_time(&header[at..])?
    } else {
        None
    };
    Ok(FileHeader {
        flags,
        packed,
        unpacked,
        host,
        crc: u32_at(fields, 9),
        time: u32_at(fields, 13),
        version: fields[17],
        method: fields[18],
        attributes: u32_at(fields, 21),
        name,
        fraction,
    })
}

fn extended_time(bytes: &[u8]) -> Field<Option<(u32, bool)>> {
    let flags = bytes
        .get(..2)
        .map(|flags| u16::from_le_bytes([flags[0], flags[1]]))
        .ok_or(BadField("RAR 4 extended time runs past its header"))?;
    let mut at = 2;
    let mut modified = None;
    for index in (0..4).rev() {
        let mode = flags >> (index * 4);
        if mode & 8 == 0 {
            continue;
        }
        if index != 3 {
            at += 4;
        }
        let count = usize::from(mode & 3);
        let units = bytes
            .get(at..at + count)
            .ok_or(BadField("RAR 4 extended time runs past its header"))?
            .iter()
            .fold(0u32, |units, &byte| u32::from(byte) << 16 | units >> 8);
        at += count;
        if index == 3 {
            modified = Some((units, mode & 4 != 0));
        }
    }
    field_check(
        at <= bytes.len(),
        "RAR 4 extended time runs past its header",
    )?;
    Ok(modified)
}

fn unicode_name(raw: &[u8]) -> Field<String> {
    let Some(zero) = raw.iter().position(|&byte| byte == 0) else {
        return String::from_utf8(raw.to_vec()).map_err(|_| BadField("RAR 4 name is not UTF-8"));
    };
    let encoded = &raw[zero + 1..];
    let longest = raw.len();
    let mut units: Vec<u16> = Vec::new();
    let mut at = 0;
    let next = |at: &mut usize| -> Option<u8> {
        let byte = encoded.get(*at).copied();
        *at += 1;
        byte
    };
    let high = u16::from(next(&mut at).unwrap_or(0));
    let mut flags = 0u8;
    let mut flag_bits = 0;
    while at < encoded.len() && units.len() < longest {
        if flag_bits == 0 {
            flags = encoded[at];
            at += 1;
            flag_bits = 8;
        }
        flag_bits -= 2;
        match (flags >> flag_bits) & 3 {
            0 => match next(&mut at) {
                Some(low) => units.push(u16::from(low)),
                None => break,
            },
            1 => match next(&mut at) {
                Some(low) => units.push(high << 8 | u16::from(low)),
                None => break,
            },
            2 => {
                let Some(pair) = encoded.get(at..at + 2) else {
                    break;
                };
                units.push(u16::from_le_bytes([pair[0], pair[1]]));
                at += 2;
            }
            _ => {
                let Some(length) = next(&mut at) else { break };
                let (extra, run_high) = if length & 0x80 != 0 {
                    match next(&mut at) {
                        Some(extra) => (extra, high),
                        None => break,
                    }
                } else {
                    (0, 0)
                };
                for _ in 0..(length & 0x7F) + 2 {
                    if units.len() >= longest {
                        break;
                    }
                    let source = raw.get(units.len()).copied().unwrap_or(0);
                    units.push(run_high << 8 | u16::from(source.wrapping_add(extra)));
                }
            }
        }
    }
    String::from_utf16(&units).map_err(|_| BadField("RAR 4 Unicode name is not valid UTF-16"))
}

fn entry_name(raw: &[u8], flags: u16) -> Result<String> {
    let name = if flags & UNICODE != 0 {
        unicode_name(raw)?
    } else {
        String::from_utf8(raw.to_vec()).map_err(|_| {
            anyhow::anyhow!("RAR 4 name is in a code page, which Zipper does not read")
        })?
    };
    // EXTERNAL(WinRAR): RAR 3 and RAR 4 write `\` between folders on every host.
    Ok(name.replace('\\', "/"))
}

fn modified(header: &FileHeader) -> Option<SystemTime> {
    let base = crate::dos_local_time((header.time >> 16) as u16, header.time as u16)?;
    match header.fraction {
        Some((units, extra_second)) => base.checked_add(Duration::new(
            u64::from(extra_second),
            (units % 10_000_000) * 100,
        )),
        None => Some(base),
    }
}

fn file_mode(header: &FileHeader, directory: bool) -> u32 {
    match header.host {
        Host::Unix => header.attributes & 0o777,
        Host::Windows if directory => DIRECTORY_MODE,
        Host::Windows if header.attributes & READ_ONLY != 0 => READ_ONLY_MODE,
        Host::Windows => FILE_MODE,
    }
}

enum Data<'a> {
    Stored(Parts<'a>),
    Compressed(Box<rar4_lz::Decoder<Parts<'a>>>),
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
            Data::Compressed(data) => data.read(buffer)?,
        };
        self.crc.update(&buffer[..read]);
        self.read += read as u64;
        Ok(read)
    }
}

impl Checked<'_> {
    fn finish(self, name: &str, header: &FileHeader) -> Result<Option<rar4_lz::PpmdSpent>> {
        let mut spent = None;
        if let Data::Compressed(data) = &self.data {
            data.finish_at_declared_size()
                .with_context(|| format!("RAR 4 entry {name:?} is damaged"))?;
            spent = Some(data.ppmd_spent());
        }
        ensure!(
            self.read == header.unpacked,
            "truncated RAR 4 entry {name:?}"
        );
        ensure!(
            self.crc.finalize() == header.crc,
            "RAR 4 entry {name:?} fails its CRC-32 check"
        );
        Ok(spent)
    }
}

struct Block {
    base: Base,
    header: Vec<u8>,
    data: u64,
    data_size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    more: bool,
    number: Option<u16>,
}

// EXTERNAL(rar 6.24): an end block of a volume gives a CRC-32 of the volume's bytes before it
// when flag 0x0002 is set, then its volume number from 0 when flag 0x0008 is set.
fn end_block(header: &[u8], flags: u16) -> Field<End> {
    let at = BASE + if flags & END_DATA_CRC != 0 { 4 } else { 0 };
    let number = if flags & END_VOLUME_NUMBER != 0 {
        let bytes = header.get(at..at + 2).ok_or(BadField(
            "RAR 4 end of archive block is shorter than its volume number",
        ))?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    } else {
        None
    };
    Ok(End {
        more: flags & END_NEXT_VOLUME != 0,
        number,
    })
}

struct Volume {
    reader: BufReader<ArchiveFile>,
    length: u64,
    next: u64,
    index: u64,
    path: PathBuf,
    reopen: Option<Unchanged>,
    in_set: bool,
}

impl Volume {
    fn open(
        file: ArchiveFile,
        path: PathBuf,
        reopen: Option<Unchanged>,
        index: u64,
    ) -> Result<(Self, u16)> {
        let length = file.len()?;
        let mut volume = Volume {
            reader: BufReader::with_capacity(COPY_BUFFER, file),
            length,
            next: 0,
            index,
            path,
            reopen,
            in_set: false,
        };
        let mut signature = [0u8; SIGNATURE.len()];
        volume
            .reader
            .read_exact(&mut signature)
            .context("truncated RAR signature")?;
        ensure!(signature == SIGNATURE, "RAR 4 signature is missing");
        volume.next = SIGNATURE.len() as u64;
        let main = volume
            .block()?
            .context("RAR 4 archive has no main header")?;
        ensure!(
            main.base.kind == MAIN_HEADER && main.header.len() >= BASE + MAIN_FIELDS,
            "RAR 4 archive does not start with its main header"
        );
        ensure!(
            main.base.flags & MAIN_ENCRYPTED == 0,
            "RAR 4 archive has encrypted headers, which Zipper does not decrypt"
        );
        ensure!(
            main.base.flags & MAIN_SOLID == 0,
            "Zipper does not read RAR 4 solid archives"
        );
        Ok((volume, main.base.flags))
    }

    fn label(&self) -> String {
        rar_volumes::file_name(&self.path)
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step = i64::try_from(i128::from(offset) - i128::from(here))
            .context("RAR 4 block offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    fn block(&mut self) -> Result<Option<Block>> {
        let result = self.next_block();
        match self.in_set {
            true => result.with_context(|| format!("in RAR volume {}", self.label())),
            false => result,
        }
    }

    fn next_block(&mut self) -> Result<Option<Block>> {
        self.seek_to(self.next)?;
        if self.next == self.length {
            return Ok(None);
        }
        let mut start = [0u8; BASE];
        self.reader
            .read_exact(&mut start)
            .context("truncated RAR 4 header")?;
        let base = base(&start);
        ensure!(
            usize::from(base.size) >= BASE,
            "RAR 4 header is shorter than 7 bytes"
        );
        let mut header = start.to_vec();
        header.resize(usize::from(base.size), 0);
        self.reader
            .read_exact(&mut header[BASE..])
            .context("truncated RAR 4 header")?;
        ensure!(
            crc32fast::hash(&header[2..]) as u16 == base.crc,
            "RAR 4 header fails its CRC check"
        );
        let data_size = match base.kind {
            FILE_HEADER | SERVICE_HEADER => file_header(&header, base.flags)?.packed,
            _ if base.flags & LONG_BLOCK != 0 => {
                ensure!(
                    header.len() >= BASE + 4,
                    "RAR 4 header is shorter than its data size"
                );
                u64::from(u32_at(&header, BASE))
            }
            _ => 0,
        };
        let data = self.reader.stream_position()?;
        self.next = data
            .checked_add(data_size)
            .filter(|&end| end <= self.length)
            .context("truncated RAR 4 data area")?;
        Ok(Some(Block {
            base,
            header,
            data,
            data_size,
        }))
    }
}

struct Set {
    names: Option<Names>,
    new_numbering: bool,
}

fn next_volume(set: &Set, current: &Volume) -> Result<Volume> {
    let names = set.names.as_ref().with_context(|| {
        format!(
            "{} is a RAR 4 volume whose name does not end in {}, so Zipper cannot name the volume after it",
            current.label(),
            if set.new_numbering { ".partN.rar" } else { ".rar" }
        )
    })?;
    let index = current.index + 1;
    let (path, file, unchanged) = rar_volumes::open(names, index, &current.path)?;
    let label = rar_volumes::file_name(&path);
    let (mut volume, flags) = Volume::open(file, path, Some(unchanged), index)
        .with_context(|| format!("in RAR volume {label}"))?;
    volume.in_set = true;
    ensure!(
        flags & MAIN_VOLUME != 0,
        "RAR volume {label} does not mark itself as a volume, so it does not belong to the set"
    );
    ensure!(
        flags & MAIN_FIRST_VOLUME == 0,
        "RAR volume {label} marks itself as the first volume of a set, so the volumes are out of order or come from different sets"
    );
    ensure!(
        (flags & MAIN_NEW_NUMBERING != 0) == set.new_numbering,
        "RAR volume {label} and the first volume disagree on how the volumes are named, so they come from different sets"
    );
    Ok(volume)
}

fn more_volumes(set: Option<&Set>, volume: &Volume, block: &Block) -> Result<bool> {
    let flags = block.base.flags;
    if set.is_none() {
        ensure!(
            flags & END_NEXT_VOLUME == 0,
            "RAR 4 end of archive block says more volumes follow, but the main header does not mark a volume"
        );
        return Ok(false);
    }
    let end = end_block(&block.header, flags)?;
    if let Some(number) = end.number {
        ensure!(
            u64::from(number) == volume.index,
            "RAR volume {} calls itself volume {} of its set, not volume {}, so the volumes are out of order or come from different sets",
            volume.label(),
            u64::from(number) + 1,
            volume.index + 1
        );
    }
    Ok(end.more)
}

fn same_entry(first: (&[u8], &FileHeader), part: (&[u8], &FileHeader)) -> bool {
    let ((first_header, first), (part_header, part)) = (first, part);
    let kept = !(SPLIT_BEFORE | SPLIT_AFTER | LARGE);
    first_header[first.name.clone()] == part_header[part.name.clone()]
        && first.flags & kept == part.flags & kept
        && first.unpacked == part.unpacked
        && first.host == part.host
        && first.version == part.version
        && first.method == part.method
}

pub(crate) struct Reader {
    volume: Volume,
    set: Option<Set>,
    ppmd_spent: rar4_lz::PpmdSpent,
}

impl Reader {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let (mut volume, flags) = Volume::open(file, path.to_owned(), None, 0)?;
        let set = match flags & MAIN_VOLUME {
            0 => None,
            _ => {
                let new_numbering = flags & MAIN_NEW_NUMBERING != 0;
                let names = match new_numbering {
                    true => Names::new_style(path),
                    false => Names::old_style(path),
                };
                ensure!(
                    flags & MAIN_FIRST_VOLUME != 0,
                    rar_volumes::later_volume(path, names.as_ref(), None)
                );
                Some(Set {
                    names,
                    new_numbering,
                })
            }
        };
        volume.in_set = set.is_some();
        Ok(Reader {
            volume,
            set,
            ppmd_spent: rar4_lz::PpmdSpent::default(),
        })
    }

    // EXTERNAL(rar 6.24): a part that continues in the next volume carries the CRC-32 of
    // its own packed data, and the last part the CRC-32 of the whole file.
    fn continuation(
        &mut self,
        block: &Block,
        first: &FileHeader,
        name: &str,
    ) -> Result<(FileHeader, Span, Volume)> {
        let set = self.set.as_ref().with_context(|| {
            format!("RAR 4 entry {name:?} continues into another volume, but the archive is not a volume")
        })?;
        let mut span = Span::starting(Part {
            path: Some(self.volume.path.clone()),
            reopen: None,
            offset: block.data,
            size: block.data_size,
            check: Some(PartCheck {
                crc: Some(first.crc),
                ..PartCheck::default()
            }),
        });
        let mut later: Option<Volume> = None;
        loop {
            let volume = later.as_mut().unwrap_or(&mut self.volume);
            loop {
                let after = volume.block()?.with_context(|| {
                    format!(
                        "RAR 4 entry {name:?} continues into the next volume, but {} ends without its end of archive block",
                        volume.label()
                    )
                })?;
                match after.base.kind {
                    SERVICE_HEADER if after.base.flags & (SPLIT_BEFORE | SPLIT_AFTER) == 0 => {}
                    OLD_COMMENT..=OLD_SIGNATURE => {}
                    END_HEADER => {
                        ensure!(
                            more_volumes(Some(set), volume, &after)?,
                            "RAR 4 entry {name:?} continues into the next volume, but {} is the last volume",
                            volume.label()
                        );
                        break;
                    }
                    _ => bail!(
                        "RAR 4 entry {name:?} continues into the next volume, but another block follows it in {}",
                        volume.label()
                    ),
                }
            }
            let mut next = next_volume(set, volume)?;
            let part = next.block()?.with_context(|| {
                format!(
                    "RAR volume {} does not start with the rest of RAR 4 entry {name:?}",
                    next.label()
                )
            })?;
            ensure!(
                part.base.kind == FILE_HEADER && part.base.flags & SPLIT_BEFORE != 0,
                "RAR volume {} does not start with the rest of RAR 4 entry {name:?}",
                next.label()
            );
            let header = file_header(&part.header, part.base.flags)?;
            ensure!(
                same_entry((&block.header, first), (&part.header, &header)),
                "RAR volume {} continues RAR 4 entry {name:?} with another name, size or method, so the volumes come from different sets",
                next.label()
            );
            let last = part.base.flags & SPLIT_AFTER == 0;
            span.push(Part {
                path: Some(next.path.clone()),
                reopen: next.reopen,
                offset: part.data,
                size: part.data_size,
                check: (!last).then(|| PartCheck {
                    crc: Some(header.crc),
                    ..PartCheck::default()
                }),
            })?;
            if last {
                let whole = FileHeader {
                    crc: header.crc,
                    ..first.clone()
                };
                return Ok((whole, span, next));
            }
            later = Some(next);
        }
    }

    fn entry(&mut self, tree: &mut tree::Tree, block: &Block) -> Result<()> {
        let header = file_header(&block.header, block.base.flags)?;
        let name = entry_name(&block.header[header.name.clone()], header.flags)?;
        ensure!(
            header.flags & SPLIT_BEFORE == 0,
            "RAR 4 entry {name:?} continues from a volume before, but no entry there continues into it"
        );
        ensure!(
            header.flags & ENCRYPTED == 0,
            "RAR 4 entry {name:?} is encrypted, which Zipper does not decrypt"
        );
        ensure!(
            header.flags & SOLID == 0,
            "RAR 4 entry {name:?} is solid, which Zipper does not read"
        );
        let (header, span, after) = match header.flags & SPLIT_AFTER {
            0 => (header, Span::one(block.data_size), None),
            _ => {
                let (whole, span, after) = self.continuation(block, &header, &name)?;
                (whole, span, Some(after))
            }
        };
        let result = self.contents(tree, block, &header, &name, span);
        if let Some(after) = after {
            self.volume = after;
        }
        result
    }

    fn contents(
        &mut self,
        tree: &mut tree::Tree,
        block: &Block,
        header: &FileHeader,
        name: &str,
        span: Span,
    ) -> Result<()> {
        let directory = header.flags & DIRECTORY_BITS == DIRECTORY_BITS
            || header.host == Host::Windows && header.attributes & WINDOWS_DIRECTORY != 0;
        let mode = file_mode(header, directory);
        let time = modified(header);
        if directory {
            ensure!(
                span.packed == 0 && header.unpacked == 0,
                "RAR 4 folder entry {name:?} carries data"
            );
            return tree.directory(name, mode, time);
        }
        ensure!(
            (STORED..=BEST).contains(&header.method),
            "RAR 4 entry {name:?} uses an unknown method"
        );
        ensure!(
            header.method == STORED || header.version == RAR_2_9_ALGORITHM,
            "RAR 4 entry {name:?} uses the RAR {}.{} compression, which Zipper does not read",
            header.version / 10,
            header.version % 10
        );
        let symlink = header.host == Host::Unix && header.attributes & TYPE_BITS == SYMLINK_TYPE;
        if symlink {
            ensure!(
                header.unpacked <= MAX_NAME,
                "RAR 4 symlink {name:?} has a target longer than {MAX_NAME} bytes"
            );
        }
        let packed = span.packed;
        self.volume.seek_to(block.data)?;
        let raw = Parts::new(&mut self.volume.reader, span, "RAR 4", name);
        let data = if header.method == STORED {
            ensure!(
                packed == header.unpacked,
                "RAR 4 stored entry {name:?} packs {packed} bytes for {}",
                header.unpacked
            );
            Data::Stored(raw)
        } else {
            Data::Compressed(Box::new(rar4_lz::Decoder::new(
                raw,
                packed,
                header.unpacked,
                self.ppmd_spent,
            )))
        };
        let mut checked = Checked {
            data,
            crc: crc32fast::Hasher::new(),
            read: 0,
        };
        if symlink {
            let mut target = Vec::new();
            (&mut checked)
                .take(header.unpacked)
                .read_to_end(&mut target)
                .with_context(|| format!("RAR 4 entry {name:?} is damaged"))?;
            let spent = checked.finish(name, header)?;
            self.charge(spent);
            let target = String::from_utf8(target)
                .map_err(|_| anyhow::anyhow!("RAR 4 link target is not UTF-8"))?;
            return tree.symlink(name, target);
        }
        tree.file(name, header.unpacked, mode, time, &mut checked)?;
        let spent = checked.finish(name, header)?;
        self.charge(spent);
        Ok(())
    }

    fn charge(&mut self, spent: Option<rar4_lz::PpmdSpent>) {
        if let Some(spent) = spent {
            self.ppmd_spent = spent;
        }
    }

    pub(crate) fn parse(&mut self, tree: &mut tree::Tree) -> Result<()> {
        loop {
            let Some(block) = self.volume.block()? else {
                ensure!(
                    self.set.is_none(),
                    "RAR volume {} ends without its end of archive block, so it is truncated",
                    self.volume.label()
                );
                return Ok(());
            };
            tree.check()?;
            match block.base.kind {
                FILE_HEADER => self.entry(tree, &block)?,
                SERVICE_HEADER | OLD_COMMENT..=OLD_SIGNATURE => {}
                END_HEADER => {
                    if !more_volumes(self.set.as_ref(), &self.volume, &block)? {
                        return Ok(());
                    }
                    let set = self.set.as_ref().expect("only a set has more volumes");
                    self.volume = next_volume(set, &self.volume)?;
                }
                MARK_HEADER | MAIN_HEADER => {
                    bail!("RAR 4 archive has a second marker or main header")
                }
                other => bail!("RAR 4 header type {other:#x} is unknown"),
            }
        }
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn header(flags: u16, name: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; BASE + FILE_FIELDS];
        out[2] = FILE_HEADER;
        out[3..5].copy_from_slice(&flags.to_le_bytes());
        out[BASE + 8] = 3;
        out[BASE + 19..BASE + 21].copy_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(tail);
        let size = out.len() as u16;
        out[5..7].copy_from_slice(&size.to_le_bytes());
        out
    }

    #[test]
    fn file_headers_keep_the_name_and_large_sizes() {
        let bytes = header(0, b"a.txt", &[]);
        let found = file_header(&bytes, 0).unwrap();
        assert_eq!(&bytes[found.name.clone()], b"a.txt");
        assert_eq!(found.host, Host::Unix);
        let mut large = header(LARGE, b"", &[]);
        large.extend([1, 0, 0, 0, 2, 0, 0, 0]);
        large[BASE + 19] = 1;
        large.push(b'x');
        let found = file_header(&large, LARGE).unwrap();
        assert_eq!((found.packed, found.unpacked), (1 << 32, 2 << 32));
        assert!(file_header(&header(0, b"ab", &[])[..BASE + FILE_FIELDS + 1], 0).is_err());
    }

    #[test]
    fn extended_times_read_the_fraction_of_the_modification_time() {
        assert_eq!(
            extended_time(&[0x00, 0xF0, 0x01, 0x02, 0x03]).unwrap(),
            Some((0x03_02_01, true))
        );
        assert_eq!(extended_time(&[0x00, 0x00]).unwrap(), None);
        assert!(extended_time(&[0x00, 0x80, 0, 0, 0]).is_ok());
        assert!(extended_time(&[0x00, 0x08]).is_err());
        assert!(extended_time(&[0x00]).is_err());
    }

    #[test]
    fn unicode_names_decode_each_pair_of_flag_bits() {
        assert_eq!(unicode_name(b"plain").unwrap(), "plain");
        let mut raw = b"ab".to_vec();
        raw.push(0);
        raw.extend([0x30, 0b0110_0000, 0x42, 0x01, 0x42]);
        assert_eq!(unicode_name(&raw).unwrap(), "\u{3042}\u{4201}");
        let mut run = b"ab".to_vec();
        run.push(0);
        run.extend([0x00, 0b1100_0000, 0x00]);
        assert_eq!(unicode_name(&run).unwrap(), "ab");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn extended_times_never_panic_and_fit_three_bytes_of_units() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(Some((units, _))) = extended_time(&bytes[..length]) {
            assert!(units < 1 << 24);
        }
    }

    #[kani::proof]
    fn end_blocks_never_panic_and_read_the_volume_number_inside_the_block() {
        let bytes: [u8; 14] = kani::any();
        let length: usize = kani::any();
        let flags: u16 = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(end) = end_block(&bytes[..length], flags) {
            assert_eq!(end.more, flags & END_NEXT_VOLUME != 0);
            match end.number {
                Some(number) => {
                    let at = BASE + if flags & END_DATA_CRC != 0 { 4 } else { 0 };
                    assert!(at + 2 <= length);
                    assert_eq!(number, u16::from_le_bytes([bytes[at], bytes[at + 1]]));
                }
                None => assert_eq!(flags & END_VOLUME_NUMBER, 0),
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(4)]
    fn file_headers_never_panic_and_keep_the_name_inside_the_header() {
        let bytes: [u8; 36] = kani::any();
        let length: usize = kani::any();
        let flags: u16 = kani::any();
        kani::assume(length <= bytes.len());
        kani::assume(flags & EXTENDED_TIME == 0);
        if let Ok(header) = file_header(&bytes[..length], flags) {
            assert!(header.name.start >= BASE + FILE_FIELDS);
            assert!(header.name.start < header.name.end && header.name.end <= length);
        }
    }
}
