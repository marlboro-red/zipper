//! In-process reader for ISO 9660 disc images, with Joliet, Rock Ridge and
//! El Torito, and for the UDF file system on the same discs.
//! `docs/reference/iso-reader.md` gives the rules.

pub use crate::native_cpio::ReadOptions;
use crate::{
    Entry, Field, field_check,
    native_cpio::tree::{self, BACKEND_HINT, Tree},
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    io::Read,
    path::Path,
    time::SystemTime,
};

pub(crate) const SECTOR: u64 = 2048;
pub(crate) const SECTOR_BYTES: usize = 2048;
const DESCRIPTORS_START: u64 = 16;
const MAX_DESCRIPTORS: u64 = 256;
const DESCRIPTOR_HEAD: usize = 190;
const ROOT_RECORD: usize = 156;
const RECORD_HEAD: usize = 33;
const ROOT_RECORD_LENGTH: usize = 34;
const MAX_ISO_DEPTH: usize = 256;
const MAX_CONTINUATIONS: usize = 32;
const MAX_BOOT_ENTRIES: usize = 64;
const MAX_BOOT_EXTENSIONS: usize = 32;
const CATALOG_ENTRY: usize = 32;
const CATALOG_SECTORS: u64 = 16;
pub(crate) const MAX_DIRECTORY_BYTES: u64 = 1 << 30;
pub(crate) const FILE_MODE: u32 = 0o644;
pub(crate) const DIRECTORY_MODE: u32 = 0o755;
const STANDARD_ID: &[u8; 5] = b"CD001";
const EL_TORITO: &[u8] = b"EL TORITO SPECIFICATION";
const BOOT_FOLDER: &str = "[BOOT]";
const FLAG_DIRECTORY: u8 = 0x02;
const FLAG_MORE_EXTENTS: u8 = 0x80;
const TYPE_MASK: u32 = 0o170000;
const MODE_DIRECTORY: u32 = 0o040000;
const MODE_REGULAR: u32 = 0o100000;
const MODE_SYMLINK: u32 = 0o120000;
const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_FROM_1970_TO_CIVIL_ZERO: i64 = 719_468;
pub(crate) const NO_PARENT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Directory,
    File,
    Symlink(Box<str>),
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) parent: u32,
    pub(crate) name: Box<str>,
    pub(crate) kind: Kind,
    pub(crate) mode: u32,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) size: u64,
    pub(crate) extents: Extents,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum Extents {
    #[default]
    None,
    One(Extent),
    Many(Vec<Extent>),
}

impl Extents {
    pub(crate) fn from_vec(mut extents: Vec<Extent>) -> Extents {
        match extents.len() {
            0 => Extents::None,
            1 => Extents::One(extents.pop().unwrap()),
            _ => Extents::Many(extents),
        }
    }

    pub(crate) fn as_slice(&self) -> &[Extent] {
        match self {
            Extents::None => &[],
            Extents::One(extent) => std::slice::from_ref(extent),
            Extents::Many(extents) => extents,
        }
    }

    fn push(&mut self, extent: Extent) {
        *self = match std::mem::take(self) {
            Extents::None => Extents::One(extent),
            Extents::One(first) => Extents::Many(vec![first, extent]),
            Extents::Many(mut extents) => {
                extents.push(extent);
                Extents::Many(extents)
            }
        };
    }
}

pub(crate) struct Catalog {
    pub(crate) format: &'static str,
    pub(crate) nodes: Vec<Node>,
}

pub(crate) struct Image {
    file: ArchiveFile,
    pub(crate) length: u64,
    directory_bytes: u64,
}

impl Image {
    fn open(path: &Path) -> Result<Image> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        Ok(Image {
            file,
            length,
            directory_bytes: 0,
        })
    }

    pub(crate) fn read_exact_at(
        &mut self,
        buffer: &mut [u8],
        offset: u64,
        what: &str,
    ) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .filter(|end| *end <= self.length)
            .with_context(|| format!("the image ends inside its {what}"))?;
        let mut done = 0;
        while done < buffer.len() {
            let read = self
                .file
                .read_at(&mut buffer[done..], offset + done as u64)
                .with_context(|| format!("cannot read the image's {what}"))?;
            ensure!(read > 0, "the image ends inside its {what}");
            done += read;
        }
        debug_assert_eq!(offset + done as u64, end);
        Ok(())
    }

    pub(crate) fn charge_directory(&mut self, bytes: u64) -> Result<()> {
        self.directory_bytes = self.directory_bytes.saturating_add(bytes);
        ensure!(
            self.directory_bytes <= MAX_DIRECTORY_BYTES,
            "the image's folders hold more than {MAX_DIRECTORY_BYTES} bytes"
        );
        Ok(())
    }

    pub(crate) fn ensure_inside(&self, extent: Extent, what: &str) -> Result<()> {
        ensure!(
            extent
                .offset
                .checked_add(extent.length)
                .is_some_and(|end| end <= self.length),
            "{what} lies past the end of the image"
        );
        Ok(())
    }
}

fn both_endian_16(bytes: &[u8]) -> Field<u16> {
    let little = u16::from_le_bytes([bytes[0], bytes[1]]);
    field_check(
        little == u16::from_be_bytes([bytes[2], bytes[3]]),
        "ISO 9660 field differs between its two byte orders",
    )?;
    Ok(little)
}

fn both_endian_32(bytes: &[u8]) -> Field<u32> {
    let little = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    field_check(
        little == u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        "ISO 9660 field differs between its two byte orders",
    )?;
    Ok(little)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    length: usize,
    extent: u32,
    size: u32,
    date: [u8; 7],
    flags: u8,
    id_length: usize,
    system_use: usize,
}

impl Record {
    fn id<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[RECORD_HEAD..RECORD_HEAD + self.id_length]
    }

    fn system_use<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.system_use..self.length]
    }

    fn is_directory(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }

    fn is_system_item(&self, bytes: &[u8]) -> bool {
        self.id_length == 1 && bytes[RECORD_HEAD] < 2
    }
}

