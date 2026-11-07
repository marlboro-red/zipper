//! The UDF file system of a disc image, read into the catalog the ISO
//! reader extracts. `docs/reference/iso-reader.md` gives the rules.

use crate::{
    BadField, Field, field_check,
    native_cpio::tree::{BACKEND_HINT, MAX_ENTRIES},
    native_iso::{
        Catalog, Child, DIRECTORY_MODE, Extent, Extents, FILE_MODE, Folder, Image, Kind, NO_PARENT,
        Node, SECTOR, SECTOR_BYTES, Walk, civil_time, walk_folders,
    },
};
use anyhow::anyhow;
use std::time::SystemTime;

const ANCHOR_SECTOR: u64 = 256;
const SMALL_SECTOR: u64 = 512;
const TAG: usize = 16;
const DESCRIPTOR: usize = 512;
const TAG_PARTITION: u16 = 5;
const TAG_ANCHOR: u16 = 2;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATOR: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_IDENTIFIER: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;
const FILE_ENTRY_HEAD: usize = 176;
const EXTENDED_SHIFT: usize = 40;
const FID_HEAD: usize = 38;
const SHORT_AD: usize = 8;
const LONG_AD: usize = 16;
const LENGTH_MASK: u32 = 0x3FFF_FFFF;
const MAX_PARTITIONS: usize = 64;
const MAX_MAPS: u32 = 64;
const MAP_TABLE: usize = 440;
const MAX_UDF_DEPTH: usize = 1024;
const MAX_FOLDER_BYTES: u64 = 64 << 20;
const MIN_FILE_SET_EXTENT: u32 = 512;
const TYPE_DIRECTORY: u8 = 4;
const TYPE_FILE: u8 = 5;
const TYPE_REAL_TIME_FILE: u8 = 249;
const TYPE_METADATA: u8 = 250;
const TYPE_METADATA_BITMAP: u8 = 252;
const AD_SHORT: u8 = 0;
const AD_LONG: u8 = 1;
const AD_EXTENDED: u8 = 2;
const AD_INLINE: u8 = 3;
const RECORDED: u8 = 0;
const FID_DIRECTORY: u8 = 2;
const FID_DELETED: u8 = 4;
const FID_PARENT: u8 = 8;
const METADATA_PARTITION: &[u8] = b"*UDF Metadata Partition";
const MAX_OFFSET_MINUTES: i16 = 1440;

