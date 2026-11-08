use crate::{
    BadField, Field,
    disk::{Claims, Disk, Layer, Opened, Pages, read_at, read_file_exact},
    field_check,
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, bail, ensure};
use std::{io, path::Path};

pub(crate) const MAGIC: &[u8; 4] = b"KDMV";
pub(crate) const DESCRIPTOR_START: &[u8] = b"# Disk DescriptorFile";
pub(crate) const HEADER: usize = 512;
const SECTOR: u64 = 512;
const SECTOR_LOG: u32 = 9;
const GD_AT_END: u64 = u64::MAX;
const LARGEST_VERSION: u32 = 3;
const STREAM_VERSION: u32 = 3;
const VALID_NEWLINE_TEST: u32 = 1;
const ZEROED_GRAIN_ENTRIES: u32 = 1 << 2;
const COMPRESSED: u32 = 1 << 16;
const MARKERS: u32 = 1 << 17;
const NEWLINE_TEST: &[u8; 4] = b"\n \r\n";
const NO_COMPRESSION: u16 = 0;
const DEFLATE: u16 = 1;
const LARGEST_GRAIN_LOG: u32 = 16;
const TABLE_LOG: u32 = 9;
const LARGEST_CAPACITY_LOG: u32 = 37;
const DESCRIPTOR_LIMIT: u64 = 1 << 20;
const EXTENT_LIMIT: usize = 4096;
const ENTRY: u64 = 4;
const DIRECTORY_BATCH: usize = 1 << 16;
const GRAIN_MARKER: usize = 12;
const MARKER: usize = 16;
const MARKER_END_OF_STREAM: u32 = 0;
const MARKER_GRAIN_TABLE: u32 = 1;
const MARKER_GRAIN_DIRECTORY: u32 = 2;
const MARKER_FOOTER: u32 = 3;
const ZERO_GRAIN: u32 = 1;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) version: u32,
    pub(crate) flags: u32,
    pub(crate) capacity: u64,
    pub(crate) grain_log: u32,
    pub(crate) descriptor_at: u64,
    pub(crate) descriptor_sectors: u64,
    pub(crate) gd: u64,
    pub(crate) overhead: u64,
    pub(crate) compression: u16,
}

impl Header {
    pub(crate) fn stream(&self) -> bool {
        self.flags & (COMPRESSED | MARKERS) != 0
    }

    pub(crate) fn grains(&self) -> u64 {
        let mask = (1 << self.grain_log) - 1;
        (self.capacity >> self.grain_log) + u64::from(self.capacity & mask != 0)
    }

    pub(crate) fn tables(&self) -> u64 {
        let grains = self.grains();
        let mask = (1 << TABLE_LOG) - 1;
        (grains >> TABLE_LOG) + u64::from(grains & mask != 0)
    }

    fn same_layout(&self, other: &Header) -> bool {
        self.version == other.version
            && self.flags == other.flags
            && self.capacity == other.capacity
            && self.grain_log == other.grain_log
            && self.overhead == other.overhead
            && self.compression == other.compression
    }
}

pub(crate) fn header(p: &[u8; HEADER]) -> Field<Header> {
    field_check(&p[..4] == MAGIC, "not a VMDK sparse extent")?;
    let header = Header {
        version: le32(p, 4),
        flags: le32(p, 8),
        capacity: le64(p, 0x0C),
        grain_log: (0..=LARGEST_GRAIN_LOG)
            .find(|log| 1u64 << log == le64(p, 0x14))
            .ok_or(BadField(
                "the VMDK grain is not a power of two of 1 to 65,536 sectors",
            ))?,
        descriptor_at: le64(p, 0x1C),
        descriptor_sectors: le64(p, 0x24),
        gd: le64(p, 0x38),
        overhead: le64(p, 0x40),
        compression: le16(p, 0x4D),
    };
    field_check(
        header.version <= LARGEST_VERSION,
        "the VMDK sparse extent has an unknown version",
    )?;
    field_check(
        header.flags & VALID_NEWLINE_TEST == 0 || &p[0x49..0x4D] == NEWLINE_TEST,
        "the VMDK header's newline test fails, so it was damaged in transfer",
    )?;
    field_check(
        le32(p, 0x2C) == 1 << TABLE_LOG,
        "the VMDK grain tables do not hold 512 entries, which 7-Zip does not read either",
    )?;
    if header.stream() {
        field_check(
            header.flags & (COMPRESSED | MARKERS) == COMPRESSED | MARKERS
                && header.version == STREAM_VERSION
                && header.compression == DEFLATE,
            "the VMDK sparse extent compresses its grains in a way 7-Zip does not read either",
        )?;
    } else {
        field_check(
            header.compression == NO_COMPRESSION,
            "the VMDK sparse extent names a compression but does not compress",
        )?;
    }
    field_check(
        header.capacity >> LARGEST_CAPACITY_LOG == 0,
        "the VMDK disk is larger than 64 TiB",
    )?;
    Ok(header)
}