fn directory_record(bytes: &[u8]) -> Field<Record> {
    field_check(
        bytes.len() >= ROOT_RECORD_LENGTH,
        "ISO 9660 directory record is cut short",
    )?;
    let length = usize::from(bytes[0]);
    field_check(
        length >= ROOT_RECORD_LENGTH && length <= bytes.len(),
        "ISO 9660 directory record length does not fit its folder",
    )?;
    field_check(
        bytes[1] == 0,
        "ISO 9660 extended attribute records are not supported",
    )?;
    let extent = both_endian_32(&bytes[2..10])?;
    let size = both_endian_32(&bytes[10..18])?;
    let mut date = [0u8; 7];
    date.copy_from_slice(&bytes[18..25]);
    field_check(
        bytes[26] == 0 && bytes[27] == 0,
        "ISO 9660 interleaved files are not supported",
    )?;
    let id_length = usize::from(bytes[32]);
    field_check(id_length > 0, "ISO 9660 directory record has an empty name")?;
    let system_use = RECORD_HEAD + id_length + usize::from(id_length % 2 == 0);
    field_check(
        system_use <= length,
        "ISO 9660 directory record name runs past the record",
    )?;
    Ok(Record {
        length,
        extent,
        size,
        date,
        flags: bytes[25],
        id_length,
        system_use,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Descriptor {
    kind: u8,
    joliet: bool,
    block_size: u16,
    root: Record,
}

fn volume_descriptor(bytes: &[u8; DESCRIPTOR_HEAD]) -> Field<Descriptor> {
    field_check(
        &bytes[1..6] == STANDARD_ID,
        "ISO 9660 volume descriptor has no CD001",
    )?;
    field_check(
        bytes[6] <= 2,
        "ISO 9660 volume descriptor has an unknown version",
    )?;
    let kind = bytes[0];
    if !matches!(kind, 1 | 2) {
        return Ok(Descriptor {
            kind,
            joliet: false,
            block_size: 0,
            root: Record {
                length: 0,
                extent: 0,
                size: 0,
                date: [0; 7],
                flags: 0,
                id_length: 0,
                system_use: 0,
            },
        });
    }
    field_check(
        bytes[72..80].iter().all(|byte| *byte == 0),
        "ISO 9660 volume descriptor has data in an unused field",
    )?;
    both_endian_32(&bytes[80..88])?;
    both_endian_32(&bytes[132..140])?;
    let block_size = both_endian_16(&bytes[128..132])?;
    let root = directory_record(&bytes[ROOT_RECORD..])?;
    field_check(
        root.length == ROOT_RECORD_LENGTH && root.is_directory(),
        "ISO 9660 root record is not a 34-byte folder record",
    )?;
    let joliet = kind == 2
        && bytes[7] & 1 == 0
        && bytes[88] == b'%'
        && bytes[89] == b'/'
        && matches!(bytes[90], b'@' | b'C' | b'E');
    Ok(Descriptor {
        kind,
        joliet,
        block_size,
        root,
    })
}

fn dates_are_digits(sector: &[u8]) -> bool {
    (0..4).all(|date| {
        sector[813 + date * 17..813 + date * 17 + 16]
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, 0 | b' '))
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Continuation {
    block: u32,
    offset: u32,
    length: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RockRidge {
    sharing_protocol: Option<u8>,
    names: usize,
    name: Option<(usize, usize)>,
    modes: usize,
    mode: Option<u32>,
    links: usize,
    link: Option<(usize, usize)>,
    continuation: Option<Continuation>,
    continuations: usize,
    relocated: bool,
}

const SUSP_HEAD: usize = 4;

fn rock_ridge(area: &[u8]) -> Field<RockRidge> {
    let mut found = RockRidge::default();
    let mut at = 0usize;
    while area.len() - at >= SUSP_HEAD {
        let entry = &area[at..];
        let length = usize::from(entry[2]);
        if length < SUSP_HEAD || length > entry.len() {
            field_check(
                entry.iter().all(|byte| *byte == 0),
                "Rock Ridge entry length does not fit its area",
            )?;
            break;
        }
        let data = &entry[SUSP_HEAD..length];
        let version = entry[3];
        let start = at + SUSP_HEAD;
        match [entry[0], entry[1]] {
            [b'S', b'T'] => break,
            [b'S', b'P'] => {
                if length == 7 && version == 1 && data[0] == 0xBE && data[1] == 0xEF {
                    found.sharing_protocol = Some(data[2]);
                }
            }
            [b'C', b'E'] => {
                field_check(
                    length == 28 && version == 1,
                    "Rock Ridge CE entry is malformed",
                )?;
                found.continuations += 1;
                found.continuation = Some(Continuation {
                    block: both_endian_32(&data[0..8])?,
                    offset: both_endian_32(&data[8..16])?,
                    length: both_endian_32(&data[16..24])?,
                });
            }
            [b'N', b'M'] => {
                field_check(
                    version == 1 && !data.is_empty() && data[0] == 0,
                    "Rock Ridge name continues or names a special folder",
                )?;
                found.names += 1;
                found.name = Some((start + 1, start + data.len()));
            }
            [b'P', b'X'] => {
                field_check(
                    version == 1 && matches!(length, 36 | 44),
                    "Rock Ridge PX entry is malformed",
                )?;
                found.modes += 1;
                found.mode = Some(both_endian_32(&data[0..8])?);
            }
            [b'S', b'L'] => {
                field_check(
                    version == 1 && !data.is_empty() && data[0] == 0,
                    "Rock Ridge link continues past its entry",
                )?;
                found.links += 1;
                found.link = Some((start + 1, start + data.len()));
            }
            [b'R', b'E'] | [b'C', b'L'] | [b'P', b'L'] => found.relocated = true,
            _ => {}
        }
        at += length;
    }
    Ok(found)
}

const SL_CONTINUE: u8 = 1;
const SL_CURRENT: u8 = 2;
const SL_PARENT: u8 = 4;
const SL_ROOT: u8 = 8;

fn symlink_target(components: &[u8]) -> Field<Vec<u8>> {
    let mut target = Vec::new();
    let mut at = 0;
    while at < components.len() {
        field_check(
            components.len() - at >= 2,
            "Rock Ridge link component is cut short",
        )?;
        let flags = components[at];
        let length = usize::from(components[at + 1]);
        let text = components
            .get(at + 2..at + 2 + length)
            .ok_or(crate::BadField(
                "Rock Ridge link component runs past its entry",
            ))?;
        at += 2 + length;
        let special = match flags {
            0 => None,
            SL_CURRENT => Some(&b"./"[..]),
            SL_PARENT => Some(&b"../"[..]),
            SL_ROOT => Some(&b"/"[..]),
            _ => {
                field_check(
                    flags & SL_CONTINUE == 0,
                    "Rock Ridge link component continues",
                )?;
                return Err(crate::BadField(
                    "Rock Ridge link component has unknown flags",
                ));
            }
        };
        match special {
            Some(spelling) => {
                field_check(length == 0, "Rock Ridge special link component has a name")?;
                target.extend_from_slice(spelling);
            }
            None => {
                field_check(
                    length > 0 && !text.contains(&0),
                    "Rock Ridge link component name is empty or holds NUL",
                )?;
                target.extend_from_slice(text);
                if at < components.len() {
                    target.push(b'/');
                }
            }
        }
    }
    Ok(target)
}

fn joliet_name(id: &[u8]) -> Result<String> {
    ensure!(id.len().is_multiple_of(2), "Joliet name has an odd length");
    let units: Vec<u16> = id
        .chunks_exact(2)
        .map(|unit| u16::from_be_bytes([unit[0], unit[1]]))
        .collect();
    ensure!(!units.contains(&0), "Joliet name holds NUL");
    String::from_utf16(&units).context("Joliet name has an unpaired surrogate")
}

pub(crate) fn ensure_folder_fits(children: usize) -> Result<()> {
    ensure!(
        children <= tree::MAX_ENTRIES,
        "a folder holds more than {} entries",
        tree::MAX_ENTRIES
    );
    Ok(())
}

pub(crate) fn ensure_single_name(format: &str, name: &str) -> Result<()> {
    ensure!(!name.contains('/'), "{format} name {name:?} holds a slash");
    ensure!(
        !matches!(name, "" | "." | ".."),
        "{format} name {name:?} names no entry of its own"
    );
    Ok(())
}

pub(crate) fn without_version(name: &str) -> &str {
    let name = name.strip_suffix(";1").unwrap_or(name);
    name.strip_suffix('.').unwrap_or(name)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - DAYS_FROM_1970_TO_CIVIL_ZERO
}

fn civil_seconds(year: u16, fields: [u8; 5], offset_minutes: i16) -> Option<i64> {
    let [month, day, hour, minute, second] = fields.map(i64::from);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(
        days_from_civil(i64::from(year), month, day) * SECONDS_PER_DAY
            + hour * 3600
            + minute * 60
            + second
            - i64::from(offset_minutes) * 60,
    )
}

pub(crate) fn civil_time(
    year: u16,
    fields: [u8; 5],
    offset_minutes: i16,
    nanoseconds: u32,
) -> Option<SystemTime> {
    let seconds = civil_seconds(year, fields, offset_minutes)?;
    let whole = std::time::Duration::from_secs(seconds.unsigned_abs());
    let base = if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(whole)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(whole)
    };
    base?.checked_add(std::time::Duration::from_nanos(u64::from(nanoseconds)))
}

fn recording_fields(date: [u8; 7]) -> (u16, [u8; 5], i16) {
    let [year, month, day, hour, minute, second, offset] = date;
    (
        1900 + u16::from(year),
        [month, day, hour, minute, second],
        i16::from(offset as i8) * 15,
    )
}

fn recording_time(date: [u8; 7]) -> Option<SystemTime> {
    let (year, fields, offset) = recording_fields(date);
    civil_time(year, fields, offset, 0)
}

pub(crate) trait Folder {
    type Key: Copy + Eq + Hash;
    fn key(&self) -> Self::Key;
}

pub(crate) struct Child<F> {
    pub(crate) folder: Option<F>,
    pub(crate) node: Node,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Walk {
    Tree,
    Cycle,
    Shared,
    TooDeep,
    TooMany,
}

pub(crate) fn walk_folders<F: Folder>(
    root: F,
    max_depth: usize,
    max_nodes: usize,
    nodes: &mut Vec<Node>,
    mut list: impl FnMut(F) -> Result<Vec<Child<F>>>,
) -> Result<Walk> {
    struct Frame<F: Folder> {
        key: F::Key,
        node: u32,
        children: std::vec::IntoIter<Child<F>>,
    }
    let mut open: HashSet<F::Key> = HashSet::from([root.key()]);
    let mut read: HashSet<F::Key> = HashSet::new();
    let mut stack = vec![Frame {
        key: root.key(),
        node: NO_PARENT,
        children: list(root)?.into_iter(),
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(mut child) = frame.children.next() else {
            let done = stack.pop().unwrap();
            open.remove(&done.key);
            read.insert(done.key);
            continue;
        };
        if nodes.len() >= max_nodes {
            return Ok(Walk::TooMany);
        }
        child.node.parent = frame.node;
        let index = nodes.len() as u32;
        nodes.push(child.node);
        let Some(folder) = child.folder else {
            continue;
        };
        let key = folder.key();
        if open.contains(&key) {
            return Ok(Walk::Cycle);
        }
        if read.contains(&key) {
            return Ok(Walk::Shared);
        }
        if stack.len() > max_depth {
            return Ok(Walk::TooDeep);
        }
        open.insert(key);
        let children = list(folder)?.into_iter();
        stack.push(Frame {
            key,
            node: index,
            children,
        });
    }
    Ok(Walk::Tree)
}

#[derive(Clone, Copy)]
struct IsoFolder {
    extent: u32,
    size: u32,
}

impl Folder for IsoFolder {
    type Key = u32;
    fn key(&self) -> u32 {
        self.extent
    }
}

struct Volume {
    root: Record,
    joliet: bool,
    catalog: Option<u32>,
}

fn read_volumes(image: &mut Image) -> Result<Option<Volume>> {
    if image.length < (DESCRIPTORS_START + 1) * SECTOR {
        return Ok(None);
    }
    let mut sector = [0u8; SECTOR_BYTES];
    image.read_exact_at(
        &mut sector,
        DESCRIPTORS_START * SECTOR,
        "volume descriptors",
    )?;
    if &sector[1..6] != STANDARD_ID {
        return Ok(None);
    }
    let mut volumes: Vec<Descriptor> = Vec::new();
    let mut catalog = None;
    for index in DESCRIPTORS_START..DESCRIPTORS_START + MAX_DESCRIPTORS {
        image.read_exact_at(&mut sector, index * SECTOR, "volume descriptors")?;
        let head: &[u8; DESCRIPTOR_HEAD] = sector[..DESCRIPTOR_HEAD].try_into().unwrap();
        let descriptor = volume_descriptor(head)?;
        match descriptor.kind {
            255 => break,
            0 if sector[7..39].starts_with(EL_TORITO)
                && sector[7 + EL_TORITO.len()..39]
                    .iter()
                    .all(|byte| *byte == 0) =>
            {
                ensure!(
                    catalog.is_none(),
                    "the image has two El Torito boot records"
                );
                catalog = Some(u32::from_le_bytes(sector[0x47..0x4B].try_into().unwrap()));
            }
            1 | 2 => {
                ensure!(
                    dates_are_digits(&sector),
                    "ISO 9660 volume descriptor has a date that is not digits"
                );
                volumes.push(descriptor);
            }
            _ => {}
        }
        ensure!(
            index + 1 < DESCRIPTORS_START + MAX_DESCRIPTORS,
            "the image has more than {MAX_DESCRIPTORS} volume descriptors"
        );
    }
    let first = volumes
        .first()
        .context("the image has no ISO 9660 volume")?;
    ensure!(
        first.kind == 1,
        "the image's first ISO 9660 volume is not the primary one"
    );
    let main = volumes
        .iter()
        .skip(1)
        .rev()
        .find(|descriptor| descriptor.joliet)
        .unwrap_or(first);
    ensure!(
        u64::from(main.block_size) == SECTOR,
        "ISO 9660 blocks of {} bytes are not supported; {BACKEND_HINT}",
        main.block_size
    );
    Ok(Some(Volume {
        root: main.root,
        joliet: main.joliet,
        catalog,
    }))
}

struct IsoReader {
    joliet: bool,
    susp_skip: Option<usize>,
    root_extent: u32,
}

impl IsoReader {
    fn list(&mut self, image: &mut Image, folder: IsoFolder) -> Result<Vec<Child<IsoFolder>>> {
        let start = u64::from(folder.extent) * SECTOR;
        let size = u64::from(folder.size);
        image.ensure_inside(
            Extent {
                offset: start,
                length: size,
            },
            "an ISO 9660 folder",
        )?;
        image.charge_directory(size)?;
        let mut children = Vec::new();
        let mut pending: Option<(Vec<u8>, u8, Node)> = None;
        let mut sector = vec![0u8; SECTOR_BYTES];
        let mut first = true;
        let mut done = 0u64;
        while done < size {
            let length = (size - done).min(SECTOR);
            let bytes = &mut sector[..length as usize];
            image.read_exact_at(bytes, start + done, "folders")?;
            let mut at = 0usize;
            while at < bytes.len() {
                if bytes[at] == 0 {
                    ensure!(
                        bytes[at..].iter().all(|byte| *byte == 0),
                        "ISO 9660 folder has data after the end of its records in a sector"
                    );
                    break;
                }
                let record = directory_record(&bytes[at..])?;
                let raw = &bytes[at..at + record.length];
                at += record.length;
                if first && folder.extent == self.root_extent && self.susp_skip.is_none() {
                    self.susp_skip = sharing_protocol(record.system_use(raw));
                }
                first = false;
                if record.is_system_item(raw) {
                    ensure!(
                        pending.is_none(),
                        "ISO 9660 file ends without its last extent"
                    );
                    continue;
                }
                self.add(image, &record, raw, &mut pending, &mut children)?;
                ensure_folder_fits(children.len())?;
            }
            done += length;
        }
        ensure!(
            pending.is_none(),
            "ISO 9660 file ends without its last extent"
        );
        Ok(children)
    }

    fn add(
        &mut self,
        image: &mut Image,
        record: &Record,
        raw: &[u8],
        pending: &mut Option<(Vec<u8>, u8, Node)>,
        children: &mut Vec<Child<IsoFolder>>,
    ) -> Result<()> {
        let id = record.id(raw);
        let extent = Extent {
            offset: u64::from(record.extent) * SECTOR,
            length: u64::from(record.size),
        };
        if extent.length > 0 {
            image.ensure_inside(extent, "an ISO 9660 extent")?;
        }
        let more = record.flags & FLAG_MORE_EXTENTS != 0;
        if let Some((first_id, flags, node)) = pending {
            ensure!(
                first_id.as_slice() == id && *flags == record.flags | FLAG_MORE_EXTENTS,
                "ISO 9660 file ends without its last extent"
            );
            node.size += extent.length;
            node.extents.push(extent);
            if !more {
                let (_, _, node) = pending.take().unwrap();
                children.push(Child { folder: None, node });
            }
            return Ok(());
        }
        let mut node = Node {
            parent: NO_PARENT,
            name: self.name(id, record, raw)?.into(),
            kind: if record.is_directory() {
                Kind::Directory
            } else {
                Kind::File
            },
            mode: if record.is_directory() {
                DIRECTORY_MODE
            } else {
                FILE_MODE
            },
            modified: recording_time(record.date),
            size: if record.is_directory() {
                0
            } else {
                extent.length
            },
            extents: if record.is_directory() {
                Extents::None
            } else {
                Extents::One(extent)
            },
        };
        if let Some(skip) = self.susp_skip {
            self.apply_rock_ridge(image, &mut node, record, raw, skip)?;
        }
        if record.is_directory() {
            ensure!(!more, "ISO 9660 folder {:?} has several extents", node.name);
            children.push(Child {
                folder: Some(IsoFolder {
                    extent: record.extent,
                    size: record.size,
                }),
                node,
            });
        } else if more {
            *pending = Some((id.to_vec(), record.flags, node));
        } else {
            children.push(Child { folder: None, node });
        }
        Ok(())
    }

    fn name(&self, id: &[u8], record: &Record, raw: &[u8]) -> Result<String> {
        let name = if self.joliet {
            joliet_name(id)?
        } else {
            let from_rock_ridge = match self.susp_skip {
                Some(skip) => rock_ridge_area(record, raw, skip).and_then(|area| {
                    let found = rock_ridge(area).ok()?;
                    found.name.map(|(from, to)| area[from..to].to_vec())
                }),
                None => None,
            };
            let bytes = from_rock_ridge.unwrap_or_else(|| id.to_vec());
            ensure!(!bytes.contains(&0), "ISO 9660 name holds NUL");
            String::from_utf8(bytes).context("ISO 9660 name is not UTF-8")?
        };
        let name = without_version(&name);
        ensure_single_name("ISO 9660", name)?;
        Ok(name.to_owned())
    }

    fn apply_rock_ridge(
        &mut self,
        image: &mut Image,
        node: &mut Node,
        record: &Record,
        raw: &[u8],
        skip: usize,
    ) -> Result<()> {
        let Some(area) = rock_ridge_area(record, raw, skip) else {
            return Ok(());
        };
        let found = rock_ridge(area)?;
        ensure!(
            found.names <= 1 && found.modes <= 1 && found.links <= 1 && found.continuations <= 1,
            "Rock Ridge record {:?} repeats an entry, which 7-Zip reads only once; {BACKEND_HINT}",
            node.name
        );
        ensure!(
            !found.relocated,
            "Rock Ridge record {:?} relocates a folder, which 7-Zip does not follow; {BACKEND_HINT}",
            node.name
        );
        self.ensure_continuations_hold_no_fields(image, found.continuation, &node.name)?;
        let Some(mode) = found.mode else {
            ensure!(
                found.link.is_none(),
                "Rock Ridge link {:?} has no mode",
                node.name
            );
            return Ok(());
        };
        let kind = mode & TYPE_MASK;
        let fits = match node.kind {
            Kind::Directory => kind == MODE_DIRECTORY,
            _ if found.link.is_some() => kind == MODE_SYMLINK,
            _ => matches!(kind, MODE_REGULAR | MODE_SYMLINK),
        };
        ensure!(
            fits,
            "Rock Ridge entry {:?} has a mode of another file type",
            node.name
        );
        node.mode = mode & 0o777;
        if kind != MODE_SYMLINK {
            return Ok(());
        }
        let target = match found.link {
            Some((from, to)) => {
                ensure!(node.size == 0, "Rock Ridge link {:?} has data", node.name);
                symlink_target(&area[from..to])?
            }
            None => {
                ensure!(
                    node.size <= crate::MAX_LINK_TARGET && node.extents.as_slice().len() == 1,
                    "Rock Ridge link {:?} keeps a target longer than {} bytes",
                    node.name,
                    crate::MAX_LINK_TARGET
                );
                let mut target = vec![0u8; node.size as usize];
                image.read_exact_at(
                    &mut target,
                    node.extents.as_slice()[0].offset,
                    "link targets",
                )?;
                target
            }
        };
        let target = String::from_utf8(target).context("Rock Ridge link is not UTF-8")?;
        node.kind = Kind::Symlink(target.into());
        node.size = 0;
        node.extents = Extents::None;
        Ok(())
    }

    fn ensure_continuations_hold_no_fields(
        &mut self,
        image: &mut Image,
        mut next: Option<Continuation>,
        name: &str,
    ) -> Result<()> {
        let mut seen = HashSet::new();
        while let Some(area) = next {
            ensure!(
                seen.len() < MAX_CONTINUATIONS && seen.insert((area.block, area.offset)),
                "Rock Ridge continuation areas of {name:?} loop or chain more than {MAX_CONTINUATIONS} deep"
            );
            ensure!(
                u64::from(area.offset) + u64::from(area.length) <= SECTOR,
                "Rock Ridge continuation area of {name:?} crosses a block"
            );
            let offset = u64::from(area.block) * SECTOR + u64::from(area.offset);
            image.ensure_inside(
                Extent {
                    offset,
                    length: u64::from(area.length),
                },
                "a Rock Ridge continuation area",
            )?;
            image.charge_directory(u64::from(area.length))?;
            let mut bytes = vec![0u8; area.length as usize];
            image.read_exact_at(&mut bytes, offset, "Rock Ridge continuation areas")?;
            let found = rock_ridge(&bytes)?;
            ensure!(
                found.names == 0 && found.modes == 0 && found.links == 0 && !found.relocated,
                "Rock Ridge entry {name:?} keeps a name, mode or link in a continuation area, which 7-Zip does not read; {BACKEND_HINT}"
            );
            ensure!(
                found.continuations <= 1,
                "Rock Ridge continuation area of {name:?} repeats CE"
            );
            next = found.continuation;
        }
        Ok(())
    }
}

fn rock_ridge_area<'a>(record: &Record, raw: &'a [u8], skip: usize) -> Option<&'a [u8]> {
    record.system_use(raw).get(skip..)
}

const SP_LENGTH: usize = 7;
const XA_HEAD: usize = 14;

fn sharing_protocol(system_use: &[u8]) -> Option<usize> {
    let at = |offset: usize| {
        let entry = system_use.get(offset..offset + SP_LENGTH)?;
        (entry[..6] == [b'S', b'P', 7, 1, 0xBE, 0xEF]).then(|| usize::from(entry[6]))
    };
    at(0).or_else(|| {
        (system_use.len() >= XA_HEAD + SP_LENGTH)
            .then(|| at(XA_HEAD))
            .flatten()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BootEntry {
    bootable: bool,
    media: u8,
    sectors: u16,
    load: u32,
    vendor: [u8; 20],
}

fn validation_entry(entry: &[u8; CATALOG_ENTRY]) -> bool {
    let sum = entry.chunks_exact(2).fold(0u16, |sum, word| {
        sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]))
    });
    entry[0] == 1
        && entry[2] == 0
        && entry[3] == 0
        && entry[30] == 0x55
        && entry[31] == 0xAA
        && sum == 0
}

fn boot_entry(entry: &[u8; CATALOG_ENTRY]) -> Option<BootEntry> {
    if !matches!(entry[0], 0x88 | 0x00) || entry[5] != 0 {
        return None;
    }
    let mut vendor = [0u8; 20];
    vendor.copy_from_slice(&entry[12..32]);
    Some(BootEntry {
        bootable: entry[0] == 0x88,
        media: entry[1],
        sectors: u16::from_le_bytes([entry[6], entry[7]]),
        load: u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]),
        vendor,
    })
}

const MEDIA_NAMES: [&str; 5] = ["NoEmul", "1.2M", "1.44M", "2.88M", "HardDisk"];
const EXTENSION_FOLLOWS: u8 = 0x20;

impl BootEntry {
    fn name(&self) -> String {
        let mut name = String::from(if self.bootable { "Boot-" } else { "NotBoot-" });
        match MEDIA_NAMES.get(usize::from(self.media)) {
            Some(media) => name.push_str(media),
            None => name.push_str(&self.media.to_string()),
        }
        if self.vendor[0] == 1 && self.vendor[1..].iter().all(|byte| *byte <= 0x7F) {
            name.push('-');
            name.extend(self.vendor[1..].iter().take_while(|byte| **byte != 0).map(
                |byte| match byte {
                    b'\\' | b'/' => '_',
                    _ => char::from(*byte),
                },
            ));
        }
        name.push_str(".img");
        name
    }

    fn declared_size(&self) -> u64 {
        match self.media {
            1 => 1200 << 10,
            2 => 1440 << 10,
            3 => 2880 << 10,
            _ => u64::from(self.sectors) * 512,
        }
    }
}

fn boot_entries(catalog: &[u8]) -> Result<Vec<BootEntry>> {
    let entry_at = |index: usize| -> Result<&[u8; CATALOG_ENTRY]> {
        catalog
            .get(index * CATALOG_ENTRY..(index + 1) * CATALOG_ENTRY)
            .map(|entry| entry.try_into().unwrap())
            .context("El Torito boot catalog runs past the image")
    };
    ensure!(
        validation_entry(entry_at(0)?),
        "El Torito boot catalog has no valid validation entry"
    );
    let initial = boot_entry(entry_at(1)?).context("El Torito initial entry is malformed")?;
    let mut entries = vec![initial];
    let mut index = 2;
    loop {
        let header = entry_at(index)?;
        if !matches!(header[0], 0x90 | 0x91) {
            break;
        }
        let count = u16::from_le_bytes([header[2], header[3]]);
        index += 1;
        for _ in 0..count {
            let entry =
                boot_entry(entry_at(index)?).context("El Torito section entry is malformed")?;
            index += 1;
            if entry.media & EXTENSION_FOLLOWS != 0 {
                let mut extensions = 0;
                loop {
                    let extension = entry_at(index)?;
                    ensure!(
                        extension[0] == 0x44,
                        "El Torito extension entry is malformed"
                    );
                    index += 1;
                    extensions += 1;
                    ensure!(
                        extensions <= MAX_BOOT_EXTENSIONS,
                        "El Torito entry has more than {MAX_BOOT_EXTENSIONS} extensions"
                    );
                    if extension[1] & EXTENSION_FOLLOWS == 0 {
                        break;
                    }
                }
            }
            entries.push(entry);
            ensure!(
                entries.len() <= MAX_BOOT_ENTRIES,
                "El Torito boot catalog has more than {MAX_BOOT_ENTRIES} entries"
            );
        }
        if header[0] == 0x91 {
            break;
        }
    }
    Ok(entries)
}

fn boot_nodes(image: &mut Image, catalog_sector: u32, nodes: &mut Vec<Node>) -> Result<()> {
    let start = u64::from(catalog_sector) * SECTOR;
    ensure!(
        start < image.length,
        "El Torito boot catalog lies past the end of the image"
    );
    let length = (image.length - start).min(CATALOG_SECTORS * SECTOR);
    let mut catalog = vec![0u8; length as usize];
    image.read_exact_at(&mut catalog, start, "El Torito boot catalog")?;
    let entries = boot_entries(&catalog)?;
    let expand = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.sectors <= 1 && entry.media == 0)
        .max_by_key(|(index, entry)| (entry.load, *index))
        .map(|(index, _)| index);
    let folder = nodes.len() as u32;
    nodes.push(Node {
        parent: NO_PARENT,
        name: BOOT_FOLDER.into(),
        kind: Kind::Directory,
        mode: DIRECTORY_MODE,
        modified: None,
        size: 0,
        extents: Extents::None,
    });
    let numbered = entries.len() > 1;
    for (index, entry) in entries.iter().enumerate() {
        let offset = u64::from(entry.load) * SECTOR;
        ensure!(
            offset < image.length,
            "El Torito boot image lies past the end of the image"
        );
        let rest = image.length - offset;
        let declared = entry.declared_size();
        let size = if Some(index) == expand || rest < declared {
            rest
        } else {
            declared
        };
        let name = if numbered {
            format!("{}-{}", index + 1, entry.name())
        } else {
            entry.name()
        };
        nodes.push(Node {
            parent: folder,
            name: name.into(),
            kind: Kind::File,
            mode: FILE_MODE,
            modified: None,
            size,
            extents: Extents::One(Extent {
                offset,
                length: size,
            }),
        });
    }
    Ok(())
}

fn read_iso(image: &mut Image, volume: Volume) -> Result<Catalog> {
    let mut reader = IsoReader {
        joliet: volume.joliet,
        susp_skip: None,
        root_extent: volume.root.extent,
    };
    let root = IsoFolder {
        extent: volume.root.extent,
        size: volume.root.size,
    };
    let mut nodes = Vec::new();
    let walked = walk_folders(
        root,
        MAX_ISO_DEPTH,
        tree::MAX_ENTRIES,
        &mut nodes,
        |folder| reader.list(image, folder),
    )?;
    match walked {
        Walk::Tree => {}
        Walk::Cycle | Walk::Shared => bail!("an ISO 9660 folder is reached twice"),
        Walk::TooDeep => bail!("ISO 9660 folders nest more than {MAX_ISO_DEPTH} deep"),
        Walk::TooMany => bail!("the image has more than {} entries", tree::MAX_ENTRIES),
    }
    if let Some(catalog) = volume.catalog {
        boot_nodes(image, catalog, &mut nodes)?;
    }
    Ok(Catalog {
        format: "ISO",
        nodes,
    })
}

fn ensure_names_are_distinct(catalog: &Catalog) -> Result<()> {
    let mut order: Vec<u32> = (0..catalog.nodes.len() as u32).collect();
    let key = |index: &u32| {
        let node = &catalog.nodes[*index as usize];
        (node.parent, &node.name)
    };
    order.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
    for pair in order.windows(2) {
        ensure!(
            key(&pair[0]) != key(&pair[1]),
            "{} has two entries named {:?} in one folder",
            catalog.format,
            catalog.nodes[pair[0] as usize].name
        );
    }
    Ok(())
}

fn read_catalog(image: &mut Image) -> Result<Catalog> {
    let catalog = match crate::udf::read(image)? {
        crate::udf::Opened::Tree(catalog) => catalog,
        opened => {
            let volume = read_volumes(image)?;
            match (opened, volume) {
                (_, Some(volume)) => read_iso(image, volume)?,
                (crate::udf::Opened::Unsupported(reason), None) => {
                    bail!("UDF image is not one 7-Zip opens: {reason}")
                }
                _ => bail!("not an ISO 9660 or UDF image"),
            }
        }
    };
    ensure_names_are_distinct(&catalog)?;
    Ok(catalog)
}

struct ExtentReader<'a> {
    image: &'a mut Image,
    extents: std::slice::Iter<'a, Extent>,
    offset: u64,
    left: u64,
}