pub(crate) enum Opened {
    NotUdf,
    Unsupported(&'static str),
    Tree(Catalog),
}

enum Failure {
    NotUdf,
    Unsupported(&'static str),
    Refused(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure::Refused(error)
    }
}

type Opening<T> = std::result::Result<T, Failure>;

fn unsupported<T>(field: Field<T>) -> Opening<T> {
    field.map_err(|BadField(reason)| Failure::Unsupported(reason))
}

fn refuse<T>(message: String) -> Opening<T> {
    Err(Failure::Refused(anyhow!(message)))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | u64::from(u32_at(bytes, at + 4)) << 32
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tag {
    id: u16,
    location: u32,
    crc_length: usize,
}

fn tag(bytes: &[u8]) -> Field<Tag> {
    field_check(bytes.len() >= TAG, "UDF descriptor tag is cut short")?;
    let sum = bytes[..TAG]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    field_check(
        sum == bytes[4] && bytes[5] == 0,
        "UDF descriptor tag checksum does not match",
    )?;
    field_check(
        matches!(u16_at(bytes, 2), 2 | 3),
        "UDF descriptor tag has an unknown version",
    )?;
    let crc_length = usize::from(u16_at(bytes, 10));
    let covered = bytes
        .get(TAG..TAG + crc_length)
        .ok_or(BadField("UDF descriptor runs past its block"))?;
    field_check(
        crc16(covered) == u16_at(bytes, 8),
        "UDF descriptor CRC does not match",
    )?;
    Ok(Tag {
        id: u16_at(bytes, 0),
        location: u32_at(bytes, 12),
        crc_length,
    })
}

fn anchor(bytes: &[u8]) -> Option<(u32, u32)> {
    let found = tag(bytes).ok()?;
    (found.id == TAG_ANCHOR && found.location == ANCHOR_SECTOR as u32 && found.crc_length >= TAG)
        .then(|| (u32_at(bytes, 16), u32_at(bytes, 20)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileEntry {
    file_type: u8,
    ad_kind: u8,
    size: u64,
    modified: [u8; 12],
    ads: (usize, usize),
}

fn file_entry(block: &[u8], extended: bool) -> Field<FileEntry> {
    let shift = if extended { EXTENDED_SHIFT } else { 0 };
    let head = FILE_ENTRY_HEAD + shift;
    field_check(block.len() >= head, "UDF file entry is cut short")?;
    let mut modified = [0u8; 12];
    let time = if extended { 92 } else { 84 };
    modified.copy_from_slice(&block[time..time + 12]);
    let attributes = u32_at(block, 168 + shift) as usize;
    let descriptors = u32_at(block, 172 + shift) as usize;
    field_check(
        attributes.is_multiple_of(4),
        "UDF extended attributes are not 4-byte aligned",
    )?;
    let start = head
        .checked_add(attributes)
        .filter(|start| *start <= block.len())
        .ok_or(BadField("UDF extended attributes run past their block"))?;
    let end = start
        .checked_add(descriptors)
        .filter(|end| *end <= block.len())
        .ok_or(BadField("UDF allocation descriptors run past their block"))?;
    Ok(FileEntry {
        file_type: block[27],
        ad_kind: (u16_at(block, 34) & 7) as u8,
        size: u64_at(block, 56),
        modified,
        ads: (start, end),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ad {
    length: u32,
    kind: u8,
    block: u32,
    partition: Option<u16>,
}

fn allocation_descriptors(area: &[u8], long: bool) -> Field<Vec<Ad>> {
    let size = if long { LONG_AD } else { SHORT_AD };
    field_check(
        area.len().is_multiple_of(size),
        "UDF allocation descriptors do not fill their area",
    )?;
    Ok(area
        .chunks_exact(size)
        .map(|ad| {
            let raw = u32_at(ad, 0);
            Ad {
                length: raw & LENGTH_MASK,
                kind: (raw >> 30) as u8,
                block: u32_at(ad, 4),
                partition: long.then(|| u16_at(ad, 8)),
            }
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fid {
    characteristics: u8,
    block: u32,
    partition: u16,
    name: (usize, usize),
    length: usize,
}

fn file_identifier(bytes: &[u8], crc_length: usize) -> Field<Fid> {
    field_check(bytes.len() >= FID_HEAD, "UDF file identifier is cut short")?;
    let name_length = usize::from(bytes[19]);
    let implementation = usize::from(u16_at(bytes, 36));
    let name_start = FID_HEAD + implementation;
    let name_end = name_start + name_length;
    let length = name_end.div_ceil(4) * 4;
    field_check(
        length <= bytes.len(),
        "UDF file identifier runs past its folder",
    )?;
    field_check(
        bytes[name_end..length].iter().all(|byte| *byte == 0),
        "UDF file identifier padding is not zero",
    )?;
    field_check(
        crc_length + TAG == length,
        "UDF file identifier tag does not cover it",
    )?;
    Ok(Fid {
        characteristics: bytes[18],
        block: u32_at(bytes, 24),
        partition: u16_at(bytes, 28),
        name: (name_start, name_end),
        length,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapKind {
    Physical { number: u16 },
    Metadata { number: u16, file: u32 },
}

fn partition_maps(table: &[u8], count: u32) -> Field<Vec<MapKind>> {
    field_check(
        count <= MAX_MAPS,
        "UDF logical volume has more than 64 partition maps",
    )?;
    let mut maps = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        let map = &table[at..];
        field_check(map.len() >= 2, "UDF partition map runs past its table")?;
        let length = usize::from(map[1]);
        field_check(
            length >= 2 && length <= map.len(),
            "UDF partition map runs past its table",
        )?;
        maps.push(match (map[0], length) {
            (1, 6) => MapKind::Physical {
                number: u16_at(map, 4),
            },
            (2, 64) => {
                field_check(
                    u16_at(map, 2) == 0,
                    "UDF partition map has reserved bytes set",
                )?;
                field_check(
                    &map[5..5 + METADATA_PARTITION.len()] == METADATA_PARTITION,
                    "UDF virtual and sparable partitions are not supported",
                )?;
                MapKind::Metadata {
                    number: u16_at(map, 38),
                    file: u32_at(map, 40),
                }
            }
            _ => return Err(BadField("UDF partition map has an unknown type")),
        });
        at += length;
    }
    Ok(maps)
}

fn udf_time_fields(stamp: &[u8; 12]) -> (u16, [u8; 5], i16, u32) {
    let zone = (u16_at(stamp, 0) & 0xFFF) as i16;
    let minutes = if zone & 0x800 != 0 { zone - 4096 } else { zone };
    let offset = if minutes.abs() > MAX_OFFSET_MINUTES {
        0
    } else {
        minutes
    };
    let nanoseconds = if stamp[9..12].iter().all(|part| *part <= 99) {
        u32::from(stamp[9]) * 10_000_000
            + u32::from(stamp[10]) * 100_000
            + u32::from(stamp[11]) * 1000
    } else {
        0
    };
    (
        u16_at(stamp, 2),
        [stamp[4], stamp[5], stamp[6], stamp[7], stamp[8]],
        offset,
        nanoseconds,
    )
}

fn udf_time(stamp: &[u8; 12]) -> Option<SystemTime> {
    let (year, fields, offset, nanoseconds) = udf_time_fields(stamp);
    civil_time(year, fields, offset, nanoseconds)
}

fn decoded_name(bytes: &[u8]) -> anyhow::Result<String> {
    let name = match bytes.split_first() {
        None => String::new(),
        Some((8, rest)) => {
            anyhow::ensure!(!rest.contains(&0), "UDF name holds NUL");
            rest.iter().map(|byte| char::from(*byte)).collect()
        }
        Some((16, rest)) => {
            anyhow::ensure!(rest.len() % 2 == 0, "UDF name has an odd length");
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|unit| u16::from_be_bytes([unit[0], unit[1]]))
                .collect();
            anyhow::ensure!(!units.contains(&0), "UDF name holds NUL");
            String::from_utf16(&units).map_err(|_| anyhow!("UDF name has an unpaired surrogate"))?
        }
        Some((other, _)) => anyhow::bail!("UDF name has unknown compression {other}"),
    };
    let name = if name.trim_matches([' ', '\t', '\n']).is_empty() {
        String::from("[]")
    } else {
        name
    };
    crate::native_iso::ensure_single_name("UDF", &name)?;
    Ok(name)
}

#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    length: u64,
}

enum Data {
    Inline(Extent),
    Extents(Vec<(Extent, u8)>),
}

struct Item {
    file_type: u8,
    size: u64,
    modified: Option<SystemTime>,
    data: Data,
}

impl Item {
    fn chunks_match(&self) -> bool {
        match &self.data {
            Data::Inline(extent) => extent.length == self.size,
            Data::Extents(extents) => {
                extents.iter().all(|(_, kind)| *kind == RECORDED)
                    && extents.iter().map(|(extent, _)| extent.length).sum::<u64>() == self.size
            }
        }
    }

    fn extents(&self) -> Vec<Extent> {
        match &self.data {
            Data::Inline(extent) => vec![*extent],
            Data::Extents(extents) => extents
                .iter()
                .map(|(extent, _)| *extent)
                .filter(|extent| extent.length > 0)
                .collect(),
        }
    }
}

struct UdfFolder {
    span: usize,
    block: u32,
    extents: Vec<Extent>,
}

impl Folder for UdfFolder {
    type Key = (usize, u32);
    fn key(&self) -> (usize, u32) {
        (self.span, self.block)
    }
}

struct Volume {
    spans: Vec<Span>,
    maps: Vec<usize>,
}

impl Volume {
    fn span_of_map(&self, map: u16) -> Opening<usize> {
        self.maps
            .get(usize::from(map))
            .copied()
            .ok_or(Failure::Unsupported(
                "UDF address names a missing partition map",
            ))
    }

    fn read_item(&self, image: &mut Image, span: usize, block: u32) -> Opening<(Item, Tag)> {
        let partition = self.spans[span];
        let at = u64::from(block) * SECTOR;
        if at + SECTOR > partition.length {
            return refuse(format!(
                "UDF entry at block {block} lies outside its partition"
            ));
        }
        let mut bytes = [0u8; SECTOR_BYTES];
        image.read_exact_at(&mut bytes, partition.start + at, "UDF file entries")?;
        let found = unsupported(tag(&bytes))?;
        let extended = match found.id {
            TAG_FILE_ENTRY => false,
            TAG_EXTENDED_FILE_ENTRY => true,
            _ => return Err(Failure::Unsupported("UDF file entry has the wrong tag")),
        };
        let entry = unsupported(file_entry(&bytes, extended))?;
        let data = match entry.ad_kind {
            AD_INLINE => Data::Inline(Extent {
                offset: partition.start + at + entry.ads.0 as u64,
                length: (entry.ads.1 - entry.ads.0) as u64,
            }),
            AD_SHORT | AD_LONG => {
                let area = &bytes[entry.ads.0..entry.ads.1];
                let ads = unsupported(allocation_descriptors(area, entry.ad_kind == AD_LONG))?;
                let mut extents = Vec::with_capacity(ads.len());
                for ad in ads {
                    let span = match ad.partition {
                        Some(map) => self.span_of_map(map)?,
                        None => span,
                    };
                    let partition = self.spans[span];
                    let start = u64::from(ad.block) * SECTOR;
                    let length = u64::from(ad.length);
                    if start + length > partition.length {
                        return refuse(format!(
                            "UDF extent at block {} lies outside its partition",
                            ad.block
                        ));
                    }
                    extents.push((
                        Extent {
                            offset: partition.start + start,
                            length,
                        },
                        ad.kind,
                    ));
                }
                Data::Extents(extents)
            }
            AD_EXTENDED => {
                return Err(Failure::Unsupported(
                    "UDF extended allocation descriptors are not supported",
                ));
            }
            _ => {
                return refuse(String::from(
                    "UDF file entry has a reserved allocation descriptor type",
                ));
            }
        };
        let item = Item {
            file_type: entry.file_type,
            size: entry.size,
            modified: udf_time(&entry.modified),
            data,
        };
        for extent in item.extents() {
            image.ensure_inside(extent, "a UDF extent")?;
        }
        Ok((item, found))
    }

    fn metadata_span(&self, image: &mut Image, physical: usize, file: u32) -> Opening<Span> {
        let (item, _) = self.read_item(image, physical, file)?;
        if !(TYPE_METADATA..=TYPE_METADATA_BITMAP).contains(&item.file_type) {
            return Err(Failure::Unsupported("UDF metadata file has the wrong type"));
        }
        let Data::Extents(extents) = item.data else {
            return Err(Failure::Unsupported("UDF metadata file is not in extents"));
        };
        let Some(((first, kind), rest)) = extents.split_first() else {
            return Err(Failure::Unsupported("UDF metadata file has no extent"));
        };
        if *kind != RECORDED || rest.iter().any(|(extent, _)| extent.length != 0) {
            return Err(Failure::Unsupported(
                "UDF metadata file has several extents",
            ));
        }
        Ok(Span {
            start: first.offset,
            length: first.length / SECTOR * SECTOR,
        })
    }

    fn folder_bytes(&self, image: &mut Image, extents: &[Extent]) -> Opening<Vec<u8>> {
        let mut sorted = extents.to_vec();
        sorted.sort_unstable_by_key(|extent| extent.offset);
        if sorted
            .windows(2)
            .any(|pair| pair[0].offset + pair[0].length > pair[1].offset)
        {
            return refuse(String::from("a UDF folder's extents overlap"));
        }
        let total: u64 = extents.iter().map(|extent| extent.length).sum();
        if total > MAX_FOLDER_BYTES {
            return refuse(format!(
                "a UDF folder holds more than {MAX_FOLDER_BYTES} bytes"
            ));
        }
        image.charge_directory(total)?;
        let mut bytes = vec![0u8; total as usize];
        let mut at = 0usize;
        for extent in extents {
            let piece = &mut bytes[at..at + extent.length as usize];
            image.read_exact_at(piece, extent.offset, "UDF folders")?;
            at += extent.length as usize;
        }
        Ok(bytes)
    }

    fn child(
        &self,
        image: &mut Image,
        span: usize,
        block: u32,
        name: String,
        directory: bool,
    ) -> Opening<Child<UdfFolder>> {
        let (item, _) = self.read_item(image, span, block)?;
        if !matches!(
            item.file_type,
            TYPE_DIRECTORY | TYPE_FILE | TYPE_REAL_TIME_FILE
        ) {
            return Err(Failure::Unsupported(
                "UDF entry is not a file or folder, such as a link, which 7-Zip does not open",
            ));
        }
        if (item.file_type == TYPE_DIRECTORY) != directory {
            return Err(Failure::Unsupported(
                "UDF folder entry and its file identifier disagree",
            ));
        }
        if !item.chunks_match() {
            if directory {
                return Err(Failure::Unsupported(
                    "UDF folder extents do not match its size",
                ));
            }
            return refuse(format!(
                "UDF file {name:?} has extents that do not hold its size, which 7-Zip does not extract"
            ));
        }
        let extents = item.extents();
        Ok(Child {
            folder: directory.then(|| UdfFolder {
                span,
                block,
                extents: extents.clone(),
            }),
            node: Node {
                parent: NO_PARENT,
                name: name.into(),
                kind: if directory {
                    Kind::Directory
                } else {
                    Kind::File
                },
                mode: if directory { DIRECTORY_MODE } else { FILE_MODE },
                modified: item.modified,
                size: if directory { 0 } else { item.size },
                extents: if directory {
                    Extents::None
                } else {
                    Extents::from_vec(extents)
                },
            },
        })
    }

    fn list(&self, image: &mut Image, folder: UdfFolder) -> Opening<Vec<Child<UdfFolder>>> {
        let bytes = self.folder_bytes(image, &folder.extents)?;
        let mut children = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            let rest = &bytes[at..];
            let found = unsupported(tag(rest))?;
            if found.id != TAG_FILE_IDENTIFIER {
                return Err(Failure::Unsupported("UDF folder holds another descriptor"));
            }
            let fid = unsupported(file_identifier(rest, found.crc_length))?;
            at += fid.length;
            if fid.characteristics & (FID_DELETED | FID_PARENT) != 0 {
                continue;
            }
            crate::native_iso::ensure_folder_fits(children.len() + 1)?;
            let name = decoded_name(&rest[fid.name.0..fid.name.1])?;
            let span = self.span_of_map(fid.partition)?;
            children.push(self.child(
                image,
                span,
                fid.block,
                name,
                fid.characteristics & FID_DIRECTORY != 0,
            )?);
        }
        Ok(children)
    }
}

type DescriptorSequence = (Vec<(u16, Span)>, Vec<[u8; SECTOR_BYTES]>);

fn read_descriptor_sequence(
    image: &mut Image,
    length: u32,
    location: u32,
) -> Opening<DescriptorSequence> {
    let mut partitions = Vec::new();
    let mut volumes = Vec::new();
    let mut sector = [0u8; SECTOR_BYTES];
    for index in 0..u64::from(length) / SECTOR {
        let at = (u64::from(location) + index) * SECTOR;
        image.read_exact_at(&mut sector, at, "UDF volume descriptors")?;
        let found = unsupported(tag(&sector))?;
        match found.id {
            TAG_PARTITION => {
                if partitions.len() >= MAX_PARTITIONS {
                    return Err(Failure::Unsupported(
                        "UDF volume has more than 64 partitions",
                    ));
                }
                let number = u16_at(&sector, 22);
                if partitions.iter().any(|(known, _)| *known == number) {
                    return refuse(format!("UDF volume describes partition {number} twice"));
                }
                partitions.push((
                    number,
                    Span {
                        start: u64::from(u32_at(&sector, 188)) * SECTOR,
                        length: u64::from(u32_at(&sector, 192)) * SECTOR,
                    },
                ));
            }
            TAG_LOGICAL_VOLUME => volumes.push(sector),
            TAG_TERMINATOR => return Ok((partitions, volumes)),
            _ => {}
        }
    }
    Err(Failure::Unsupported(
        "UDF volume descriptor sequence has no terminator",
    ))
}

fn open(image: &mut Image) -> Opening<Catalog> {
    let mut bytes = [0u8; DESCRIPTOR];
    let mut at_sector = |image: &mut Image, sector: u64| -> Opening<Option<(u32, u32)>> {
        if image.length < sector * (ANCHOR_SECTOR + 1) {
            return Ok(None);
        }
        image.read_exact_at(&mut bytes, ANCHOR_SECTOR * sector, "UDF anchor")?;
        Ok(anchor(&bytes))
    };
    let Some((length, location)) = at_sector(image, SECTOR)? else {
        if at_sector(image, SMALL_SECTOR)?.is_some() {
            return refuse(format!(
                "UDF with 512-byte sectors is not supported; {BACKEND_HINT}"
            ));
        }
        return Err(Failure::NotUdf);
    };
    let (partitions, volumes) = read_descriptor_sequence(image, length, location)?;
    let lvd = match volumes.as_slice() {
        [] => {
            return Ok(Catalog {
                format: "UDF",
                nodes: Vec::new(),
            });
        }
        [lvd] => lvd,
        _ => {
            return refuse(format!(
                "UDF image has several logical volumes; {BACKEND_HINT}"
            ));
        }
    };
    if u64::from(u32_at(lvd, 212)) != SECTOR {
        return Err(Failure::Unsupported(
            "UDF block size is not the sector size",
        ));
    }
    let table_length = u32_at(lvd, 264) as usize;
    let Some(table) = lvd.get(MAP_TABLE..MAP_TABLE.saturating_add(table_length)) else {
        return Err(Failure::Unsupported(
            "UDF partition map table runs past its block",
        ));
    };
    let kinds = unsupported(partition_maps(table, u32_at(lvd, 268)))?;
    let mut volume = Volume {
        spans: Vec::new(),
        maps: Vec::new(),
    };
    let find = |number: u16| {
        partitions
            .iter()
            .find(|(known, _)| *known == number)
            .map(|(_, span)| *span)
            .ok_or(Failure::Unsupported(
                "UDF partition map names a missing partition",
            ))
    };
    for kind in &kinds {
        let number = match kind {
            MapKind::Physical { number } | MapKind::Metadata { number, .. } => *number,
        };
        volume.spans.push(find(number)?);
        volume.maps.push(volume.spans.len() - 1);
    }
    for (index, kind) in kinds.iter().enumerate() {
        if let MapKind::Metadata { file, .. } = kind {
            let span = volume.metadata_span(image, volume.maps[index], *file)?;
            volume.spans.push(span);
            volume.maps[index] = volume.spans.len() - 1;
        }
    }
    if u32_at(lvd, 248) & LENGTH_MASK < MIN_FILE_SET_EXTENT {
        return Err(Failure::Unsupported("UDF file set extent is too short"));
    }
    let file_set_span = volume.span_of_map(u16_at(lvd, 256))?;
    let file_set_block = u32_at(lvd, 252);
    let partition = volume.spans[file_set_span];
    let at = u64::from(file_set_block) * SECTOR;
    if at + SECTOR > partition.length {
        return refuse(String::from("UDF file set lies outside its partition"));
    }
    let mut file_set = [0u8; DESCRIPTOR];
    image.read_exact_at(&mut file_set, partition.start + at, "UDF file set")?;
    if unsupported(tag(&file_set))?.id != TAG_FILE_SET {
        return Err(Failure::Unsupported("UDF file set has the wrong tag"));
    }
    let root_span = volume.span_of_map(u16_at(&file_set, 408))?;
    let root = volume.child(
        image,
        root_span,
        u32_at(&file_set, 404),
        String::new(),
        true,
    )?;
    let root = root.folder.expect("a folder child has a folder");
    let mut nodes = Vec::new();
    let mut failure = None;
    let walked = walk_folders(root, MAX_UDF_DEPTH, MAX_ENTRIES, &mut nodes, |folder| {
        volume.list(image, folder).map_err(|error| {
            let message = match &error {
                Failure::Unsupported(reason) => anyhow!("{reason}"),
                Failure::Refused(error) => anyhow!("{error:#}"),
                Failure::NotUdf => anyhow!("not UDF"),
            };
            failure = Some(error);
            message
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    match walked? {
        Walk::Tree => Ok(Catalog {
            format: "UDF",
            nodes,
        }),
        Walk::Cycle => Err(Failure::Unsupported("a UDF folder contains itself")),
        Walk::TooDeep => Err(Failure::Unsupported("UDF folders nest more than 1024 deep")),
        Walk::Shared => refuse(String::from(
            "a UDF folder is reached from two folders, which 7-Zip extracts twice",
        )),
        Walk::TooMany => refuse(format!("the image has more than {MAX_ENTRIES} entries")),
    }
}

pub(crate) fn read(image: &mut Image) -> anyhow::Result<Opened> {
    match open(image) {
        Ok(catalog) => Ok(Opened::Tree(catalog)),
        Err(Failure::NotUdf) => Ok(Opened::NotUdf),
        Err(Failure::Unsupported(reason)) => Ok(Opened::Unsupported(reason)),
        Err(Failure::Refused(error)) => Err(error),
    }
}

pub(crate) fn has_anchor(image: &mut Image) -> anyhow::Result<bool> {
    let mut bytes = [0u8; DESCRIPTOR];
    for sector in [SECTOR, SMALL_SECTOR] {
        if image.length >= sector * (ANCHOR_SECTOR + 1) {
            image.read_exact_at(&mut bytes, ANCHOR_SECTOR * sector, "UDF anchor")?;
            if anchor(&bytes).is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn tagged(id: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; TAG];
        bytes.extend_from_slice(body);
        bytes[0..2].copy_from_slice(&id.to_le_bytes());
        bytes[2..4].copy_from_slice(&2u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&(body.len() as u16).to_le_bytes());
        let crc = crc16(body);
        bytes[8..10].copy_from_slice(&crc.to_le_bytes());
        let sum = bytes[..TAG]
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 4)
            .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
        bytes[4] = sum;
        bytes
    }

    #[test]
    fn the_crc_is_the_ccitt_crc_of_ecma_167() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn tags_are_accepted_only_whole_and_unchanged() {
        let bytes = tagged(TAG_FILE_SET, b"body of the tag");
        assert_eq!(tag(&bytes).unwrap().id, TAG_FILE_SET);
        for index in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[index] ^= 0x10;
            assert!(tag(&damaged).is_err(), "byte {index}");
        }
        for length in 0..bytes.len() {
            assert!(tag(&bytes[..length]).is_err(), "cut at {length}");
        }
    }

    #[test]
    fn names_decode_as_7zip_decodes_them() {
        assert_eq!(decoded_name(b"\x08caf\xe9").unwrap(), "café");
        assert_eq!(decoded_name(b"\x10\x00a\xd8\x3c\xdf\x75").unwrap(), "a🍵");
        assert_eq!(decoded_name(b"").unwrap(), "[]");
        assert_eq!(decoded_name(b"\x08 \t ").unwrap(), "[]");
        assert_eq!(decoded_name(b"\x08 x").unwrap(), " x");
        for bad in [
            &b"\x08a\0"[..],
            b"\x10\0",
            b"\x10\0\0",
            b"\x07a",
            b"\x10\xdc\x00",
            b"\x08a/b",
        ] {
            assert!(decoded_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn time_zones_out_of_range_read_as_utc() {
        let mut stamp = [0u8; 12];
        stamp[0..2].copy_from_slice(&(0x1000u16 | 0xFC4).to_le_bytes());
        stamp[2..4].copy_from_slice(&2024u16.to_le_bytes());
        stamp[4..9].copy_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(udf_time_fields(&stamp).2, -60);
        stamp[0..2].copy_from_slice(&(0x1000u16 | 0x801).to_le_bytes());
        assert_eq!(udf_time_fields(&stamp).2, 0);
        stamp[9] = 100;
        assert_eq!(udf_time_fields(&stamp).3, 0);
        stamp[9..12].copy_from_slice(&[12, 34, 56]);
        assert_eq!(udf_time_fields(&stamp).3, 123_456_000);
    }

    #[test]
    fn file_identifiers_cover_exactly_their_padded_bytes() {
        let mut fid = vec![0u8; 44];
        fid[19] = 3;
        fid[38..41].copy_from_slice(b"\x08ab");
        let read = file_identifier(&fid, 28).unwrap();
        assert_eq!((read.name, read.length), ((38, 41), 44));
        assert!(file_identifier(&fid, 27).is_err());
        fid[42] = 1;
        assert!(file_identifier(&fid, 28).is_err());
        assert!(file_identifier(&fid[..43], 28).is_err());
    }

    #[test]
    fn partition_maps_of_each_kind_are_read_and_others_refused() {
        let mut table = vec![1u8, 6, 1, 0, 7, 0];
        let mut metadata = vec![0u8; 64];
        metadata[0] = 2;
        metadata[1] = 64;
        metadata[5..5 + METADATA_PARTITION.len()].copy_from_slice(METADATA_PARTITION);
        metadata[38] = 7;
        metadata[40] = 9;
        table.extend_from_slice(&metadata);
        assert_eq!(
            partition_maps(&table, 2).unwrap(),
            [
                MapKind::Physical { number: 7 },
                MapKind::Metadata { number: 7, file: 9 }
            ]
        );
        assert!(partition_maps(&table, 3).is_err());
        let mut sparable = table.clone();
        sparable[11..11 + 23].copy_from_slice(b"*UDF Sparable Partition");
        assert!(partition_maps(&sparable, 2).is_err());
        assert!(partition_maps(&[1, 0], 1).is_err());
        assert!(partition_maps(&[], 65).is_err());
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
    #[kani::unwind(17)]
    fn tags_never_panic_and_cover_only_their_bytes() {
        let bytes: [u8; 22] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(found) = tag(bytes) {
            assert!(TAG + found.crc_length <= bytes.len());
        }
    }

    #[kani::proof]
    fn file_entries_never_panic_and_keep_descriptors_inside_their_block() {
        let bytes: [u8; 232] = kani::any();
        let block = prefix(&bytes);
        let extended: bool = kani::any();
        if let Ok(entry) = file_entry(block, extended) {
            assert!(entry.ads.0 <= entry.ads.1 && entry.ads.1 <= block.len());
            assert!(entry.ads.0 >= FILE_ENTRY_HEAD);
            assert!(entry.ad_kind < 8);
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn allocation_descriptors_never_panic_and_fill_their_area() {
        let bytes: [u8; 32] = kani::any();
        let area = prefix(&bytes);
        let long: bool = kani::any();
        if let Ok(ads) = allocation_descriptors(area, long) {
            assert!(ads.len() * if long { LONG_AD } else { SHORT_AD } == area.len());
            for ad in ads {
                assert!(ad.length <= LENGTH_MASK && ad.kind < 4);
                assert!(ad.partition.is_some() == long);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn file_identifiers_never_panic_and_name_inside_their_length() {
        let bytes: [u8; 48] = kani::any();
        let bytes = prefix(&bytes);
        if let Ok(fid) = file_identifier(bytes, kani::any()) {
            assert!(fid.name.0 >= FID_HEAD && fid.name.0 <= fid.name.1);
            assert!(fid.name.1 <= fid.length && fid.length <= bytes.len());
            assert!(fid.length % 4 == 0 && fid.length >= FID_HEAD + 2);
        }
    }

    #[kani::proof]
    #[kani::unwind(24)]
    fn partition_maps_never_panic_and_stay_inside_their_table() {
        let bytes: [u8; 70] = kani::any();
        let table = prefix(&bytes);
        let count: u32 = kani::any();
        kani::assume(count <= 3 || count > MAX_MAPS);
        if let Ok(maps) = partition_maps(table, count) {
            assert!(maps.len() == count as usize);
        }
    }

    #[kani::proof]
    #[kani::unwind(4)]
    fn time_fields_never_overflow_and_keep_offsets_within_a_day() {
        let stamp: [u8; 12] = kani::any();
        let (_, _, offset, nanoseconds) = udf_time_fields(&stamp);
        assert!(offset.abs() <= MAX_OFFSET_MINUTES);
        assert!(nanoseconds < 1_000_000_000);
    }
}
