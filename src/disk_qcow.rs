use crate::{
    BadField, Field,
    disk::{Claims, Disk, Layer, Opened, Pages, read_at, read_file_exact},
    field_check,
    volumes::ArchiveFile,
};
use anyhow::{Context, Result, bail, ensure};
use std::io;

pub(crate) const MAGIC: &[u8; 4] = b"QFI\xfb";
pub(crate) const HEADER_READ: usize = 112;
const V1_HEADER: u64 = 48;
const V2_HEADER: u64 = 72;
const V3_HEADER: u32 = 104;
const COMPRESSION_TYPE_AT: usize = 104;
const SMALLEST_CLUSTER_LOG: u32 = 9;
const LARGEST_CLUSTER_LOG: u32 = 21;
const V1_LARGEST_CLUSTER_LOG: u32 = 16;
const V1_SMALLEST_TABLE_LOG: u32 = 6;
const LARGEST_DISK_LOG: u32 = 46;
const FILE_OFFSET_LOG: u32 = 56;
const L1_ENTRY_LIMIT: u64 = 1 << 22;
const ENTRY: u64 = 8;
const OFFSET_MASK: u64 = 0x00FF_FFFF_FFFF_FE00;
const COPIED: u64 = 1 << 63;
const COMPRESSED: u64 = 1 << 62;
const V1_COMPRESSED: u64 = 1 << 63;
const ZERO: u64 = 1;
const L1_RESERVED: u64 = !(OFFSET_MASK | COPIED);
const L2_RESERVED: u64 = !(OFFSET_MASK | COPIED | ZERO);
const DIRTY: u64 = 1;
const CORRUPT: u64 = 1 << 1;
const EXTERNAL_DATA: u64 = 1 << 2;
const COMPRESSION_TYPE: u64 = 1 << 3;
const EXTENDED_L2: u64 = 1 << 4;
const KNOWN_INCOMPATIBLE: u64 = DIRTY | CORRUPT | EXTERNAL_DATA | COMPRESSION_TYPE | EXTENDED_L2;
const ZLIB: u8 = 0;
const ZSTD: u8 = 1;
const COMPRESSED_SECTOR: u64 = 512;
const TABLE_BATCH: usize = 1 << 16;

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) version: u32,
    pub(crate) cluster_log: u32,
    pub(crate) table_log: u32,
    pub(crate) size: u64,
    pub(crate) l1_at: u64,
    pub(crate) l1_entries: u64,
    pub(crate) header_end: u64,
    pub(crate) backing: bool,
    pub(crate) encrypted: bool,
    pub(crate) zstd: bool,
    pub(crate) unread: Option<&'static str>,
    pub(crate) refcount_at: u64,
    pub(crate) refcount_clusters: u32,
}

impl Header {
    pub(crate) fn table_bytes(&self) -> u64 {
        ENTRY << self.table_log
    }
}

pub(crate) fn needed_l1_entries(size: u64, cluster_log: u32, table_log: u32) -> u64 {
    let span_log = cluster_log + table_log;
    (size >> span_log) + u64::from(size & ((1 << span_log) - 1) != 0)
}

