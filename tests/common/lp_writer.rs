#![allow(dead_code)]
//! A writer for Android logical partition images, from the liblp metadata
//! format: 4 KiB reserved, two geometry copies, the metadata slots and
//! their backups, then the partition data.
use sha2::Digest;

pub const SECTOR: u64 = 512;
pub const GEOMETRY_AT: usize = 4096;
pub const METADATA_AT: usize = 12288;
const ALIGNMENT: u64 = 4096;

pub enum Data {
    Bytes(Vec<u8>),
    At(u64),
    Zero,
    Kind(u32),
}

pub struct Extent {
    pub sectors: u64,
    pub data: Data,
}

pub struct Partition {
    pub name: Vec<u8>,
    pub attributes: u32,
    pub extents: Vec<Extent>,
    pub group: u32,
}

pub struct Layout {
    pub partitions: Vec<Partition>,
    pub slots: u32,
    pub max_size: u32,
    pub block_size: u32,
    pub minor: u16,
    pub groups: Vec<(&'static str, u32, u64)>,
    pub device_size: Option<u64>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            partitions: Vec::new(),
            slots: 2,
            max_size: 65536,
            block_size: 4096,
            minor: 0,
            groups: vec![("default", 0, 0)],
            device_size: None,
        }
    }
}

pub fn linear(sectors: u64, bytes: &[u8]) -> Extent {
    Extent {
        sectors,
        data: Data::Bytes(bytes.to_vec()),
    }
}

pub fn zero(sectors: u64) -> Extent {
    Extent {
        sectors,
        data: Data::Zero,
    }
}

pub fn at(sectors: u64, sector: u64) -> Extent {
    Extent {
        sectors,
        data: Data::At(sector),
    }
}

pub fn partition(name: &str, extents: Vec<Extent>) -> Partition {
    Partition {
        name: name.as_bytes().to_vec(),
        attributes: 0,
        extents,
        group: 0,
    }
}

pub fn header_size(minor: u16) -> usize {
    if minor >= 2 { 256 } else { 128 }
}

