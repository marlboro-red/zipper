//! In-process extraction of Compound File Binary files: Windows Installer
//! packages, patches and merge modules, and compound documents such as
//! `.doc`, `.xls` and `.msg`. See `docs/reference/msi-reader.md` for what it
//! accepts and writes.
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::{self, BACKEND_HINT};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, SystemTime},
};

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const HEADER: usize = 512;
const HEADER_DIFAT: usize = 109;
const ENTRY: usize = 128;
const NAME_UNITS: usize = 32;
const MINOR_VERSION: u16 = 0x3E;
const LITTLE_ENDIAN: u16 = 0xFFFE;
const MINI_SHIFT: u32 = 6;
const MINI_SECTOR: u64 = 1 << MINI_SHIFT;
const MINI_STREAM_CUTOFF: u64 = 4096;
const FREE: u32 = 0xFFFF_FFFF;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
const FAT_SECTOR: u32 = 0xFFFF_FFFD;
const DIFAT_SECTOR: u32 = 0xFFFF_FFFC;
const LAST_REGULAR: u32 = 0xFFFF_FFFA;
const NO_ENTRY: u32 = 0xFFFF_FFFF;
const NO_PARENT: u32 = u32::MAX;
const MAX_ENTRIES: usize = 1 << 20;
// EXTERNAL(7-Zip ComHandler): an item below a storage at level 256 gets a [LONG_PATH]
// folder in place of its real path, where the root's children are level 1.
const MAX_LEVEL: u32 = 256;
const PATH_BYTES_PER_DIRECTORY_BYTE: u64 = 2;
const MIN_PATH_BYTES: u64 = 1024 * 1024;
const MAX_PATH_BYTES: u64 = 64 * 1024 * 1024;
const MSI_FIRST: u16 = 0x3800;
const MSI_TABLE: u16 = 0x4840;
const MSI_CHARACTERS: &[u8; 64] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._";
const CAB_SIGNATURE: &[u8; 8] = b"MSCF\0\0\0\0";
const FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;
const COPY_BUFFER: usize = 1 << 20;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const FILETIME_TO_UNIX_SECONDS: u64 = 11_644_473_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    shift: u32,
    directory_sectors: u32,
    fat_sectors: u32,
    directory_start: u32,
    mini_fat_start: u32,
    mini_fat_sectors: u32,
    difat_start: u32,
    difat_sectors: u32,
    difat: [u32; HEADER_DIFAT],
}

impl Header {
    fn sector_size(&self) -> u64 {
        1 << self.shift
    }

    fn entries_per_sector(&self) -> usize {
        1 << (self.shift - 2)
    }

    fn fat_entries(&self) -> u32 {
        self.fat_sectors << (self.shift - 2)
    }