impl Read for ExtentReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.left == 0 {
            match self.extents.next() {
                Some(extent) => {
                    self.offset = extent.offset;
                    self.left = extent.length;
                }
                None => return Ok(0),
            }
        }
        let want = (buffer.len() as u64).min(self.left) as usize;
        let read = self.image.file.read_at(&mut buffer[..want], self.offset)?;
        self.offset += read as u64;
        self.left -= read as u64;
        Ok(read)
    }
}

fn extract(catalog: &Catalog, image: &mut Image, tree: &mut Tree) -> Result<()> {
    let mut folders: HashMap<u32, String> = HashMap::new();
    for (index, node) in catalog.nodes.iter().enumerate() {
        tree.check()?;
        let path = match node.parent {
            NO_PARENT => node.name.to_string(),
            parent => format!("{}/{}", folders[&parent], node.name),
        };
        match &node.kind {
            Kind::Directory => {
                tree.directory(&path, node.mode, node.modified)?;
                folders.insert(index as u32, path);
            }
            Kind::Symlink(target) => tree.symlink(&path, target.to_string())?,
            Kind::File => {
                let mut data = ExtentReader {
                    image,
                    extents: node.extents.as_slice().iter(),
                    offset: 0,
                    left: 0,
                };
                tree.file(&path, node.size, node.mode, node.modified, &mut data)?;
            }
        }
    }
    Ok(())
}