pub(crate) fn header(p: &[u8; HEADER_READ]) -> Field<Header> {
    field_check(&p[..4] == MAGIC, "not a QCOW image")?;
    let version = be32(p, 4);
    let mut header = Header {
        version,
        cluster_log: 0,
        table_log: 0,
        size: be64(p, 24),
        l1_at: 0,
        l1_entries: 0,
        header_end: V1_HEADER,
        backing: be64(p, 8) != 0,
        encrypted: false,
        zstd: false,
        unread: None,
        refcount_at: 0,
        refcount_clusters: 0,
    };
    field_check(
        header.size >> LARGEST_DISK_LOG == 0,
        "the QCOW disk is larger than 64 TiB",
    )?;
    if version == 1 {
        header.cluster_log = u32::from(p[32]);
        header.table_log = u32::from(p[33]);
        field_check(
            (SMALLEST_CLUSTER_LOG..=V1_LARGEST_CLUSTER_LOG).contains(&header.cluster_log),
            "the QCOW cluster size is not a power of two of 512 bytes to 64 KiB",
        )?;
        field_check(
            (V1_SMALLEST_TABLE_LOG..=header.cluster_log).contains(&header.table_log),
            "the QCOW second-level table size is out of range",
        )?;
        header.encrypted = be32(p, 36) != 0;
        header.l1_at = be64(p, 40);
        header.l1_entries = needed_l1_entries(header.size, header.cluster_log, header.table_log);
        field_check(
            header.l1_entries <= L1_ENTRY_LIMIT,
            "the QCOW first-level table holds more than 4,194,304 entries",
        )?;
        return Ok(header);
    }
    field_check(
        version == 2 || version == 3,
        "the QCOW image has an unknown version",
    )?;
    header.cluster_log = be32(p, 20);
    field_check(
        (SMALLEST_CLUSTER_LOG..=LARGEST_CLUSTER_LOG).contains(&header.cluster_log),
        "the QCOW cluster size is not a power of two of 512 bytes to 2 MiB",
    )?;
    header.table_log = header.cluster_log - 3;
    header.encrypted = be32(p, 32) != 0;
    header.l1_entries = u64::from(be32(p, 36));
    header.refcount_at = be64(p, 48);
    header.refcount_clusters = be32(p, 56);
    header.l1_at = be64(p, 40);
    header.header_end = V2_HEADER;
    field_check(
        header.l1_entries <= L1_ENTRY_LIMIT,
        "the QCOW first-level table holds more than 4,194,304 entries",
    )?;
    field_check(
        header.l1_entries >= needed_l1_entries(header.size, header.cluster_log, header.table_log),
        "the QCOW first-level table is shorter than its disk needs",
    )?;
    if version == 3 {
        let incompatible = be64(p, 72);
        let length = be32(p, 100);
        field_check(
            length >= V3_HEADER && u64::from(length) < 1 << header.cluster_log,
            "the QCOW header length is malformed",
        )?;
        header.header_end = u64::from(length);
        field_check(
            incompatible & !KNOWN_INCOMPATIBLE == 0,
            "the QCOW image needs features Zipper does not know",
        )?;
        let compression = if length as usize > COMPRESSION_TYPE_AT {
            p[COMPRESSION_TYPE_AT]
        } else {
            ZLIB
        };
        field_check(
            (compression != ZLIB) == (incompatible & COMPRESSION_TYPE != 0),
            "the QCOW compression type and its feature bit disagree",
        )?;
        field_check(
            compression == ZLIB || compression == ZSTD,
            "the QCOW image names an unknown compression type",
        )?;
        header.zstd = compression == ZSTD;
        header.unread = if incompatible & EXTERNAL_DATA != 0 {
            Some("keeps its data in an external file")
        } else if incompatible & EXTENDED_L2 != 0 {
            Some("uses extended second-level entries")
        } else if incompatible & CORRUPT != 0 {
            Some("is marked corrupt")
        } else {
            None
        };
    }
    Ok(header)
}