    fn mini_fat_entries(&self) -> u32 {
        self.mini_fat_sectors << (self.shift - 2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Empty,
    Storage,
    Stream,
    Root,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryEntry {
    name: [u16; NAME_UNITS],
    name_units: u8,
    kind: Kind,
    left: u32,
    right: u32,
    child: u32,
    modified: u64,
    start: u32,
    size: u64,
}

impl DirectoryEntry {
    fn is_storage(&self) -> bool {
        matches!(self.kind, Kind::Storage | Kind::Root)
    }

    fn name(&self) -> &[u16] {
        &self.name[..usize::from(self.name_units)]
    }

    fn in_mini_stream(&self) -> bool {
        self.kind == Kind::Stream && self.size < MINI_STREAM_CUTOFF
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

// EXTERNAL(7-Zip ComHandler): CDatabase::Open makes each of these header checks.
fn header(bytes: &[u8; HEADER]) -> Field<Header> {
    field_check(bytes[..8] == SIGNATURE, "not a compound file")?;
    field_check(
        u16_at(bytes, 0x18) == MINOR_VERSION,
        "compound file header has an unknown minor version",
    )?;
    let major = u16_at(bytes, 0x1A);
    field_check(
        major == 3 || major == 4,
        "compound file header has an unknown major version",
    )?;
    field_check(
        u16_at(bytes, 0x1C) == LITTLE_ENDIAN,
        "compound file is not little-endian",
    )?;
    let shift = u32::from(u16_at(bytes, 0x1E));
    field_check(
        shift == 3 * u32::from(major),
        "compound file sector size does not match its version",
    )?;
    field_check(
        u32::from(u16_at(bytes, 0x20)) == MINI_SHIFT,
        "compound file mini sectors are not 64 bytes",
    )?;
    field_check(
        u16_at(bytes, 0x22) == 0 && u32_at(bytes, 0x24) == 0,
        "compound file header reserved fields are not zero",
    )?;
    let directory_sectors = u32_at(bytes, 0x28);
    field_check(
        (major == 4) == (directory_sectors != 0),
        "compound file directory sector count does not match its version",
    )?;
    field_check(
        directory_sectors <= (1 << 30) >> (shift - 9),
        "compound file directory has more sectors than a file can hold",
    )?;
    let fat_sectors = u32_at(bytes, 0x2C);
    field_check(
        shift != 9 || fat_sectors < 1 << 15,
        "compound file with 512-byte sectors has a FAT for more than 2 GiB",
    )?;
    field_check(
        fat_sectors != 0 && fat_sectors <= u32::MAX >> (shift - 2),
        "compound file FAT has an impossible size",
    )?;
    field_check(
        u64::from(u32_at(bytes, 0x38)) == MINI_STREAM_CUTOFF,
        "compound file mini stream cutoff is not 4096 bytes",
    )?;
    let mini_fat_sectors = u32_at(bytes, 0x40);
    field_check(
        mini_fat_sectors <= u32::MAX >> (shift - 2),
        "compound file mini FAT has an impossible size",
    )?;
    let difat_sectors = u32_at(bytes, 0x48);
    field_check(
        difat_sectors <= (1 << 30) >> (shift - 3) >> (shift - 3),
        "compound file DIFAT has more sectors than a file can hold",
    )?;
    let mut difat = [0; HEADER_DIFAT];
    for (index, sector) in difat.iter_mut().enumerate() {
        *sector = u32_at(bytes, 0x4C + 4 * index);
    }
    Ok(Header {
        shift,
        directory_sectors,
        fat_sectors,
        directory_start: u32_at(bytes, 0x30),
        mini_fat_start: u32_at(bytes, 0x3C),
        mini_fat_sectors,
        difat_start: u32_at(bytes, 0x44),
        difat_sectors,
        difat,
    })
}

// SPEC(MS-CFB 2.6.1): the name is at most 31 UTF-16 units and a NUL, its length counts the
// NUL in bytes, and version 3 files ignore the high half of the stream size.
fn directory_entry(bytes: &[u8; ENTRY], shift: u32) -> Field<DirectoryEntry> {
    let kind = match bytes[66] {
        0 => Kind::Empty,
        1 => Kind::Storage,
        2 => Kind::Stream,
        5 => Kind::Root,
        _ => return Err(BadField("compound file entry has an unknown type")),
    };
    let mut name = [0u16; NAME_UNITS];
    let mut name_units = NAME_UNITS;
    for (index, unit) in name.iter_mut().enumerate() {
        *unit = u16_at(bytes, 2 * index);
        if *unit == 0 {
            name_units = index;
            break;
        }
    }
    if kind != Kind::Empty {
        field_check(
            name_units < NAME_UNITS && usize::from(u16_at(bytes, 64)) == 2 * name_units + 2,
            "compound file entry name does not match its length",
        )?;
    }
    field_check(bytes[67] < 2, "compound file entry has an unknown color")?;
    let high = if shift == 12 { u32_at(bytes, 124) } else { 0 };
    let size = u64::from(u32_at(bytes, 120)) | u64::from(high) << 32;
    field_check(
        u64::from(high) < 1 << shift,
        "compound file entry is larger than its FAT can address",
    )?;
    Ok(DirectoryEntry {
        name,
        name_units: name_units as u8,
        kind,
        left: u32_at(bytes, 68),
        right: u32_at(bytes, 72),
        child: u32_at(bytes, 76),
        modified: u64::from(u32_at(bytes, 108)) | u64::from(u32_at(bytes, 112)) << 32,
        start: u32_at(bytes, 116),
        size,
    })
}

fn walk_chain(
    table: &[u32],
    start: u32,
    count: u64,
    mut visit: impl FnMut(u32) -> Field<()>,
) -> Field<()> {
    field_check(
        count <= table.len() as u64,
        "compound file stream is longer than its allocation table",
    )?;
    let mut sector = start;
    for _ in 0..count {
        let next = *table
            .get(sector as usize)
            .ok_or(BadField("compound file chain leaves its allocation table"))?;
        visit(sector)?;
        sector = next;
    }
    field_check(
        sector == END_OF_CHAIN,
        "compound file chain does not end where its stream ends",
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    Free,
    Chain,
    Fat,
    Difat,
}

fn claim_start(used: &mut [Use], sector: u32) -> Field<()> {
    match used.get_mut(sector as usize) {
        Some(state @ Use::Free) => {
            *state = Use::Chain;
            Ok(())
        }
        Some(_) => Err(BadField(
            "compound file chain starts at a sector already in use",
        )),
        None => Err(BadField(
            "compound file chain starts past the end of the file",
        )),
    }
}

// EXTERNAL(7-Zip ComHandler): CDatabase::Open refuses a FAT that links twice to one sector.
fn claim_links(fat: &[u32], used: &mut [Use]) -> Field<()> {
    for (index, &next) in fat.iter().enumerate() {
        if next > LAST_REGULAR {
            field_check(
                next != DIFAT_SECTOR || used[index] == Use::Difat,
                "compound file FAT marks a DIFAT sector the DIFAT does not hold",
            )?;
            continue;
        }
        match used.get_mut(next as usize) {
            Some(state @ Use::Free) => *state = Use::Chain,
            Some(_) => {
                return Err(BadField(
                    "compound file FAT links two sectors to one, or links to a FAT sector",
                ));
            }
            None => return Err(BadField("compound file FAT links past the end of the file")),
        }
    }
    Ok(())
}

fn ensure_every_sector_is_accounted_for(fat: &[u32], used: &[Use]) -> Field<()> {
    for (&next, &state) in fat.iter().zip(used) {
        let expected = match state {
            Use::Free => FREE,
            Use::Chain => END_OF_CHAIN,
            Use::Fat => FAT_SECTOR,
            Use::Difat => DIFAT_SECTOR,
        };
        let found = if next <= LAST_REGULAR {
            END_OF_CHAIN
        } else {
            next
        };
        field_check(
            found == expected,
            "compound file FAT does not match how its sectors are used",
        )?;
    }
    Ok(())
}

fn check_entry(entry: &DirectoryEntry, first: bool, used: &mut [Use]) -> Field<()> {
    if first {
        field_check(
            entry.kind == Kind::Root
                && entry.left == NO_ENTRY
                && entry.right == NO_ENTRY
                && entry.child != 0,
            "compound file does not start with its root entry",
        )?;
        if entry.start != END_OF_CHAIN {
            claim_start(used, entry.start)?;
        }
        return Ok(());
    }
    match entry.kind {
        Kind::Storage => Ok(()),
        // EXTERNAL(7-Zip ComHandler): some compound files hold empty entries with zero links
        // and a start of 0, end of chain or free, and 7-Zip accepts them.
        Kind::Empty => field_check(
            [0, FREE, END_OF_CHAIN].contains(&entry.start)
                && [entry.left, entry.right, entry.child]
                    .iter()
                    .all(|&link| link == NO_ENTRY || link == 0),
            "compound file has an unused entry with links or data",
        ),
        Kind::Root => Err(BadField("compound file has a second root entry")),
        Kind::Stream => {
            field_check(entry.child == NO_ENTRY, "compound file stream has children")?;
            if entry.size == 0 {
                field_check(
                    entry.start == END_OF_CHAIN,
                    "compound file empty stream starts a chain",
                )
            } else if entry.size >= MINI_STREAM_CUTOFF {
                claim_start(used, entry.start)
            } else {
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Links {
    storage: bool,
    empty: bool,
    left: u32,
    right: u32,
    child: u32,
}

impl From<&DirectoryEntry> for Links {
    fn from(entry: &DirectoryEntry) -> Self {
        Links {
            storage: entry.is_storage(),
            empty: entry.kind == Kind::Empty,
            left: entry.left,
            right: entry.right,
            child: entry.child,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ref {
    parent: u32,
    entry: u32,
    level: u32,
}

// EXTERNAL(7-Zip ComHandler): CDatabase::AddNodes lists each tree in order, left subtree,
// the entry, a storage's children, then the right subtree.
fn walk_directory<T>(entries: &[T], links: impl Fn(&T) -> Links) -> Field<Vec<Ref>> {
    const UNSEEN: u8 = 0;
    const LISTED: u8 = 2;
    const DONE: u8 = 3;
    let count = entries.len();
    let Some(root) = entries.first().map(&links) else {
        return Err(BadField("compound file has no root entry"));
    };
    let mut refs = Vec::new();
    let mut index = root.child;
    if index == NO_ENTRY {
        field_check(
            entries.iter().skip(1).all(|entry| links(entry).empty),
            "compound file has a used entry outside its directory tree",
        )?;
        return Ok(refs);
    }
    field_check(
        index != 0 && (index as usize) < count,
        "compound file root links outside its directory",
    )?;
    refs.reserve_exact(count - 1);
    let mut parents = vec![0u32; count];
    let mut states = vec![0u8; count];
    states[0] = DONE;
    let exit = 0u32;
    let mut level = 1u32;
    let mut state = UNSEEN;
    let mut parent = exit;
    let mut ref_parent = NO_PARENT;
    loop {
        if state >= DONE {
            index = parent;
            if index == exit {
                break;
            }
            parent = parents[index as usize];
            state = states[index as usize];
            if state == LISTED {
                ref_parent = refs
                    .get(ref_parent as usize)
                    .map(|reference: &Ref| reference.parent)
                    .ok_or(BadField("compound file directory walk lost its place"))?;
                level -= 1;
            }
            continue;
        }
        let item = links(&entries[index as usize]);
        field_check(!item.empty, "compound file directory links an unused entry")?;
        state += 1;
        states[index as usize] = state;
        let next = if state != LISTED {
            if state < LISTED {
                item.left
            } else {
                item.right
            }
        } else {
            refs.push(Ref {
                parent: ref_parent,
                entry: index,
                level,
            });
            if !item.storage {
                continue;
            }
            if item.child != NO_ENTRY {
                level += 1;
                field_check(
                    level <= MAX_LEVEL,
                    "compound file nests storages more than 256 deep",
                )?;
                ref_parent = (refs.len() - 1) as u32;
            }
            item.child
        };
        if next != NO_ENTRY {
            parents[index as usize] = parent;
            state = UNSEEN;
            parent = index;
            index = next;
            field_check(
                (index as usize) < count && states[index as usize] == UNSEEN,
                "compound file directory links outside itself or loops",
            )?;
        }
    }
    for (item, &state) in entries.iter().zip(&states) {
        let expected = if links(item).empty { UNSEEN } else { DONE };
        field_check(
            state == expected,
            "compound file has a used entry outside its directory tree",
        )?;
    }
    Ok(refs)
}

// EXTERNAL(7-Zip ComHandler): MsiName_To_FileName maps units 0x3800 to 0x483F to one or two
// of 64 characters, 0x4840 to `!`, and units below 0x20 to their number in brackets.
fn decoded_name(units: &[u16]) -> Vec<u16> {
    let mut decoded = Vec::with_capacity(2 * units.len());
    for &unit in units {
        match unit {
            0..0x20 => decoded.extend(format!("[{unit}]").encode_utf16()),
            MSI_FIRST..MSI_TABLE => {
                let packed = unit - MSI_FIRST;
                decoded.push(u16::from(MSI_CHARACTERS[usize::from(packed & 63)]));
                if let Some(&second) = MSI_CHARACTERS.get(usize::from(packed >> 6)) {
                    decoded.push(u16::from(second));
                }
            }
            MSI_TABLE => decoded.push(u16::from(b'!')),
            _ => decoded.push(unit),
        }
    }
    decoded
}

fn has_msi_units(units: &[u16]) -> bool {
    units
        .iter()
        .any(|unit| (MSI_FIRST..=MSI_TABLE).contains(unit))
}

fn ends_with_ignoring_ascii_case(units: &[u16], suffix: &str) -> bool {
    units.len() >= suffix.len()
        && units[units.len() - suffix.len()..]
            .iter()
            .zip(suffix.bytes())
            .all(|(&unit, byte)| {
                u8::try_from(unit).is_ok_and(|unit| unit.eq_ignore_ascii_case(&byte))
            })
}

fn path_name(units: &[u16]) -> Result<String> {
    let name = String::from_utf16(&decoded_name(units))
        .context("compound file entry name holds an unpaired UTF-16 surrogate")?
        .replace('/', "_");
    ensure!(
        name != "." && name != "..",
        "unsafe archive path: compound file entry {name:?}"
    );
    Ok(if name.is_empty() {
        "[]".to_owned()
    } else {
        name
    })
}

fn filetime(ticks: u64) -> Option<SystemTime> {
    if ticks == 0 {
        return None;
    }
    let since_1601 = Duration::new(
        ticks / FILETIME_TICKS_PER_SECOND,
        (ticks % FILETIME_TICKS_PER_SECOND) as u32 * 100,
    );
    let offset = Duration::from_secs(FILETIME_TO_UNIX_SECONDS);
    match since_1601.checked_sub(offset) {
        Some(after) => std::time::UNIX_EPOCH.checked_add(after),
        None => std::time::UNIX_EPOCH.checked_sub(offset - since_1601),
    }
}

/// Returns whether the file starts with the compound file signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(SIGNATURE.len());
    crate::volumes::open(path)?
        .take(SIGNATURE.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == SIGNATURE)
}

/// Returns whether the default engine hands this file to 7-Zip: an
/// installer whose one CAB or `exe` stream is not a CAB, which 7-Zip may
/// open as another format. A file this reader cannot parse stays with it,
/// so the error comes from it.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let Ok(mut compound) = Compound::read(path) else {
        return Ok(false);
    };
    Ok(match compound.main_subfile() {
        Some(main) => !compound.starts_with_cab(main)?,
        None => false,
    })
}

struct Compound {
    file: ArchiveFile,
    length: u64,
    shift: u32,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_sectors: Vec<u32>,
    entries: Vec<DirectoryEntry>,
    refs: Vec<Ref>,
}

struct Tables {
    fat: Vec<u32>,
    used: Vec<Use>,
}

impl Compound {
    fn read(path: &Path) -> Result<Self> {
        let mut file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut bytes = [0u8; HEADER];
        file.read_exact(&mut bytes)
            .context("truncated compound file header")?;
        let header = header(&bytes)?;
        let sector_size = header.sector_size();
        let sectors_in_file = length.saturating_sub(sector_size).div_ceil(sector_size);
        let stored = u64::from(header.fat_entries()).min(sectors_in_file) as usize;
        for (count, what) in [
            (header.fat_sectors, "FAT"),
            (header.difat_sectors, "DIFAT"),
            (header.mini_fat_sectors, "mini FAT"),
        ] {
            ensure!(
                u64::from(count) <= sectors_in_file,
                "truncated compound file: its {what} has {count} sectors and the file holds {sectors_in_file}"
            );
        }
        let mut compound = Compound {
            file,
            length,
            shift: header.shift,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            mini_sectors: Vec::new(),
            entries: Vec::new(),
            refs: Vec::new(),
        };
        let mut sector = vec![0u8; sector_size as usize];
        let Tables { fat, mut used } = compound.read_fat(&header, stored, &mut sector)?;
        compound.fat = fat;
        claim_links(&compound.fat, &mut used)?;
        compound.read_mini_fat(&header, &mut used, &mut sector)?;
        compound.read_directory(&header, &mut used, &mut sector)?;
        compound.read_mini_stream_sectors(&header)?;
        compound.check_lock_sector(&header, &mut used)?;
        ensure_every_sector_is_accounted_for(&compound.fat, &used)?;
        compound.check_streams()?;
        compound.refs = walk_directory(&compound.entries, |entry| Links::from(entry))?;
        compound.ensure_paths_fit_their_budget()?;
        Ok(compound)
    }

    fn ensure_paths_fit_their_budget(&self) -> Result<()> {
        let directory_bytes = (self.entries.len() * ENTRY) as u64;
        let budget =
            (PATH_BYTES_PER_DIRECTORY_BYTE * directory_bytes).clamp(MIN_PATH_BYTES, MAX_PATH_BYTES);
        let mut lengths = Vec::with_capacity(self.refs.len());
        let mut total = 0u64;
        for reference in &self.refs {
            let name = path_name(self.entry(reference).name())?.len() as u64;
            let length = match lengths.get(reference.parent as usize) {
                Some(&parent) => parent + 1 + name,
                None => name,
            };
            lengths.push(length);
            total += length;
            ensure!(
                total <= budget,
                "compound file paths need more than {budget} bytes, twice its directory within 1 to 64 MiB"
            );
        }
        Ok(())
    }

    fn read_sector(&mut self, index: u32, buffer: &mut [u8]) -> Result<()> {
        let offset = (u64::from(index) + 1) << self.shift;
        ensure!(
            offset + buffer.len() as u64 <= self.length,
            "truncated compound file: sector {index} lies past its end"
        );
        self.file.seek(SeekFrom::Start(offset))?;
        self.file
            .read_exact(buffer)
            .with_context(|| format!("truncated compound file: cannot read sector {index}"))
    }

    fn read_fat(&mut self, header: &Header, stored: usize, sector: &mut [u8]) -> Result<Tables> {
        let per_sector = header.entries_per_sector();
        let mut used = vec![Use::Free; stored];
        let mut difat = header.difat.to_vec();
        difat.reserve_exact(header.difat_sectors as usize * (per_sector - 1));
        let mut next = header.difat_start;
        for _ in 0..header.difat_sectors {
            ensure!(
                used.get(next as usize) == Some(&Use::Free),
                "compound file DIFAT chain reaches a sector past its end or in use"
            );
            used[next as usize] = Use::Difat;
            self.read_sector(next, sector)?;
            difat.extend((0..per_sector - 1).map(|index| u32_at(sector, 4 * index)));
            next = u32_at(sector, 4 * (per_sector - 1));
        }
        ensure!(
            next == END_OF_CHAIN || next == FREE,
            "compound file DIFAT chain does not end where its header says"
        );
        let fat_sectors = header.fat_sectors as usize;
        ensure!(
            fat_sectors <= difat.len() && difat[fat_sectors..].iter().all(|&sector| sector == FREE),
            "compound file DIFAT does not list exactly its FAT sectors"
        );
        let lock = lock_sector(header.shift);
        let mut fat = vec![FREE; stored];
        for (index, &fat_sector) in difat[..fat_sectors].iter().enumerate() {
            ensure!(
                used.get(fat_sector as usize) == Some(&Use::Free),
                "compound file FAT sector lies past its end or is used twice"
            );
            used[fat_sector as usize] = Use::Fat;
            self.read_sector(fat_sector, sector)?;
            for slot in 0..per_sector {
                let covered = index * per_sector + slot;
                let next = u32_at(sector, 4 * slot);
                match fat.get_mut(covered) {
                    Some(entry) => *entry = next,
                    None => ensure!(
                        next == FREE || (covered as u64 == u64::from(lock) && next == END_OF_CHAIN),
                        "compound file FAT allocates sector {covered}, past the end of the file"
                    ),
                }
            }
        }
        for &fat_sector in &difat[..fat_sectors] {
            ensure!(
                fat[fat_sector as usize] == FAT_SECTOR,
                "compound file FAT does not mark its own sector {fat_sector}"
            );
        }
        Ok(Tables { fat, used })
    }

    fn read_mini_fat(
        &mut self,
        header: &Header,
        used: &mut [Use],
        sector: &mut [u8],
    ) -> Result<()> {
        let per_sector = header.entries_per_sector();
        let mut next = header.mini_fat_start;
        if header.mini_fat_sectors != 0 {
            claim_start(used, next)?;
        }
        self.mini_fat
            .reserve_exact(header.mini_fat_entries() as usize);
        for _ in 0..header.mini_fat_sectors {
            let following = *self
                .fat
                .get(next as usize)
                .context("compound file mini FAT chain leaves the file")?;
            self.read_sector(next, sector)?;
            self.mini_fat
                .extend((0..per_sector).map(|index| u32_at(sector, 4 * index)));
            next = following;
        }
        ensure!(
            next == END_OF_CHAIN,
            "compound file mini FAT chain does not end where its header says"
        );
        Ok(())
    }

    fn read_directory(
        &mut self,
        header: &Header,
        used: &mut [Use],
        sector: &mut [u8],
    ) -> Result<()> {
        let mut next = header.directory_start;
        claim_start(used, next)?;
        let per_sector = sector.len() / ENTRY;
        let (mut chain, mut at) = (0usize, next);
        while chain <= MAX_ENTRIES / per_sector
            && let Some(&following) = self.fat.get(at as usize)
        {
            chain += 1;
            at = following;
        }
        self.entries
            .reserve_exact((chain * per_sector).min(MAX_ENTRIES));
        let mut sectors_read = 0usize;
        loop {
            let following = *self
                .fat
                .get(next as usize)
                .context("compound file directory chain leaves the file")?;
            ensure!(
                sectors_read < self.fat.len()
                    && (header.directory_sectors == 0
                        || sectors_read < header.directory_sectors as usize),
                "compound file directory has more sectors than its header gives"
            );
            sectors_read += 1;
            self.read_sector(next, sector)?;
            for bytes in sector.chunks_exact(ENTRY) {
                ensure!(
                    self.entries.len() < MAX_ENTRIES,
                    "compound file has more than {MAX_ENTRIES} directory entries"
                );
                let entry = directory_entry(bytes.try_into().unwrap(), self.shift)?;
                check_entry(&entry, self.entries.is_empty(), used)?;
                self.entries.push(entry);
            }
            next = following;
            if next == END_OF_CHAIN {
                return Ok(());
            }
        }
    }

    fn read_mini_stream_sectors(&mut self, header: &Header) -> Result<()> {
        let root = self.entries[0];
        let count = root.size.div_ceil(header.sector_size());
        ensure!(
            count <= self.fat.len() as u64,
            "compound file mini stream is longer than the file"
        );
        let mini_sectors = root.size.div_ceil(MINI_SECTOR);
        ensure!(
            mini_sectors <= u64::from(header.mini_fat_entries()),
            "compound file mini stream is longer than its mini FAT"
        );
        self.mini_fat.truncate(mini_sectors as usize);
        let mut sectors = Vec::with_capacity(count as usize);
        walk_chain(&self.fat, root.start, count, |sector| {
            sectors.push(sector);
            Ok(())
        })?;
        self.mini_sectors = sectors;
        Ok(())
    }

    // EXTERNAL(7-Zip ComHandler): the range lock sector, which holds offset 0x7FFFFF00, must
    // be end of chain or free.
    fn check_lock_sector(&self, header: &Header, used: &mut [Use]) -> Result<()> {
        let lock = lock_sector(header.shift);
        if lock < header.fat_entries()
            && let Some(&next) = self.fat.get(lock as usize)
        {
            ensure!(
                used[lock as usize] == Use::Free,
                "compound file uses its range lock sector"
            );
            match next {
                END_OF_CHAIN => used[lock as usize] = Use::Chain,
                FREE => {}
                _ => bail!("compound file range lock sector is part of a chain"),
            }
        }
        Ok(())
    }

    fn check_streams(&self) -> Result<()> {
        let mut mini_used = vec![false; self.mini_fat.len()];
        for (index, entry) in self.entries.iter().enumerate() {
            match entry.kind {
                Kind::Empty | Kind::Storage => {}
                Kind::Stream if entry.in_mini_stream() => {
                    let mut left = entry.size;
                    walk_chain(
                        &self.mini_fat,
                        entry.start,
                        entry.size.div_ceil(MINI_SECTOR),
                        |mini| {
                            field_check(
                                !std::mem::replace(&mut mini_used[mini as usize], true),
                                "compound file streams share a mini sector",
                            )?;
                            let offset = self.mini_offset(mini).ok_or(BadField(
                                "compound file mini sector lies outside the mini stream",
                            ))?;
                            let bytes = left.min(MINI_SECTOR);
                            left -= bytes;
                            field_check(
                                offset + bytes <= self.length,
                                "truncated compound file: a stream lies past its end",
                            )
                        },
                    )
                    .with_context(|| format!("compound file entry {index}"))?;
                }
                Kind::Stream | Kind::Root => {
                    let sector_size = 1u64 << self.shift;
                    let mut left = entry.size;
                    walk_chain(
                        &self.fat,
                        entry.start,
                        entry.size.div_ceil(sector_size),
                        |sector| {
                            let bytes = left.min(sector_size);
                            left -= bytes;
                            field_check(
                                ((u64::from(sector) + 1) << self.shift) + bytes <= self.length,
                                "truncated compound file: a stream lies past its end",
                            )
                        },
                    )
                    .with_context(|| format!("compound file entry {index}"))?;
                }
            }
        }
        Ok(())
    }

    fn mini_offset(&self, mini: u32) -> Option<u64> {
        let per_sector_shift = self.shift - MINI_SHIFT;
        let sector = *self.mini_sectors.get((mini >> per_sector_shift) as usize)?;
        let within = u64::from(mini & ((1 << per_sector_shift) - 1)) << MINI_SHIFT;
        Some(((u64::from(sector) + 1) << self.shift) + within)
    }

    fn entry(&self, reference: &Ref) -> &DirectoryEntry {
        &self.entries[reference.entry as usize]
    }

    fn path(&self, index: usize) -> Result<String> {
        let mut names = Vec::new();
        let mut at = index as u32;
        while at != NO_PARENT {
            let reference = &self.refs[at as usize];
            names.push(path_name(self.entry(reference).name())?);
            at = reference.parent;
        }
        names.reverse();
        Ok(names.join("/"))
    }

    // EXTERNAL(7-Zip ComHandler): when a stream name is MSI-encoded, the one stream whose name
    // ends in `.cab` or `exe` is the main subfile, which 7-Zip opens in the file's place.
    fn main_subfile(&self) -> Option<usize> {
        let streams = || {
            self.refs
                .iter()
                .enumerate()
                .filter(|(_, reference)| !self.entry(reference).is_storage())
        };
        if !streams().any(|(_, reference)| has_msi_units(self.entry(reference).name())) {
            return None;
        }
        let mut candidates = streams().filter(|(_, reference)| {
            let name = decoded_name(self.entry(reference).name());
            ends_with_ignoring_ascii_case(&name, ".cab")
                || ends_with_ignoring_ascii_case(&name, "exe")
        });
        let (main, _) = candidates.next()?;
        candidates.next().is_none().then_some(main)
    }

    fn stream(&mut self, index: usize) -> StreamReader<'_> {
        let entry = *self.entry(&self.refs[index]);
        let (table, mini_sectors) = if entry.in_mini_stream() {
            (&self.mini_fat[..], Some(&self.mini_sectors[..]))
        } else {
            (&self.fat[..], None)
        };
        StreamReader {
            file: &mut self.file,
            table,
            mini_sectors,
            shift: self.shift,
            sector: entry.start,
            within: 0,
            remaining: entry.size,
        }
    }

    fn starts_with_cab(&mut self, index: usize) -> Result<bool> {
        let mut magic = Vec::with_capacity(CAB_SIGNATURE.len());
        self.stream(index)
            .take(CAB_SIGNATURE.len() as u64)
            .read_to_end(&mut magic)?;
        Ok(magic == CAB_SIGNATURE)
    }

    fn extract(&mut self, tree: &mut tree::Tree) -> Result<()> {
        for index in 0..self.refs.len() {
            tree.check()?;
            let name = self.path(index)?;
            let entry = *self.entry(&self.refs[index]);
            let modified = filetime(entry.modified);
            match entry.kind {
                Kind::Storage => tree.directory(&name, DIRECTORY_MODE, modified)?,
                Kind::Stream => {
                    let mut data = self.stream(index);
                    tree.file(&name, entry.size, FILE_MODE, modified, &mut data)?;
                }
                Kind::Empty | Kind::Root => bail!("compound file lists entry {index} twice"),
            }
        }
        Ok(())
    }
}

fn lock_sector(shift: u32) -> u32 {
    (0x7FFF_FFFF >> shift) - 1
}

struct StreamReader<'a> {
    file: &'a mut ArchiveFile,
    table: &'a [u32],
    mini_sectors: Option<&'a [u32]>,
    shift: u32,
    sector: u32,
    within: u64,
    remaining: u64,
}

impl StreamReader<'_> {
    fn unit(&self) -> u64 {
        match self.mini_sectors {
            Some(_) => MINI_SECTOR,
            None => 1 << self.shift,
        }
    }

    fn offset(&self, sector: u32) -> Option<u64> {
        match self.mini_sectors {
            None => Some((u64::from(sector) + 1) << self.shift),
            Some(mini_sectors) => {
                let per_sector_shift = self.shift - MINI_SHIFT;
                let big = *mini_sectors.get((sector >> per_sector_shift) as usize)?;
                let within = u64::from(sector & ((1 << per_sector_shift) - 1)) << MINI_SHIFT;
                Some(((u64::from(big) + 1) << self.shift) + within)
            }
        }
    }

    fn next(&self, sector: u32) -> std::io::Result<u32> {
        self.table.get(sector as usize).copied().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compound file chain leaves its allocation table",
            )
        })
    }
}

impl Read for StreamReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compound file sector lies outside the file",
            )
        };
        if self.remaining == 0 || buffer.is_empty() {
            return Ok(0);
        }
        let unit = self.unit();
        if self.within == unit {
            self.sector = self.next(self.sector)?;
            self.within = 0;
        }
        let start = self.offset(self.sector).ok_or_else(invalid)? + self.within;
        let want = self.remaining.min(buffer.len() as u64);
        let mut available = unit - self.within;
        let mut last = self.sector;
        while available < want {
            let next = self.next(last)?;
            let contiguous = next <= LAST_REGULAR
                && self.offset(next) == Some(self.offset(last).ok_or_else(invalid)? + unit);
            if !contiguous {
                break;
            }
            last = next;
            available += unit;
        }
        let count = want.min(available);
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut buffer[..count as usize])?;
        let consumed = self.within + count;
        let whole = (consumed - 1) / unit;
        for _ in 0..whole {
            self.sector = self.next(self.sector)?;
        }
        self.within = consumed - whole * unit;
        self.remaining -= count;
        Ok(count as usize)
    }
}