/// Returns whether the file holds a well-formed ISO 9660 volume descriptor
/// at sector 16, or a UDF anchor at sector 256 of 2048 or 512 bytes.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut image = Image::open(path)?;
    let mut head = [0u8; DESCRIPTOR_HEAD];
    if image.length >= DESCRIPTORS_START * SECTOR + head.len() as u64 {
        image.read_exact_at(&mut head, DESCRIPTORS_START * SECTOR, "volume descriptors")?;
        if volume_descriptor(&head)
            .is_ok_and(|descriptor| matches!(descriptor.kind, 0 | 1 | 2 | 255))
        {
            return Ok(true);
        }
    }
    crate::udf::has_anchor(&mut image)
}

/// Verify and optionally extract an ISO 9660 or UDF disc image into a new
/// directory, in the tree `7z x` extracts. A UDF file system that 7-Zip
/// opens wins over ISO 9660, and a Joliet volume wins over the primary one,
/// whose names may come from Rock Ridge. El Torito boot images extract
/// under `[BOOT]` beside an ISO 9660 tree. It refuses a disc 7-Zip would
/// read differently than it does, and suggests the 7-Zip backend.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut image = Image::open(path)?;
    let catalog = read_catalog(&mut image)?;
    tree::extract(catalog.format, path, destination, options, |tree| {
        extract(&catalog, &mut image, tree)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct TestFolder(u8);

    impl Folder for TestFolder {
        type Key = u8;
        fn key(&self) -> u8 {
            self.0
        }
    }

    type Graph = Vec<Vec<Option<u8>>>;

    fn node(name: String) -> Node {
        Node {
            parent: NO_PARENT,
            name: name.into(),
            kind: Kind::File,
            mode: FILE_MODE,
            modified: None,
            size: 0,
            extents: Extents::None,
        }
    }

    fn label(folder: u8, slot: usize) -> String {
        format!("{folder}.{slot}")
    }

    struct Reference<'a> {
        graph: &'a Graph,
        max_depth: usize,
        open: Vec<u8>,
        read: Vec<u8>,
        nodes: Vec<(u32, String)>,
    }

    impl Reference<'_> {
        fn visit(
            &mut self,
            folder: u8,
            parent: u32,
            depth: usize,
        ) -> std::result::Result<(), Walk> {
            for (slot, child) in self.graph[usize::from(folder)].iter().enumerate() {
                let index = self.nodes.len() as u32;
                self.nodes.push((parent, label(folder, slot)));
                let Some(target) = *child else { continue };
                if self.open.contains(&target) {
                    return Err(Walk::Cycle);
                }
                if self.read.contains(&target) {
                    return Err(Walk::Shared);
                }
                if depth + 1 > self.max_depth {
                    return Err(Walk::TooDeep);
                }
                self.open.push(target);
                self.visit(target, index, depth + 1)?;
                self.open.retain(|known| *known != target);
                self.read.push(target);
            }
            Ok(())
        }
    }

    fn children_lists(folders: u8) -> Vec<Vec<Option<u8>>> {
        let choices: Vec<Option<u8>> = std::iter::once(None)
            .chain((0..folders).map(Some))
            .collect();
        let mut lists = vec![Vec::new()];
        for first in &choices {
            lists.push(vec![*first]);
            for second in &choices {
                lists.push(vec![*first, *second]);
            }
        }
        lists
    }

    #[test]
    fn every_small_folder_graph_walks_as_a_recursive_reference_does() {
        const FOLDERS: u8 = 4;
        let lists = children_lists(FOLDERS);
        let mut cases = 0usize;
        let mut outcomes = HashMap::new();
        for code in 0..lists.len().pow(u32::from(FOLDERS)) {
            let mut rest = code;
            let graph: Graph = (0..FOLDERS)
                .map(|_| {
                    let list = lists[rest % lists.len()].clone();
                    rest /= lists.len();
                    list
                })
                .collect();
            for max_depth in [1, 2, 8] {
                let mut reference = Reference {
                    graph: &graph,
                    max_depth,
                    open: vec![0],
                    read: Vec::new(),
                    nodes: Vec::new(),
                };
                let expected = match reference.visit(0, NO_PARENT, 0) {
                    Ok(()) => Walk::Tree,
                    Err(outcome) => outcome,
                };
                let mut nodes = Vec::new();
                let walked =
                    walk_folders(TestFolder(0), max_depth, usize::MAX, &mut nodes, |folder| {
                        Ok(graph[usize::from(folder.0)]
                            .iter()
                            .enumerate()
                            .map(|(slot, child)| Child {
                                folder: child.map(TestFolder),
                                node: node(label(folder.0, slot)),
                            })
                            .collect())
                    })
                    .unwrap();
                assert_eq!(walked, expected, "{graph:?} at depth {max_depth}");
                if walked == Walk::Tree {
                    let got: Vec<(u32, String)> = nodes
                        .iter()
                        .map(|node| (node.parent, node.name.to_string()))
                        .collect();
                    assert_eq!(got, reference.nodes, "{graph:?}");
                    for (index, node) in nodes.iter().enumerate() {
                        assert!(node.parent == NO_PARENT || (node.parent as usize) < index);
                    }
                }
                *outcomes.entry(format!("{walked:?}")).or_insert(0usize) += 1;
                cases += 1;
            }
        }
        assert!(cases > 2_000_000, "{cases}");
        assert_eq!(outcomes.len(), 4, "{outcomes:?}");
    }

    #[test]
    fn the_walk_stops_at_its_node_limit() {
        let mut nodes = Vec::new();
        let walked = walk_folders(TestFolder(0), 8, 3, &mut nodes, |_| {
            Ok((0..5)
                .map(|slot| Child {
                    folder: None,
                    node: node(label(0, slot)),
                })
                .collect())
        })
        .unwrap();
        assert_eq!(walked, Walk::TooMany);
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn names_drop_only_a_first_version_and_one_dot_as_7zip_does() {
        for (stored, shown) in [
            ("NAME.;1", "NAME"),
            ("NAME;1", "NAME"),
            ("NAME;2", "NAME;2"),
            ("A.B.;1", "A.B"),
            ("DOT..;1", "DOT."),
            ("TRAIL.", "TRAIL"),
            (";1", ""),
            ("plain", "plain"),
        ] {
            assert_eq!(without_version(stored), shown, "{stored}");
        }
    }

    #[test]
    fn link_components_join_as_7zip_joins_them() {
        for (components, target) in [
            (
                &[4u8, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'][..],
                &b"../hello"[..],
            ),
            (&[0, 1, b'a', 0, 1, b'b'], b"a/b"),
            (&[8, 0, 0, 3, b'e', b't', b'c'], b"/etc"),
            (&[2, 0, 0, 1, b'x'], b"./x"),
            (&[0, 1, b'a', 4, 0], b"a/../"),
        ] {
            assert_eq!(
                symlink_target(components).unwrap(),
                target,
                "{components:?}"
            );
        }
        for components in [
            &[1u8, 1, b'a'][..],
            &[0, 0],
            &[4, 1, b'a'],
            &[0, 5, b'a'],
            &[0],
            &[3, 0],
        ] {
            assert!(symlink_target(components).is_err(), "{components:?}");
        }
    }

    #[test]
    fn recording_dates_read_as_utc_seconds() {
        assert_eq!(civil_seconds(2024, [1, 2, 3, 4, 5], 0), Some(1_704_164_645));
        assert_eq!(
            civil_seconds(2024, [1, 2, 3, 4, 5], 60),
            Some(1_704_164_645 - 3600)
        );
        assert_eq!(civil_seconds(1970, [1, 1, 0, 0, 0], 0), Some(0));
        assert_eq!(civil_seconds(1969, [12, 31, 23, 59, 59], 0), Some(-1));
        assert_eq!(civil_seconds(2024, [13, 1, 0, 0, 0], 0), None);
        assert_eq!(civil_seconds(2024, [2, 0, 0, 0, 0], 0), None);
        assert_eq!(
            recording_fields([124, 1, 2, 3, 4, 5, 4]),
            (2024, [1, 2, 3, 4, 5], 60)
        );
        assert_eq!(recording_fields([0, 1, 1, 0, 0, 0, 0x80]).2, -128 * 15);
    }

    #[test]
    fn rock_ridge_entries_are_read_in_place_and_padding_ends_them() {
        let mut area = Vec::new();
        area.extend_from_slice(b"NM\x07\x01\x00ab");
        area.extend_from_slice(b"PX\x24\x01");
        area.extend_from_slice(&0o100644u32.to_le_bytes());
        area.extend_from_slice(&0o100644u32.to_be_bytes());
        area.extend_from_slice(&[0; 24]);
        area.extend_from_slice(b"SL\x08\x01\x00\x00\x01c");
        area.extend_from_slice(&[0, 0, 0, 0, 0]);
        let found = rock_ridge(&area).unwrap();
        let (from, to) = found.name.unwrap();
        assert_eq!(&area[from..to], b"ab");
        assert_eq!(found.mode, Some(0o100644));
        let (from, to) = found.link.unwrap();
        assert_eq!(&area[from..to], &[0, 1, b'c']);
        area.push(1);
        assert!(rock_ridge(&area).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn prefix<const N: usize>(bytes: &[u8; N]) -> &[u8] {
        let length: usize = kani::any();
        kani::assume(length <= N);
        &bytes[..length]
    }

    #[kani::proof]
    fn directory_records_never_panic_and_stay_inside_their_bytes() {
        let bytes: [u8; 40] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(record) = directory_record(bytes) {
            assert!(record.length <= bytes.len());
            assert!(record.id_length >= 1);
            assert!(RECORD_HEAD + record.id_length <= record.system_use);
            assert!(record.system_use <= record.length);
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn volume_descriptors_never_panic_and_hold_a_whole_root_record() {
        let bytes: [u8; DESCRIPTOR_HEAD] = kani::any();
        if let Ok(descriptor) = volume_descriptor(&bytes) {
            if matches!(descriptor.kind, 1 | 2) {
                assert!(descriptor.root.length == ROOT_RECORD_LENGTH);
                assert!(descriptor.root.system_use <= ROOT_RECORD_LENGTH);
                assert!(descriptor.root.is_directory());
            }
            assert!(!descriptor.joliet || descriptor.kind == 2);
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn rock_ridge_entries_never_panic_and_name_ranges_inside_their_area() {
        let bytes: [u8; 16] = kani::any();
        let area = prefix(&bytes);
        if let Ok(found) = rock_ridge(area) {
            for (from, to) in [found.name, found.link].into_iter().flatten() {
                assert!(from <= to && to <= area.len());
            }
            assert!(found.names + found.modes + found.links + found.continuations <= 4);
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn link_targets_never_panic_and_stay_within_twice_their_components() {
        let bytes: [u8; 8] = kani::any();
        let components = prefix(&bytes);
        if let Ok(target) = symlink_target(components) {
            assert!(target.len() <= 2 * components.len());
        }
    }

    #[kani::proof]
    fn sharing_protocol_is_read_only_from_a_whole_entry() {
        let bytes: [u8; 24] = kani::any();
        let system_use = prefix(&bytes);
        if sharing_protocol(system_use).is_some() {
            assert!(system_use.len() >= SP_LENGTH);
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn boot_entries_never_panic_and_their_sizes_fit_their_media() {
        let bytes: [u8; CATALOG_ENTRY] = kani::any();
        let _ = validation_entry(&bytes);
        if let Some(entry) = boot_entry(&bytes) {
            assert!(entry.declared_size() <= (2880 << 10).max(0xFFFF * 512));
        }
    }

    #[kani::proof]
    fn civil_times_never_overflow() {
        let date: [u8; 7] = kani::any();
        let (year, fields, offset) = recording_fields(date);
        assert!((1900..=2155).contains(&year));
        let _ = civil_seconds(year, fields, offset);
        let _ = civil_seconds(kani::any(), kani::any(), kani::any());
    }
}