pub(crate) fn l1_entry(value: u64, v1: bool) -> Field<u64> {
    if v1 {
        field_check(
            value >> FILE_OFFSET_LOG == 0,
            "a QCOW first-level entry lies past 2^56",
        )?;
        return Ok(value);
    }
    field_check(
        value & L1_RESERVED == 0,
        "a QCOW first-level entry sets reserved bits",
    )?;
    Ok(value & OFFSET_MASK)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cluster {
    Zeros,
    Data(u64),
    Compressed { at: u64, length: u64 },
}

pub(crate) fn l2_entry(value: u64, header: &Header) -> Field<Cluster> {
    let cluster_log = header.cluster_log;
    if header.version == 1 {
        if value & V1_COMPRESSED != 0 {
            let shift = 63 - cluster_log;
            return Ok(Cluster::Compressed {
                at: value & ((1 << shift) - 1),
                length: (value >> shift) & ((1 << cluster_log) - 1),
            });
        }
        field_check(
            value >> FILE_OFFSET_LOG == 0,
            "a QCOW cluster lies past 2^56",
        )?;
        return Ok(if value == 0 {
            Cluster::Zeros
        } else {
            Cluster::Data(value)
        });
    }
    if value & COMPRESSED != 0 {
        let shift = 62 - (cluster_log - 8);
        let at = value & ((1 << shift) - 1);
        let sectors = ((value & !(COPIED | COMPRESSED)) >> shift) + 1;
        return Ok(Cluster::Compressed {
            at,
            length: sectors * COMPRESSED_SECTOR - (at & (COMPRESSED_SECTOR - 1)),
        });
    }
    field_check(
        value & L2_RESERVED == 0 && (header.version == 3 || value & ZERO == 0),
        "a QCOW second-level entry sets reserved bits",
    )?;
    let at = value & OFFSET_MASK;
    if value & ZERO != 0 || at == 0 {
        return Ok(Cluster::Zeros);
    }
    Ok(Cluster::Data(at))
}

struct Qcow {
    header: Header,
    length: u64,
    pages: Pages,
    inflated: Option<(u64, Vec<u8>)>,
}

fn other(error: BadField) -> io::Error {
    io::Error::other(error.0)
}

impl Qcow {
    fn entry(&mut self, file: &ArchiveFile, at: u64) -> io::Result<u64> {
        let mut bytes = [0u8; ENTRY as usize];
        self.pages.read(file, self.length, at, &mut bytes)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn cluster(&mut self, file: &ArchiveFile, index: u64) -> io::Result<Cluster> {
        let table = index >> self.header.table_log;
        let l1 = self.entry(file, self.header.l1_at + ENTRY * table)?;
        let l2_at = l1_entry(l1, self.header.version == 1).map_err(other)?;
        if l2_at == 0 {
            return Ok(Cluster::Zeros);
        }
        let slot = index & ((1 << self.header.table_log) - 1);
        let value = self.entry(file, l2_at + ENTRY * slot)?;
        l2_entry(value, &self.header).map_err(other)
    }
}

fn inflate(
    file: &ArchiveFile,
    length: u64,
    at: u64,
    packed: u64,
    cluster: usize,
) -> io::Result<Vec<u8>> {
    let end = at.saturating_add(packed).min(length);
    let mut input = vec![0u8; (end - at) as usize];
    read_file_exact(file, &mut input, at)?;
    cluster_bytes(&input, cluster)
}

fn cluster_bytes(input: &[u8], cluster: usize) -> io::Result<Vec<u8>> {
    let mut output = vec![0u8; cluster];
    let written = crate::framing::inflate_until_full(&mut Vec::new(), input, &mut output)
        .map_err(|_| io::Error::other("a compressed QCOW cluster is damaged"))?;
    if written != Some(cluster) {
        return Err(io::Error::other(
            "a compressed QCOW cluster does not fill its cluster",
        ));
    }
    Ok(output)
}

impl Layer for Qcow {
    fn read(&mut self, file: &ArchiveFile, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let cluster_log = self.header.cluster_log;
        let index = offset >> cluster_log;
        let within = offset & ((1 << cluster_log) - 1);
        let want = (buffer.len() as u64).min((1 << cluster_log) - within) as usize;
        match self.cluster(file, index)? {
            Cluster::Zeros => buffer[..want].fill(0),
            Cluster::Data(at) => read_file_exact(file, &mut buffer[..want], at + within)?,
            Cluster::Compressed { at, length } => {
                if self
                    .inflated
                    .as_ref()
                    .is_none_or(|(held, _)| *held != index)
                {
                    let bytes = inflate(file, self.length, at, length, 1 << cluster_log)?;
                    self.inflated = Some((index, bytes));
                }
                let bytes = &self.inflated.as_ref().unwrap().1;
                buffer[..want].copy_from_slice(&bytes[within as usize..within as usize + want]);
            }
        }
        Ok(want)
    }
}

struct Used {
    cluster_log: u32,
    owned: Claims,
    packed: Claims,
}

impl Used {
    fn own(&mut self, at: u64, bytes: u64, what: &str) -> Result<()> {
        let (first, last) = (at >> self.cluster_log, (at + bytes - 1) >> self.cluster_log);
        for unit in first..=last {
            ensure!(
                !self.packed.holds(unit) && self.owned.claim(unit),
                "two QCOW tables or clusters share a cluster of the file, at the {what}"
            );
        }
        Ok(())
    }

    fn pack(&mut self, at: u64, bytes: u64) -> Result<()> {
        let (first, last) = (at >> self.cluster_log, (at + bytes - 1) >> self.cluster_log);
        for unit in first..=last {
            ensure!(
                !self.owned.holds(unit),
                "a compressed QCOW cluster and a table or cluster share a cluster of the file"
            );
            self.packed.claim(unit);
        }
        Ok(())
    }
}

fn check_unused_l1_entries(file: &ArchiveFile, length: u64, header: &Header) -> Result<()> {
    let needed = needed_l1_entries(header.size, header.cluster_log, header.table_log);
    let cluster = 1u64 << header.cluster_log;
    ensure!(
        header
            .l1_at
            .checked_add(header.l1_entries * ENTRY)
            .is_some_and(|end| end <= length),
        "the QCOW first-level table lies past the end of the image"
    );
    let mut batch = vec![0u8; TABLE_BATCH];
    let mut read = needed;
    while read < header.l1_entries {
        let count = (header.l1_entries - read).min(TABLE_BATCH as u64 / ENTRY);
        let bytes = &mut batch[..(count * ENTRY) as usize];
        read_file_exact(file, bytes, header.l1_at + read * ENTRY)
            .context("cannot read the QCOW first-level table")?;
        for value in bytes.chunks_exact(ENTRY as usize) {
            let l2_at = l1_entry(be64(value, 0), false).map_err(anyhow::Error::from)?;
            ensure!(
                l2_at & (cluster - 1) == 0
                    && l2_at
                        .checked_add(header.table_bytes())
                        .is_some_and(|end| end <= length),
                "a QCOW first-level entry past the disk names a table outside the image"
            );
        }
        read += count;
    }
    Ok(())
}

fn check_tables(file: &ArchiveFile, length: u64, header: &Header) -> Result<()> {
    let cluster_log = header.cluster_log;
    let cluster = 1u64 << cluster_log;
    let disk_clusters = (header.size >> cluster_log) + u64::from(header.size & (cluster - 1) != 0);
    let per_table = 1u64 << header.table_log;
    let mut used = Used {
        cluster_log,
        owned: Claims::default(),
        packed: Claims::default(),
    };
    used.own(0, header.header_end, "header")?;
    if header.version != 1 {
        check_unused_l1_entries(file, length, header)?;
        ensure!(
            header
                .refcount_at
                .checked_add(u64::from(header.refcount_clusters) << cluster_log)
                .is_some_and(|end| end <= length),
            "the QCOW reference count table lies past the end of the image"
        );
    }
    let needed = needed_l1_entries(header.size, cluster_log, header.table_log);
    if needed == 0 {
        return Ok(());
    }
    ensure!(
        header.version == 1 || header.l1_at & (cluster - 1) == 0,
        "the QCOW first-level table is not aligned to a cluster"
    );
    ensure!(
        header.l1_at >= header.header_end
            && header
                .l1_at
                .checked_add(needed * ENTRY)
                .is_some_and(|end| end <= length),
        "the QCOW first-level table lies outside the image"
    );
    let header_clusters_end = (header.header_end - 1) >> cluster_log;
    let l1_from = if header.l1_at >> cluster_log == header_clusters_end {
        (header_clusters_end + 1) << cluster_log
    } else {
        header.l1_at
    };
    let l1_end = header.l1_at + needed * ENTRY;
    if l1_end > l1_from {
        used.own(l1_from, l1_end - l1_from, "first-level table")?;
    }
    let mut l1_batch = vec![0u8; TABLE_BATCH];
    let mut table = vec![0u8; header.table_bytes() as usize];
    let mut read = 0u64;
    while read < needed {
        let count = (needed - read).min(TABLE_BATCH as u64 / ENTRY);
        let bytes = &mut l1_batch[..(count * ENTRY) as usize];
        read_file_exact(file, bytes, header.l1_at + read * ENTRY)
            .context("cannot read the QCOW first-level table")?;
        for (place, value) in bytes.chunks_exact(ENTRY as usize).enumerate() {
            let first = (read + place as u64) * per_table;
            let l2_at =
                l1_entry(be64(value, 0), header.version == 1).map_err(anyhow::Error::from)?;
            if l2_at == 0 {
                continue;
            }
            ensure!(
                header.version == 1 || l2_at & (cluster - 1) == 0,
                "a QCOW second-level table is not aligned to a cluster"
            );
            ensure!(
                l2_at
                    .checked_add(header.table_bytes())
                    .is_some_and(|end| end <= length),
                "a QCOW second-level table lies past the end of the image"
            );
            used.own(l2_at, header.table_bytes(), "second-level table")?;
            read_file_exact(file, &mut table, l2_at)
                .context("cannot read a QCOW second-level table")?;
            for (slot, value) in table.chunks_exact(ENTRY as usize).enumerate() {
                let index = first + slot as u64;
                match l2_entry(be64(value, 0), header).map_err(anyhow::Error::from)? {
                    Cluster::Zeros => {}
                    Cluster::Data(at) => {
                        ensure!(
                            at & (cluster - 1) == 0,
                            "a QCOW cluster is not aligned to a cluster of the file"
                        );
                        let serves = if index < disk_clusters {
                            cluster.min(header.size - index * cluster)
                        } else {
                            cluster
                        };
                        ensure!(
                            at.checked_add(serves).is_some_and(|end| end <= length),
                            "a QCOW cluster lies past the end of the image"
                        );
                        used.own(at, serves, "data")?;
                    }
                    Cluster::Compressed { at, length: packed } => {
                        if header.version == 1 {
                            bail!(
                                "the QCOW version 1 image compresses its clusters, which 7-Zip does not read either"
                            );
                        }
                        if header.zstd {
                            bail!(
                                "the QCOW image compresses its clusters with zstd, which 7-Zip does not read either"
                            );
                        }
                        ensure!(
                            at >= header.header_end
                                && at.checked_add(packed).is_some_and(|end| end <= length),
                            "a compressed QCOW cluster lies past the end of the image"
                        );
                        used.pack(at, packed)?;
                    }
                }
            }
        }
        read += count;
    }
    Ok(())
}

pub(crate) fn open(file: ArchiveFile, length: u64) -> Result<Opened> {
    let bytes = read_at(
        &file,
        length,
        0,
        HEADER_READ.min(length as usize),
        "QCOW header",
    )?;
    let mut head = [0u8; HEADER_READ];
    head[..bytes.len()].copy_from_slice(&bytes);
    let header = header(&head).map_err(anyhow::Error::from)?;
    ensure!(
        header.header_end <= length,
        "the image ends inside its QCOW header"
    );
    if header.backing {
        return Ok(Opened::Elsewhere(
            "the image is a QCOW image with a backing file, which it needs".to_owned(),
        ));
    }
    ensure!(
        !header.encrypted,
        "the QCOW image is encrypted, which 7-Zip does not read either"
    );
    if let Some(reason) = header.unread {
        bail!("the QCOW image {reason}, which 7-Zip does not read either");
    }
    check_tables(&file, length, &header)?;
    let layer = Qcow {
        header,
        length,
        pages: Pages::default(),
        inflated: None,
    };
    Ok(Opened::Disk(Disk::layered(
        file,
        header.size,
        Box::new(layer),
    )))
}

#[cfg(test)]
mod cluster_tests {
    use super::*;

    fn flate2_cluster(input: &[u8], cluster: usize) -> Option<Vec<u8>> {
        let mut output = vec![0u8; cluster];
        let mut inflater = flate2::Decompress::new(false);
        let finish = flate2::FlushDecompress::Finish;
        let status = inflater.decompress(input, &mut output, finish);
        (status.is_ok() && inflater.total_out() == cluster as u64).then_some(output)
    }

    #[test]
    fn clusters_decode_as_flate2_decodes_them_whole_cut_and_flipped() {
        let cases = crate::framing::oracle::block_cases(crate::framing::oracle::raw);
        for (input, cluster) in cases.into_iter().filter(|(_, cluster)| *cluster >= 1) {
            let ours = cluster_bytes(&input, cluster).ok();
            assert_eq!(ours, flate2_cluster(&input, cluster), "{cluster}");
        }
    }

    #[test]
    fn a_cluster_damaged_after_it_fills_reads_as_qemu_and_flate2_read_it() {
        let cluster = 1 << 16;
        let data = crate::framing::oracle::sample(31, cluster);
        for level in [1, 6, 9] {
            let stream = crate::framing::oracle::raw(&data, level);
            let tail = [stream.clone(), vec![0x5a; 512]].concat();
            let mut cases = vec![stream.clone(), tail];
            for at in stream.len().saturating_sub(4)..stream.len() {
                for bit in 0..8 {
                    let mut flipped = stream.clone();
                    flipped[at] ^= 1 << bit;
                    cases.push(flipped);
                }
            }
            cases.push(stream[..stream.len() - 1].to_vec());
            for (index, input) in cases.iter().enumerate() {
                let ours = cluster_bytes(input, cluster).ok();
                assert_eq!(
                    ours,
                    flate2_cluster(input, cluster),
                    "level {level} case {index}"
                );
            }
            assert_eq!(cluster_bytes(&cases[1], cluster).unwrap(), data);
            assert!(cluster_bytes(&stream, cluster + 1).is_err());
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_never_panic_and_keep_their_tables_inside_their_limits() {
        let bytes: [u8; HEADER_READ] = kani::any();
        if let Ok(found) = header(&bytes) {
            assert!(found.size >> LARGEST_DISK_LOG == 0);
            assert!((SMALLEST_CLUSTER_LOG..=LARGEST_CLUSTER_LOG).contains(&found.cluster_log));
            assert!(found.table_log <= found.cluster_log);
            assert!(found.header_end < 1 << found.cluster_log || found.version == 1);
            let needed = needed_l1_entries(found.size, found.cluster_log, found.table_log);
            assert!(found.version == 1 || needed <= found.l1_entries);
            assert!(found.l1_entries <= L1_ENTRY_LIMIT);
            assert!((needed << (found.cluster_log + found.table_log)) >= found.size);
        }
    }

    #[kani::proof]
    fn table_entries_never_panic_and_name_clusters_inside_their_bits() {
        let value: u64 = kani::any();
        let version: u32 = kani::any();
        kani::assume((1..=3).contains(&version));
        let cluster_log: u32 = kani::any();
        kani::assume((SMALLEST_CLUSTER_LOG..=LARGEST_CLUSTER_LOG).contains(&cluster_log));
        let header = Header {
            version,
            cluster_log,
            table_log: cluster_log - 3,
            size: 0,
            l1_at: 0,
            l1_entries: 0,
            header_end: 0,
            backing: false,
            encrypted: false,
            zstd: false,
            unread: None,
            refcount_at: 0,
            refcount_clusters: 0,
        };
        if let Ok(at) = l1_entry(value, version == 1) {
            assert!(at >> FILE_OFFSET_LOG == 0);
            assert!(version == 1 || at & 0x1FF == 0);
        }
        match l2_entry(value, &header) {
            Ok(Cluster::Data(at)) => {
                assert!(at != 0 && at >> FILE_OFFSET_LOG == 0);
                assert!(version == 1 || at & 0x1FF == 0);
            }
            Ok(Cluster::Compressed { at, length }) => {
                assert!(at >> 62 == 0);
                assert!(version == 1 || length >= 1);
                assert!(version == 1 || length <= 1 << (cluster_log + 1));
            }
            _ => {}
        }
    }
}