fn cancelled(options: &ReadOptions) -> bool {
    options
        .control
        .cancel
        .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed))
}

fn read_embedded_cab(
    compound: &mut Compound,
    main: usize,
    archive: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    if let Some(destination) = destination
        && options.existing == crate::Existing::Refuse
        && std::fs::symlink_metadata(destination).is_ok()
    {
        bail!(crate::DestinationRefused(
            "extraction destination must not exist".to_owned()
        ));
    }
    let name = compound.path(main)?;
    let size = compound.entry(&compound.refs[main]).size;
    let folder = match destination.and_then(Path::parent) {
        Some(parent) if parent.as_os_str().is_empty() => Path::new(".").to_owned(),
        Some(parent) => parent.to_owned(),
        None => std::env::temp_dir(),
    };
    let free = crate::available_bytes(&folder)
        .with_context(|| format!("cannot measure free space at {}", folder.display()))?;
    ensure!(
        size <= free,
        "the CAB stream {name:?} needs {size} bytes in {} for its copy, and {free} are free",
        folder.display()
    );
    let mut copy = tempfile::Builder::new()
        .prefix(".zipper-msi-")
        .tempfile_in(&folder)
        .with_context(|| {
            format!(
                "cannot create a copy of the CAB stream in {}",
                folder.display()
            )
        })?;
    let mut stream = compound.stream(main);
    let mut buffer = vec![0u8; COPY_BUFFER.min(size as usize)];
    loop {
        ensure!(!cancelled(&options), crate::Cancelled);
        let read = stream
            .read(&mut buffer)
            .context("cannot read the CAB stream of the compound file")?;
        if read == 0 {
            break;
        }
        copy.write_all(&buffer[..read])
            .context("cannot write the copy of the CAB stream")?;
    }
    copy.flush()?;
    #[cfg(windows)]
    if let Some(zone) = crate::ntfs_streams::archive_mark(archive)
        && options.metadata.restores()
    {
        crate::ntfs_streams::write_stream(copy.path(), "Zone.Identifier", &zone)?;
    }
    #[cfg(not(windows))]
    let _ = archive;
    crate::native_cab::read_archive(copy.path(), destination, options)
        .with_context(|| format!("in the CAB stream {name:?} of the compound file"))
}