pub fn build(layout: &Layout) -> Vec<u8> {
    let metadata_end =
        METADATA_AT as u64 + 2 * u64::from(layout.slots) * u64::from(layout.max_size);
    let first_data = metadata_end.next_multiple_of(ALIGNMENT);
    let mut next = first_data / SECTOR;
    let (mut partitions, mut extents, mut payload) = (Vec::new(), Vec::new(), Vec::new());
    for part in &layout.partitions {
        let first = (extents.len() / 24) as u32;
        for extent in &part.extents {
            let (kind, target) = match &extent.data {
                Data::Bytes(bytes) => {
                    payload.push((next, bytes.clone(), extent.sectors));
                    next += extent.sectors;
                    (0, next - extent.sectors)
                }
                Data::At(sector) => (0, *sector),
                Data::Zero => (1, 0),
                Data::Kind(kind) => (*kind, 0),
            };
            extents.extend_from_slice(&extent.sectors.to_le_bytes());
            extents.extend_from_slice(&kind.to_le_bytes());
            extents.extend_from_slice(&target.to_le_bytes());
            extents.extend_from_slice(&0u32.to_le_bytes());
        }
        let mut name = part.name.clone();
        name.resize(36, 0);
        partitions.extend_from_slice(&name);
        for word in [
            part.attributes,
            first,
            part.extents.len() as u32,
            part.group,
        ] {
            partitions.extend_from_slice(&word.to_le_bytes());
        }
    }
    let mut groups = Vec::new();
    for (name, flags, maximum) in &layout.groups {
        let mut field = name.as_bytes().to_vec();
        field.resize(36, 0);
        groups.extend_from_slice(&field);
        groups.extend_from_slice(&flags.to_le_bytes());
        groups.extend_from_slice(&maximum.to_le_bytes());
    }
    let size = layout.device_size.unwrap_or(next * SECTOR + ALIGNMENT);
    let mut device = Vec::new();
    device.extend_from_slice(&(first_data / SECTOR).to_le_bytes());
    device.extend_from_slice(&(ALIGNMENT as u32).to_le_bytes());
    device.extend_from_slice(&0u32.to_le_bytes());
    device.extend_from_slice(&size.to_le_bytes());
    let mut name = b"super".to_vec();
    name.resize(36, 0);
    device.extend_from_slice(&name);
    device.extend_from_slice(&0u32.to_le_bytes());
    let tables_parts = [
        (&partitions, 52),
        (&extents, 24),
        (&groups, 48),
        (&device, 64),
    ];
    let tables: Vec<u8> = tables_parts
        .iter()
        .flat_map(|(table, _)| table.iter().copied())
        .collect();
    let hsize = header_size(layout.minor);
    let mut header = Vec::new();
    header.extend_from_slice(&0x414C_5030u32.to_le_bytes());
    header.extend_from_slice(&10u16.to_le_bytes());
    header.extend_from_slice(&layout.minor.to_le_bytes());
    header.extend_from_slice(&(hsize as u32).to_le_bytes());
    header.extend_from_slice(&[0; 32]);
    header.extend_from_slice(&(tables.len() as u32).to_le_bytes());
    header.extend_from_slice(&[0; 32]);
    let mut offset = 0u32;
    for (table, entry) in tables_parts {
        for word in [offset, (table.len() / entry) as u32, entry as u32] {
            header.extend_from_slice(&word.to_le_bytes());
        }
        offset += table.len() as u32;
    }
    header.resize(hsize, 0);
    let mut metadata = header;
    metadata.extend_from_slice(&tables);
    metadata.resize(layout.max_size as usize, 0);
    let mut geometry = Vec::new();
    for word in [0x616C_4467u32, 52] {
        geometry.extend_from_slice(&word.to_le_bytes());
    }
    geometry.extend_from_slice(&[0; 32]);
    for word in [layout.max_size, layout.slots, layout.block_size] {
        geometry.extend_from_slice(&word.to_le_bytes());
    }
    let length = (size as usize).max(
        payload
            .iter()
            .map(|(at, _, sectors)| ((at + sectors) * SECTOR) as usize)
            .max()
            .unwrap_or(0),
    );
    let mut image = vec![0u8; length.max(metadata_end as usize)];
    image[GEOMETRY_AT..GEOMETRY_AT + 52].copy_from_slice(&geometry);
    for slot in 0..2 * layout.slots as usize {
        let at = METADATA_AT + slot * layout.max_size as usize;
        image[at..at + metadata.len()].copy_from_slice(&metadata);
    }
    for (at, bytes, _) in payload {
        let at = (at * SECTOR) as usize;
        image[at..at + bytes.len()].copy_from_slice(&bytes);
    }
    repair(&mut image, layout.slots, layout.max_size);
    image
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Recompute the geometry, header and tables checksums of the primary
/// geometry and every metadata slot, so that an edit reaches the fields.
pub fn repair(image: &mut [u8], slots: u32, max_size: u32) {
    for geometry in [GEOMETRY_AT, GEOMETRY_AT + 4096] {
        if image.get(geometry..geometry + 52).is_none() || le32(image, geometry) != 0x616C_4467 {
            continue;
        }
        image[geometry + 8..geometry + 40].fill(0);
        let sum = sha256(&image[geometry..geometry + 52]);
        image[geometry + 8..geometry + 40].copy_from_slice(&sum);
    }
    for slot in 0..2 * slots as usize {
        let at = METADATA_AT + slot * max_size as usize;
        let Some(head) = image.get(at..at + 48) else {
            continue;
        };
        let (hsize, tsize) = (le32(head, 8) as usize, le32(head, 44) as usize);
        let Some(tables) = image.get(at + hsize..at + hsize + tsize) else {
            continue;
        };
        let sum = sha256(tables);
        image[at + 48..at + 80].copy_from_slice(&sum);
        image[at + 12..at + 44].fill(0);
        let Some(header) = image.get(at..at + hsize) else {
            continue;
        };
        let sum = sha256(header);
        image[at + 12..at + 44].copy_from_slice(&sum);
    }
}

/// Repair only the checksums of the primary geometry and the first slot,
/// the copies 7-Zip reads, from a layout's slot size.
pub fn repair_first(image: &mut [u8], max_size: u32) {
    repair(image, 1, max_size);
}