pub(crate) fn marker(p: &[u8; MARKER]) -> Option<(u64, u32)> {
    (le32(p, 8) == 0).then(|| (le64(p, 0), le32(p, 12)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grain {
    Zeros,
    At(u64),
}

pub(crate) fn grain_entry(entry: u32, header: &Header) -> Grain {
    match entry {
        0 => Grain::Zeros,
        ZERO_GRAIN if header.flags & ZEROED_GRAIN_ENTRIES != 0 => Grain::Zeros,
        sector => Grain::At(u64::from(sector) << SECTOR_LOG),
    }
}

pub(crate) struct Sparse {
    header: Header,
    length: u64,
    pages: Pages,
    inflated: Option<(u64, Vec<u8>)>,
}

impl Sparse {
    fn forget(&mut self) {
        self.pages = Pages::default();
        self.inflated = None;
    }

    fn grain_bytes(&self) -> u64 {
        1 << (self.header.grain_log + SECTOR_LOG)
    }

    fn entry(&mut self, file: &ArchiveFile, at: u64) -> io::Result<u32> {
        let mut bytes = [0u8; ENTRY as usize];
        self.pages.read(file, self.length, at, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn grain(&mut self, file: &ArchiveFile, index: u64) -> io::Result<Grain> {
        let table = index >> TABLE_LOG;
        let table_at = u64::from(self.entry(file, (self.header.gd << SECTOR_LOG) + ENTRY * table)?);
        if table_at == 0 {
            return Ok(Grain::Zeros);
        }
        let slot = index & ((1 << TABLE_LOG) - 1);
        let value = self.entry(file, (table_at << SECTOR_LOG) + ENTRY * slot)?;
        Ok(grain_entry(value, &self.header))
    }
}

fn inflate_grain(file: &ArchiveFile, length: u64, at: u64, grain: u64) -> io::Result<Vec<u8>> {
    let mut head = [0u8; GRAIN_MARKER];
    read_file_exact(file, &mut head, at)?;
    let packed = u64::from(le32(&head, 8));
    let start = at + GRAIN_MARKER as u64;
    if start.checked_add(packed).is_none_or(|end| end > length) {
        return Err(io::Error::other(
            "a compressed VMDK grain lies past the end of the image",
        ));
    }
    let mut input = vec![0u8; packed as usize];
    read_file_exact(file, &mut input, start)?;
    let mut output = vec![0u8; grain as usize];
    let mut inflater = flate2::Decompress::new(true);
    let status = inflater
        .decompress(&input, &mut output, flate2::FlushDecompress::Finish)
        .map_err(|_| io::Error::other("a compressed VMDK grain is damaged"))?;
    if status != flate2::Status::StreamEnd || inflater.total_in() != packed {
        return Err(io::Error::other(
            "a compressed VMDK grain's stream does not end where its size says",
        ));
    }
    if inflater.total_out() != grain {
        return Err(io::Error::other(
            "a compressed VMDK grain does not fill its grain",
        ));
    }
    Ok(output)
}

impl Layer for Sparse {
    fn read(&mut self, file: &ArchiveFile, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let grain = self.grain_bytes();
        let index = offset / grain;
        let within = offset % grain;
        let want = (buffer.len() as u64).min(grain - within) as usize;
        match self.grain(file, index)? {
            Grain::Zeros => buffer[..want].fill(0),
            Grain::At(at) if self.header.stream() => {
                if self
                    .inflated
                    .as_ref()
                    .is_none_or(|(held, _)| *held != index)
                {
                    let bytes = inflate_grain(file, self.length, at, grain)?;
                    self.inflated = Some((index, bytes));
                }
                let bytes = &self.inflated.as_ref().unwrap().1;
                buffer[..want].copy_from_slice(&bytes[within as usize..within as usize + want]);
            }
            Grain::At(at) => read_file_exact(file, &mut buffer[..want], at + within)?,
        }
        Ok(want)
    }
}

struct Places {
    grain_log: u32,
    sectors: Claims,
    sector_units: Claims,
    grains: Claims,
}

impl Places {
    fn sectors(&mut self, sector: u64, count: u64, what: &str) -> Result<()> {
        for unit in sector..sector + count {
            ensure!(
                !self.grains.holds(unit >> self.grain_log) && self.sectors.claim(unit),
                "two VMDK tables or grains share their bytes in the file, at the {what}"
            );
            self.sector_units.claim(unit >> self.grain_log);
        }
        Ok(())
    }

    fn grain(&mut self, sector: u64) -> Result<()> {
        let first = sector >> self.grain_log;
        let last = (sector + (1 << self.grain_log) - 1) >> self.grain_log;
        for unit in first..=last {
            let clear = !self.sector_units.holds(unit)
                || (sector..sector + (1 << self.grain_log))
                    .all(|inside| !self.sectors.holds(inside));
            ensure!(
                clear && self.grains.claim(unit),
                "two VMDK tables or grains share their bytes in the file, at a grain"
            );
        }
        Ok(())
    }
}

fn read_marker(file: &ArchiveFile, length: u64, at: u64) -> Result<Option<(u64, u32)>> {
    let bytes = read_at(file, length, at, MARKER, "VMDK marker")?;
    Ok(marker(bytes[..].try_into().unwrap()))
}

fn check_grain(
    file: &ArchiveFile,
    length: u64,
    header: &Header,
    places: &mut Places,
    grain: u64,
    at: u64,
) -> Result<()> {
    let grain_bytes = 1u64 << (header.grain_log + SECTOR_LOG);
    ensure!(
        at >> SECTOR_LOG >= header.overhead,
        "a VMDK grain lies inside the extent's metadata, which 7-Zip does not read either"
    );
    if header.stream() {
        ensure!(
            at + GRAIN_MARKER as u64 <= length,
            "a compressed VMDK grain lies past the end of the image"
        );
        let head = read_at(file, length, at, GRAIN_MARKER, "VMDK grain marker")?;
        ensure!(
            le64(&head, 0) == grain << header.grain_log,
            "a compressed VMDK grain names another place in the disk"
        );
        let packed = u64::from(le32(&head, 8));
        ensure!(
            packed != 0 && packed <= 2 * grain_bytes + SECTOR,
            "a compressed VMDK grain has a malformed size"
        );
        ensure!(
            at + (GRAIN_MARKER as u64 + packed) <= length,
            "a compressed VMDK grain lies past the end of the image"
        );
        let sectors = (GRAIN_MARKER as u64 + packed).div_ceil(SECTOR);
        places.sectors(at >> SECTOR_LOG, sectors, "compressed grain")
    } else {
        let size = header.capacity << SECTOR_LOG;
        let serves = if grain < header.grains() {
            grain_bytes.min(size - grain * grain_bytes)
        } else {
            grain_bytes
        };
        ensure!(
            at + serves <= length,
            "a VMDK grain lies past the end of the image"
        );
        places.grain(at >> SECTOR_LOG)
    }
}

fn check_tables(file: &ArchiveFile, length: u64, header: &Header) -> Result<()> {
    let per_table = 1u64 << TABLE_LOG;
    let table_sectors = (per_table * ENTRY).div_ceil(SECTOR);
    let tables = header.tables();
    let mut places = Places {
        grain_log: header.grain_log,
        sectors: Claims::default(),
        sector_units: Claims::default(),
        grains: Claims::default(),
    };
    places.sectors(0, 1, "header")?;
    if header.descriptor_sectors != 0 {
        places.sectors(
            header.descriptor_at,
            header.descriptor_sectors,
            "descriptor",
        )?;
    }
    let gd_sectors = (tables * ENTRY).div_ceil(SECTOR);
    ensure!(
        header.gd != 0
            && header
                .gd
                .checked_add(gd_sectors)
                .is_some_and(|end| end <= length >> SECTOR_LOG),
        "the VMDK grain directory lies outside the image"
    );
    if header.stream() {
        ensure!(
            read_marker(file, length, (header.gd - 1) << SECTOR_LOG)?
                == Some((gd_sectors, MARKER_GRAIN_DIRECTORY)),
            "the VMDK grain directory lacks its marker, so 7-Zip does not read the image either"
        );
    }
    places.sectors(header.gd, gd_sectors, "grain directory")?;
    let mut directory = vec![0u8; DIRECTORY_BATCH];
    let mut table = vec![0u8; (per_table * ENTRY) as usize];
    for index in 0..tables {
        let place = (index * ENTRY) as usize % DIRECTORY_BATCH;
        if place == 0 {
            let bytes = ((tables - index) * ENTRY).min(DIRECTORY_BATCH as u64) as usize;
            read_file_exact(
                file,
                &mut directory[..bytes],
                (header.gd << SECTOR_LOG) + index * ENTRY,
            )
            .context("cannot read the VMDK grain directory")?;
        }
        let table_at = u64::from(le32(&directory, place));
        if table_at == 0 {
            continue;
        }
        ensure!(
            (table_at + table_sectors) << SECTOR_LOG <= length,
            "a VMDK grain table lies past the end of the image"
        );
        if header.stream() {
            ensure!(
                read_marker(file, length, (table_at - 1) << SECTOR_LOG)?
                    == Some((table_sectors, MARKER_GRAIN_TABLE)),
                "a VMDK grain table lacks its marker, so 7-Zip does not read the image either"
            );
        }
        places.sectors(table_at, table_sectors, "grain table")?;
        read_file_exact(file, &mut table, table_at << SECTOR_LOG)
            .context("cannot read a VMDK grain table")?;
        for (slot, entry) in table.chunks_exact(ENTRY as usize).enumerate() {
            if let Grain::At(at) = grain_entry(le32(entry, 0), header) {
                check_grain(
                    file,
                    length,
                    header,
                    &mut places,
                    index * per_table + slot as u64,
                    at,
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExtentLine<'t> {
    pub(crate) sectors: u64,
    pub(crate) kind: &'t str,
    pub(crate) file: &'t str,
    pub(crate) offset: u64,
}

fn word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    text.split_once(char::is_whitespace).unwrap_or((text, ""))
}

fn is_blank(line: &str) -> bool {
    line.is_empty()
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#')
}

fn is_setting(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(key, _)| !key.contains('"'))
}

// EXTERNAL(7-Zip): 7zz refuses a descriptor line that is not blank, a comment, a setting or an extent.
pub(crate) fn descriptor_line(
    line: &str,
) -> std::result::Result<Option<ExtentLine<'_>>, &'static str> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if is_blank(line) || is_comment(line) || is_setting(line) {
        return Ok(None);
    }
    let (_, rest) = word(line);
    let (sectors, rest) = word(rest);
    let (kind, rest) = word(rest);
    let Some(sectors) = sectors
        .bytes()
        .all(|digit| digit.is_ascii_digit())
        .then(|| sectors.parse::<u64>().ok())
        .flatten()
    else {
        return Err("a VMDK descriptor line is not a comment, setting or extent");
    };
    let Some((file, rest)) = rest
        .trim_start()
        .strip_prefix('"')
        .and_then(|quoted| quoted.split_once('"'))
    else {
        return Err("a VMDK extent line does not name its file in quotes");
    };
    let (offset, _) = word(rest);
    let offset = if offset.is_empty() {
        0
    } else {
        offset
            .parse::<u64>()
            .map_err(|_| "a VMDK extent line has a malformed offset")?
    };
    Ok(Some(ExtentLine {
        sectors,
        kind,
        file,
        offset,
    }))
}

fn descriptor_text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
}

fn extents_of(text: &str) -> Result<Vec<ExtentLine<'_>>> {
    let mut extents = Vec::new();
    for line in text.split('\n') {
        if let Some(extent) = descriptor_line(line).map_err(|reason| anyhow::anyhow!(reason))? {
            ensure!(
                extents.len() < EXTENT_LIMIT,
                "the VMDK descriptor names more than 4,096 extents"
            );
            extents.push(extent);
        }
    }
    Ok(extents)
}

fn parent_named(text: &str) -> bool {
    text.split('\n').any(|line| {
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("parentCID")
                && !value
                    .trim()
                    .trim_matches('"')
                    .eq_ignore_ascii_case("ffffffff")
        })
    })
}

pub(crate) fn open_sparse_extent(
    file: &ArchiveFile,
    length: u64,
) -> Result<Result<Sparse, String>> {
    let bytes = read_at(file, length, 0, HEADER, "VMDK header")?;
    let mut header = header(bytes[..].try_into().unwrap()).map_err(anyhow::Error::from)?;
    if header.descriptor_sectors != 0 {
        ensure!(
            header.descriptor_at != 0
                && header.descriptor_sectors <= DESCRIPTOR_LIMIT >> SECTOR_LOG
                && header
                    .descriptor_at
                    .checked_add(header.descriptor_sectors)
                    .is_some_and(|end| end <= length >> SECTOR_LOG),
            "the VMDK embedded descriptor lies past the end of the image"
        );
        let bytes = read_at(
            file,
            length,
            header.descriptor_at << SECTOR_LOG,
            (header.descriptor_sectors << SECTOR_LOG) as usize,
            "VMDK embedded descriptor",
        )?;
        let text = descriptor_text(&bytes);
        let text = text.as_ref();
        ensure!(
            !(header.stream() && text.is_empty()),
            "the stream-optimized VMDK's embedded descriptor is empty, which 7-Zip does not read either"
        );
        extents_of(text)?;
        if parent_named(text) {
            return Ok(Err(
                "the image is a differencing VMDK, which needs its parent disk".to_owned(),
            ));
        }
    }
    if header.gd == GD_AT_END {
        ensure!(
            header.stream() && length >= 3 * SECTOR,
            "the VMDK places its grain directory at the end but has no footer"
        );
        ensure!(
            read_marker(file, length, length - 3 * SECTOR)? == Some((1, MARKER_FOOTER)),
            "the VMDK footer lacks its marker"
        );
        ensure!(
            read_marker(file, length, length - SECTOR)? == Some((0, MARKER_END_OF_STREAM)),
            "the VMDK stream does not end with its end marker"
        );
        let footer = read_at(file, length, length - 2 * SECTOR, HEADER, "VMDK footer")?;
        let footer = self::header(footer[..].try_into().unwrap()).map_err(anyhow::Error::from)?;
        ensure!(
            footer.gd != GD_AT_END && footer.same_layout(&header),
            "the VMDK footer does not repeat its header"
        );
        header.gd = footer.gd;
    }
    check_tables(file, length, &header)?;
    Ok(Ok(Sparse {
        header,
        length,
        pages: Pages::default(),
        inflated: None,
    }))
}

pub(crate) fn open(file: ArchiveFile, length: u64) -> Result<Opened> {
    match open_sparse_extent(&file, length)? {
        Err(reason) => Ok(Opened::Elsewhere(reason)),
        Ok(sparse) => {
            let size = sparse.header.capacity << SECTOR_LOG;
            Ok(Opened::Disk(Disk::layered(file, size, Box::new(sparse))))
        }
    }
}

enum Content {
    Flat {
        file: ArchiveFile,
        at: u64,
    },
    Sparse {
        file: ArchiveFile,
        sparse: Box<Sparse>,
    },
}

struct Extent {
    start: u64,
    size: u64,
    content: Content,
}

struct Extents {
    extents: Vec<Extent>,
    last: usize,
}

impl Layer for Extents {
    fn read(&mut self, _: &ArchiveFile, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let index = self
            .extents
            .partition_point(|extent| extent.start + extent.size <= offset);
        if index != self.last
            && let Content::Sparse { sparse, .. } = &mut self.extents[self.last].content
        {
            sparse.forget();
        }
        self.last = index;
        let extent = &mut self.extents[index];
        let within = offset - extent.start;
        let want = (buffer.len() as u64).min(extent.size - within) as usize;
        match &mut extent.content {
            Content::Flat { file, at } => {
                read_file_exact(file, &mut buffer[..want], *at + within)?;
                Ok(want)
            }
            Content::Sparse { file, sparse } => sparse.read(file, within, &mut buffer[..want]),
        }
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.chars().any(char::is_control)
}

fn open_extent(folder: &Path, name: &str) -> Result<(ArchiveFile, u64)> {
    ensure!(
        plain_name(name),
        "the VMDK extent file {name:?} is not one plain file name in the descriptor's folder"
    );
    let path = folder.join(name);
    ensure!(
        !std::fs::symlink_metadata(&path).is_ok_and(|found| found.file_type().is_symlink()),
        "the VMDK extent file {name:?} is a link, which may lead outside the descriptor's folder"
    );
    let (extent, _) = crate::volumes::open_alone(&path)
        .with_context(|| format!("cannot open the VMDK extent file {name:?}"))?;
    let extent_length = extent.len()?;
    Ok((extent, extent_length))
}

pub(crate) fn open_descriptor(path: &Path, file: ArchiveFile, length: u64) -> Result<Opened> {
    ensure!(
        length <= DESCRIPTOR_LIMIT,
        "the VMDK descriptor is over 1 MiB"
    );
    let bytes = read_at(&file, length, 0, length as usize, "VMDK descriptor")?;
    let text = descriptor_text(&bytes);
    let text = text.as_ref();
    ensure!(
        text.starts_with(std::str::from_utf8(DESCRIPTOR_START).unwrap()),
        "the VMDK descriptor does not start with its signature line"
    );
    let lines = extents_of(text)?;
    if parent_named(text) {
        return Ok(Opened::Elsewhere(
            "the image is a differencing VMDK, which needs its parent disk".to_owned(),
        ));
    }
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut extents: Vec<Extent> = Vec::new();
    let mut start = 0u64;
    for found in lines {
        let size = found
            .sectors
            .checked_shl(SECTOR_LOG)
            .filter(|size| size >> SECTOR_LOG == found.sectors)
            .context("a VMDK extent is larger than 2^64 bytes")?;
        let content = match found.kind {
            "FLAT" | "VMFS" => {
                let (extent, extent_length) = open_extent(folder, found.file)?;
                let at = found
                    .offset
                    .checked_shl(SECTOR_LOG)
                    .filter(|at| at >> SECTOR_LOG == found.offset)
                    .context("a flat VMDK extent starts past 2^64")?;
                ensure!(
                    at.checked_add(size).is_some_and(|end| end <= extent_length),
                    "a flat VMDK extent is shorter than the descriptor says"
                );
                Content::Flat { file: extent, at }
            }
            "SPARSE" => {
                let (extent, extent_length) = open_extent(folder, found.file)?;
                let mut magic = [0u8; 4];
                ensure!(
                    extent_length >= HEADER as u64
                        && read_file_exact(&extent, &mut magic, 0).is_ok()
                        && &magic == MAGIC,
                    "a sparse VMDK extent file is not a VMDK sparse extent"
                );
                let sparse = match open_sparse_extent(&extent, extent_length)? {
                    Ok(sparse) => sparse,
                    Err(reason) => return Ok(Opened::Elsewhere(reason)),
                };
                ensure!(
                    sparse.header.capacity == found.sectors,
                    "a sparse VMDK extent's size differs from the descriptor's"
                );
                Content::Sparse {
                    file: extent,
                    sparse: Box::new(sparse),
                }
            }
            "ZERO" => {
                bail!("the VMDK descriptor names a ZERO extent, which 7-Zip does not read either")
            }
            other => {
                return Ok(Opened::Elsewhere(format!(
                    "the VMDK descriptor names a {other} extent"
                )));
            }
        };
        if size == 0 {
            continue;
        }
        extents.push(Extent {
            start,
            size,
            content,
        });
        start = start
            .checked_add(size)
            .filter(|end| *end >> (LARGEST_CAPACITY_LOG + SECTOR_LOG) == 0)
            .context("the VMDK disk is larger than 64 TiB")?;
    }
    Ok(Opened::Disk(Disk::layered(
        file,
        start,
        Box::new(Extents { extents, last: 0 }),
    )))
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn descriptor_lines_follow_the_grammar_7zip_reads() {
        assert_eq!(
            descriptor_line("RW 56192 FLAT \"disk-flat.vmdk\" 0"),
            Ok(Some(ExtentLine {
                sectors: 56192,
                kind: "FLAT",
                file: "disk-flat.vmdk",
                offset: 0,
            }))
        );
        assert_eq!(
            descriptor_line("  rdonly\t8000  SPARSE \"a b.vmdk\" 7 trailing words\r"),
            Ok(Some(ExtentLine {
                sectors: 8000,
                kind: "SPARSE",
                file: "a b.vmdk",
                offset: 7,
            }))
        );
        for skipped in [
            "",
            "# Extent description",
            "ddb.adapterType = \"ide\"",
            "=",
            "key=",
            "a value = \"unclosed",
        ] {
            assert_eq!(descriptor_line(skipped), Ok(None), "{skipped:?}");
        }
        for refused in [
            "junk",
            "junk words",
            "   ",
            "  # indented comment",
            "RW 0x10 FLAT \"a\" 0",
            "RW 10 FLAT a 0",
            "RW 10 FLAT \"a",
            "RW 10 ZERO",
            "RW 10",
            "RW 10 FLAT \"a\" y",
            "dd\".virtualHWVersion = \"4\"",
        ] {
            assert!(descriptor_line(refused).is_err(), "{refused:?}");
        }
        assert!(parent_named("parentCID=12345678\n"));
        assert!(parent_named("parentCID=zz\n"));
        assert!(!parent_named("parentCID=ffffffff\n"));
        assert!(!parent_named("version=1\n"));
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(18)]
    fn headers_never_panic_and_their_tables_cover_the_disk() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(found) = header(&bytes) {
            assert!(found.grain_log <= LARGEST_GRAIN_LOG);
            assert!(found.capacity >> LARGEST_CAPACITY_LOG == 0);
            assert!(!found.stream() || found.compression == DEFLATE);
            let sector: u64 = kani::any();
            kani::assume(sector < found.capacity);
            let grain = sector >> found.grain_log;
            assert!(grain < found.grains());
            assert!(grain >> TABLE_LOG < found.tables());
            assert!(found.tables() * ENTRY < 1 << 50);
        }
    }

    #[kani::proof]
    fn grain_entries_and_markers_never_panic() {
        let entry: u32 = kani::any();
        let flags: u32 = kani::any();
        let header = Header {
            version: 1,
            flags,
            capacity: 0,
            grain_log: 0,
            descriptor_at: 0,
            descriptor_sectors: 0,
            gd: 0,
            overhead: 0,
            compression: 0,
        };
        if let Grain::At(at) = grain_entry(entry, &header) {
            assert!(at != 0 && at % SECTOR == 0 && at < 1 << 41);
        }
        let bytes: [u8; MARKER] = kani::any();
        let _ = marker(&bytes);
    }
}