/// Verify and optionally extract a compound file into a new directory, as
/// `7z x` does. An installer whose only `.cab` or `exe` stream is a CAB
/// extracts that CAB's files. Every other compound file extracts its
/// streams as files and its storages as folders, under the names 7-Zip
/// gives them. It refuses an installer whose one such stream is not a CAB,
/// which the default engine hands to 7-Zip.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut compound = Compound::read(path)?;
    if let Some(main) = compound.main_subfile() {
        if !compound.starts_with_cab(main)? {
            bail!(
                "compound file {:?} is an installer whose main stream is not a CAB file, which 7-Zip may open as another format; {BACKEND_HINT}",
                compound.path(main)?
            );
        }
        return read_embedded_cab(&mut compound, main, path, destination, options);
    }
    tree::extract("compound file", path, destination, options, |tree| {
        compound.extract(tree)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    const LINKS: [u32; 7] = [0, 1, 2, 3, END_OF_CHAIN, FREE, FAT_SECTOR];

    fn tables(length: usize, values: &[u32]) -> Vec<Vec<u32>> {
        (0..values.len().pow(length as u32))
            .map(|mut code| {
                (0..length)
                    .map(|_| {
                        let value = values[code % values.len()];
                        code /= values.len();
                        value
                    })
                    .collect()
            })
            .collect()
    }

    fn reference_chain(table: &[u32], start: u32) -> Option<Vec<u32>> {
        let mut seen = Vec::new();
        let mut sector = start;
        while sector != END_OF_CHAIN {
            let index = sector as usize;
            if index >= table.len() || seen.contains(&sector) {
                return None;
            }
            seen.push(sector);
            sector = table[index];
        }
        Some(seen)
    }

    #[test]
    fn every_short_chain_is_accepted_exactly_when_it_ends_without_a_cycle() {
        let mut cases = 0;
        for length in 0..=4 {
            for table in tables(length, &LINKS) {
                for start in LINKS {
                    for count in 0..=5 {
                        let mut visited = Vec::new();
                        let walked = walk_chain(&table, start, count, |sector| {
                            visited.push(sector);
                            Ok(())
                        });
                        let expected = reference_chain(&table, start)
                            .filter(|chain| chain.len() as u64 == count);
                        assert_eq!(
                            walked.is_ok(),
                            expected.is_some(),
                            "{table:?} from {start} for {count}"
                        );
                        if let Some(expected) = expected {
                            assert_eq!(visited, expected);
                        }
                        cases += 1;
                    }
                }
            }
        }
        assert!(cases > 100_000);
    }

    #[test]
    fn every_small_fat_the_allocation_checks_accept_has_chains_that_end_apart() {
        let values = [0, 1, 2, 3, END_OF_CHAIN, FREE, FAT_SECTOR, DIFAT_SECTOR];
        let mut accepted = 0;
        for length in 1..=4 {
            for fat in tables(length, &values) {
                for starts in 0u32..1 << length {
                    let mut used: Vec<Use> = fat
                        .iter()
                        .map(|&next| match next {
                            FAT_SECTOR => Use::Fat,
                            DIFAT_SECTOR => Use::Difat,
                            _ => Use::Free,
                        })
                        .collect();
                    let starts: Vec<u32> = (0..length as u32)
                        .filter(|bit| starts & (1 << bit) != 0)
                        .collect();
                    let passed = claim_links(&fat, &mut used).is_ok()
                        && starts
                            .iter()
                            .all(|&start| claim_start(&mut used, start).is_ok())
                        && ensure_every_sector_is_accounted_for(&fat, &used).is_ok();
                    if !passed {
                        continue;
                    }
                    accepted += 1;
                    let mut owners = vec![0; length];
                    for &start in &starts {
                        let chain = reference_chain(&fat, start)
                            .unwrap_or_else(|| panic!("{fat:?} from {start} never ends"));
                        for sector in chain {
                            owners[sector as usize] += 1;
                        }
                    }
                    assert!(
                        owners.iter().all(|&count| count <= 1),
                        "{fat:?} shares a sector"
                    );
                }
            }
        }
        assert!(accepted > 1000);
    }

    fn reference_walk(entries: &[Links]) -> Option<Vec<Ref>> {
        fn visit(
            entries: &[Links],
            index: u32,
            parent: u32,
            level: u32,
            seen: &mut [bool],
            refs: &mut Vec<Ref>,
        ) -> Option<()> {
            if index == NO_ENTRY {
                return Some(());
            }
            let item = *entries.get(index as usize)?;
            if seen[index as usize] || item.empty {
                return None;
            }
            seen[index as usize] = true;
            visit(entries, item.left, parent, level, seen, refs)?;
            refs.push(Ref {
                parent,
                entry: index,
                level,
            });
            if item.storage && item.child != NO_ENTRY {
                if level + 1 > MAX_LEVEL {
                    return None;
                }
                let me = refs.len() as u32 - 1;
                visit(entries, item.child, me, level + 1, seen, refs)?;
            }
            visit(entries, item.right, parent, level, seen, refs)
        }
        let root = entries.first()?;
        if root.child == 0 {
            return None;
        }
        let mut seen = vec![false; entries.len()];
        seen[0] = true;
        let mut refs = Vec::new();
        visit(entries, root.child, NO_PARENT, 1, &mut seen, &mut refs)?;
        entries
            .iter()
            .zip(&seen)
            .all(|(item, &seen)| item.empty != seen)
            .then_some(refs)
    }

    fn links(kind: usize, left: u32, right: u32, child: u32) -> Links {
        Links {
            storage: kind == 1,
            empty: kind == 2,
            left,
            right,
            child,
        }
    }

    #[test]
    fn every_small_directory_walks_as_a_recursive_reference_does() {
        let mut cases = 0;
        for count in 1..=3u32 {
            let options: Vec<u32> = (0..count).chain([NO_ENTRY]).collect();
            let others = (count - 1) as usize;
            let per_entry = 3 * options.len().pow(3);
            for &root_child in &options {
                for code in 0..per_entry.pow(others as u32) {
                    let mut code = code;
                    let mut pick = |limit: usize| {
                        let value = code % limit;
                        code /= limit;
                        value
                    };
                    let mut entries = vec![links(1, NO_ENTRY, NO_ENTRY, root_child)];
                    for _ in 0..others {
                        let kind = pick(3);
                        let left = options[pick(options.len())];
                        let right = options[pick(options.len())];
                        let child = options[pick(options.len())];
                        entries.push(links(kind, left, right, child));
                    }
                    assert_eq!(
                        walk_directory(&entries, |links| *links).ok(),
                        reference_walk(&entries),
                        "{entries:?}"
                    );
                    cases += 1;
                }
            }
        }
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut random = |limit: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % limit
        };
        for _ in 0..300_000 {
            let count = 4 + random(4) as u32;
            let mut link = || {
                let value = random(u64::from(count) + 2) as u32;
                if value >= count { NO_ENTRY } else { value }
            };
            let mut entries = vec![links(1, NO_ENTRY, NO_ENTRY, link())];
            for _ in 1..count {
                let kind = (link() % 3) as usize;
                entries.push(links(kind, link(), link(), link()));
            }
            assert_eq!(
                walk_directory(&entries, |links| *links).ok(),
                reference_walk(&entries),
                "{entries:?}"
            );
            cases += 1;
        }
        assert!(cases > 300_000);
    }

    #[test]
    fn storages_nest_256_deep_and_no_deeper() {
        for depth in [255u32, 256, 257, 300] {
            let mut entries = vec![links(1, NO_ENTRY, NO_ENTRY, 1)];
            for level in 1..=depth {
                let child = if level == depth { NO_ENTRY } else { level + 1 };
                entries.push(links(1, NO_ENTRY, NO_ENTRY, child));
            }
            let walked = walk_directory(&entries, |links| *links);
            assert_eq!(walked.is_ok(), depth <= MAX_LEVEL, "{depth}");
            assert_eq!(walked.ok(), reference_walk(&entries), "{depth}");
        }
    }

    fn reference_decoded(unit: u16) -> String {
        let table = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._";
        let c = u32::from(unit);
        if c > 0x4840 {
            return String::from_utf16_lossy(&[unit]);
        }
        if c < 0x3800 {
            return if c < 0x20 {
                format!("[{c}]")
            } else {
                String::from_utf16_lossy(&[unit])
            };
        }
        let c = c - 0x3800;
        let c1 = c >> 6;
        if c1 > 64 {
            return "!".to_owned();
        }
        let mut out = String::new();
        out.push(char::from(table[(c & 63) as usize]));
        if c1 < 64 {
            out.push(char::from(table[c1 as usize]));
        }
        out
    }

    #[test]
    fn every_name_unit_decodes_as_7zip_decodes_it() {
        for unit in 1..=u16::MAX {
            if (0xD800..0xE000).contains(&unit) {
                continue;
            }
            assert_eq!(
                String::from_utf16(&decoded_name(&[unit])).unwrap(),
                reference_decoded(unit),
                "{unit:#x}"
            );
        }
        let summary: Vec<u16> = [5]
            .into_iter()
            .chain("SummaryInformation".encode_utf16())
            .collect();
        assert_eq!(path_name(&summary).unwrap(), "[5]SummaryInformation");
        assert_eq!(path_name(&[]).unwrap(), "[]");
        let slashed: Vec<u16> = "a/b".encode_utf16().collect();
        assert_eq!(path_name(&slashed).unwrap(), "a_b");
        assert!(path_name(&[0xD800]).is_err());
        assert_eq!(path_name(&[0xD83D, 0xDE00]).unwrap(), "\u{1F600}");
    }

    #[test]
    fn filetimes_convert_at_their_known_points() {
        assert_eq!(filetime(0), None);
        assert_eq!(
            filetime(116_444_736_000_000_000),
            Some(std::time::UNIX_EPOCH)
        );
        assert_eq!(
            filetime(116_444_736_000_000_015),
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(1500))
        );
        assert_eq!(
            filetime(116_444_735_990_000_000),
            Some(std::time::UNIX_EPOCH - Duration::from_secs(1))
        );
        let _ = filetime(1);
        let _ = filetime(u64::MAX);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(110)]
    fn headers_never_panic_and_give_tables_that_fit() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(header) = header(&bytes) {
            assert!(header.shift == 9 || header.shift == 12);
            let per_sector = header.entries_per_sector() as u64;
            assert!(per_sector == 128 || per_sector == 1024);
            assert_eq!(
                u64::from(header.fat_entries()),
                u64::from(header.fat_sectors) * per_sector
            );
            assert!(header.fat_entries() != 0);
            assert_eq!(
                u64::from(header.mini_fat_entries()),
                u64::from(header.mini_fat_sectors) * per_sector
            );
            assert!(header.shift == 12 || header.directory_sectors == 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn directory_entries_never_panic_and_keep_names_and_sizes_in_range() {
        let bytes: [u8; ENTRY] = kani::any();
        let shift = if kani::any() { 9 } else { 12 };
        if let Ok(entry) = directory_entry(&bytes, shift) {
            let units = usize::from(entry.name_units);
            assert!(units <= NAME_UNITS);
            assert!(entry.kind == Kind::Empty || units < NAME_UNITS);
            assert!(entry.name().iter().all(|&unit| unit != 0));
            assert!(entry.name[units..].iter().all(|&unit| unit == 0));
            assert!(entry.size >> 32 < 1 << shift);
            assert!(shift == 12 || entry.size <= u64::from(u32::MAX));
        }
    }

    #[kani::proof]
    #[kani::unwind(7)]
    fn chains_never_panic_and_visit_distinct_sectors_inside_their_table() {
        let full: [u32; 4] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= full.len());
        let table = &full[..length];
        let start: u32 = kani::any();
        let count: u64 = kani::any();
        kani::assume(count <= 5);
        let mut visited = [0u32; 5];
        let mut visits = 0;
        let walked = walk_chain(table, start, count, |sector| {
            visited[visits] = sector;
            visits += 1;
            Ok(())
        });
        if walked.is_ok() {
            assert_eq!(visits as u64, count);
            for first in 0..visits {
                assert!((visited[first] as usize) < length);
                for second in first + 1..visits {
                    assert_ne!(visited[first], visited[second]);
                }
            }
        }
    }

    #[kani::proof]
    fn entry_checks_never_panic_and_claim_only_the_free_start() {
        let mut used = [Use::Free; 3];
        for state in &mut used {
            *state = match kani::any::<u8>() % 4 {
                0 => Use::Free,
                1 => Use::Chain,
                2 => Use::Fat,
                _ => Use::Difat,
            };
        }
        let before = used;
        let entry = DirectoryEntry {
            name: [0; NAME_UNITS],
            name_units: 0,
            kind: match kani::any::<u8>() % 4 {
                0 => Kind::Empty,
                1 => Kind::Storage,
                2 => Kind::Stream,
                _ => Kind::Root,
            },
            left: kani::any(),
            right: kani::any(),
            child: kani::any(),
            modified: 0,
            start: kani::any(),
            size: kani::any(),
        };
        let first: bool = kani::any();
        if check_entry(&entry, first, &mut used).is_ok() {
            for (index, (&after, &was)) in used.iter().zip(&before).enumerate() {
                if after != was {
                    assert!(was == Use::Free && after == Use::Chain);
                    assert_eq!(index as u32, entry.start);
                }
            }
        }
    }
}
