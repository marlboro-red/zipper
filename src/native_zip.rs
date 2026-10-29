//! Opt-in ZIP extraction for regular files: stored/Deflate, bounded streaming,
//! independent per-worker readers, and publication only after full verification.
use crate::{Entry, parent, safe_path};
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};
use zip::{CompressionMethod, ZipArchive};

struct Item {
    entry: Entry,
    crc: u32,
    compressed: u64,
    method: CompressionMethod,
}

const MAX_METADATA: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: u64 = 100_000;

// This accounts only for the two whole-entry buffers, not process RSS.
struct BufferBudget(AtomicUsize);

impl BufferBudget {
    fn reserve(&self, bytes: usize) -> Option<BufferReservation<'_>> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |available| {
                available.checked_sub(bytes)
            })
            .ok()
            .map(|_| BufferReservation {
                budget: self,
                bytes,
            })
    }
}

struct BufferReservation<'a> {
    budget: &'a BufferBudget,
    bytes: usize,
}

impl Drop for BufferReservation<'_> {
    fn drop(&mut self) {
        self.budget.0.fetch_add(self.bytes, Ordering::Relaxed);
    }
}

fn entry_buffer(size: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(size)
        .context("cannot allocate ZIP entry buffer")?;
    buffer.resize(size, 0);
    Ok(buffer)
}

// Bound directory allocation before the general ZIP parser sees untrusted
// counts/lengths. The opt-in path accepts conventional single-disk ZIP/ZIP64
// records with no prepended executable, trailing data, or ZIP64 extension sector.
fn preflight(file: &mut BufReader<File>) -> Result<u64> {
    let length = file.get_ref().metadata()?.len();
    ensure!(length >= 22, "truncated ZIP");
    let tail_size = length.min(22 + 65535) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0; tail_size];
    file.read_exact(&mut tail)?;
    let end = (0..=tail.len() - 22)
        .rev()
        .find(|&i| {
            tail[i..i + 4] == *b"PK\x05\x06"
                && i + 22 + u16::from_le_bytes(tail[i + 20..i + 22].try_into().unwrap()) as usize
                    == tail.len()
        })
        .context("ZIP end record not found")?;
    let eocd = length - tail_size as u64 + end as u64;
    // zip-rs tolerates trailing data and may select a record embedded in the
    // comment. Restrict this opt-in reader to one unambiguous final record.
    ensure!(
        !tail[end + 4..]
            .windows(4)
            .any(|bytes| bytes == b"PK\x05\x06"),
        "ambiguous ZIP end records require the compatibility backend"
    );
    let read16 = |at| u16::from_le_bytes(tail[end + at..end + at + 2].try_into().unwrap());
    let read32 = |at| u32::from_le_bytes(tail[end + at..end + at + 4].try_into().unwrap());
    ensure!(
        read16(4) == 0 && read16(6) == 0 && read16(8) == read16(10),
        "multi-disk ZIP requires backend"
    );
    let mut count = read16(10) as u64;
    let mut bytes = read32(12) as u64;
    let mut start = read32(16) as u64;
    let mut directory_end = eocd;
    if count == u16::MAX as u64 || bytes == u32::MAX as u64 || start == u32::MAX as u64 {
        ensure!(eocd >= 20, "missing ZIP64 locator");
        file.seek(SeekFrom::Start(eocd - 20))?;
        let locator = crate::number::<20>(file)?;
        ensure!(
            &locator[..4] == b"PK\x06\x07"
                && locator[4..8] == [0; 4]
                && u32::from_le_bytes(locator[16..20].try_into()?) == 1,
            "invalid ZIP64 locator"
        );
        directory_end = u64::from_le_bytes(locator[8..16].try_into()?);
        ensure!(
            directory_end.checked_add(56) == Some(eocd - 20),
            "unsupported ZIP64 extension or offset"
        );
        file.seek(SeekFrom::Start(directory_end))?;
        let record = crate::number::<56>(file)?;
        ensure!(
            &record[..4] == b"PK\x06\x06"
                && u64::from_le_bytes(record[4..12].try_into()?) == 44
                && record[16..24] == [0; 8],
            "unsupported ZIP64 record"
        );
        count = u64::from_le_bytes(record[32..40].try_into()?);
        ensure!(
            u64::from_le_bytes(record[24..32].try_into()?) == count,
            "multi-disk ZIP64 requires backend"
        );
        bytes = u64::from_le_bytes(record[40..48].try_into()?);
        start = u64::from_le_bytes(record[48..56].try_into()?);
    }
    ensure!(
        count <= MAX_ENTRIES && bytes <= MAX_METADATA,
        "ZIP metadata limit exceeded"
    );
    ensure!(
        start.checked_add(bytes) == Some(directory_end),
        "unsupported ZIP central-directory extent"
    );
    // Validate the complete raw record chain, including comments and extras.
    file.seek(SeekFrom::Start(start))?;
    let mut position = start;
    for _ in 0..count {
        ensure!(
            directory_end - position >= 46,
            "truncated ZIP central entry"
        );
        let header = crate::number::<46>(file)?;
        ensure!(&header[..4] == b"PK\x01\x02", "invalid ZIP central entry");
        let variable: u64 = [28, 30, 32]
            .iter()
            .map(|&at| u16::from_le_bytes(header[at..at + 2].try_into().unwrap()) as u64)
            .sum();
        position += 46 + variable;
        ensure!(
            position <= directory_end,
            "ZIP central entry exceeds directory"
        );
        file.seek_relative(variable as i64)?;
    }
    ensure!(position == directory_end, "ZIP directory count mismatch");
    file.seek(SeekFrom::Start(0))?;
    Ok(count)
}

fn open(path: &Path, cache: bool) -> Result<ZipArchive<crate::zip_reader::Reader>> {
    let mut file = BufReader::with_capacity(64 * 1024, File::open(path)?);
    let count = preflight(&mut file)?;
    let reader = if cache {
        crate::zip_reader::Reader::cached(file.into_inner(), 8 * 1024 * 1024)?
    } else {
        crate::zip_reader::Reader::new(file.into_inner())?
    };
    let archive = ZipArchive::new(reader)?;
    ensure!(
        archive.len() as u64 == count && archive.offset() == 0,
        "ZIP parser disagrees with validated directory"
    );
    Ok(archive)
}

fn scan(archive: &ZipArchive<crate::zip_reader::Reader>) -> Result<Vec<Item>> {
    ensure!(archive.len() <= 1_000_000, "ZIP entry limit exceeded");
    ensure!(
        !archive.metadata().has_duplicate_names(),
        "duplicate ZIP path"
    );
    let mut items = Vec::with_capacity(archive.len());
    let mut names = HashMap::new();
    let mut metadata_bytes = 0usize;
    for index in 0..archive.len() {
        let file = archive.by_index_data(index)?;
        ensure!(
            !file.encrypted(),
            "encrypted ZIP requires the compatibility backend"
        );
        ensure!(
            !file.is_symlink(),
            "ZIP symlinks require the compatibility backend"
        );
        let directory = file.is_dir();
        if let Some(mode) = file.unix_mode() {
            let kind = mode & 0o170000;
            ensure!(
                kind == 0 || kind == if directory { 0o040000 } else { 0o100000 },
                "unsupported ZIP file type"
            );
        }
        ensure!(
            matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ),
            "ZIP compression method requires the compatibility backend"
        );
        let name = file.name()?.into_owned();
        let name = if directory {
            name.strip_suffix('/').unwrap_or(&name).to_owned()
        } else {
            name
        };
        safe_path(&name)?;
        ensure!(
            names.insert(name.clone(), directory).is_none(),
            "duplicate ZIP path"
        );
        metadata_bytes += name.len() + 64;
        ensure!(
            metadata_bytes <= 64 * 1024 * 1024,
            "ZIP metadata limit exceeded"
        );
        ensure!(!directory || file.size() == 0, "ZIP directory has payload");
        items.push(Item {
            entry: Entry {
                name,
                size: file.size(),
                directory,
            },
            crc: file.crc32(),
            compressed: file.compressed_size(),
            method: file.compression(),
        });
    }
    for item in &items {
        let mut prefix = item.entry.name.as_str();
        while let Some((p, _)) = prefix.rsplit_once('/') {
            ensure!(names.get(p) != Some(&false), "ZIP file used as directory");
            prefix = p;
        }
    }
    Ok(items)
}

/// List validated central-directory entries; payload CRCs are not checked.
pub fn list(path: &Path) -> Result<Vec<Entry>> {
    Ok(scan(&open(path, false)?)?
        .into_iter()
        .map(|item| item.entry)
        .collect())
}

/// Verify and optionally extract a stored/Deflate ZIP. This opt-in engine keeps
/// file bytes and directories, not permissions, timestamps, or extended metadata.
/// The archive and destination parent must not be modified concurrently.
pub fn read_archive(path: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    read_archive_with_options(path, destination, ReadOptions::default())
}

/// Whole-buffer Deflate fast path for entries whose compressed and decoded sizes
/// fit the shared entry-buffer budget; other entries use bounded streaming.
/// Integrity-only reads and Linux extraction also cache archives up to 8 MiB.
pub fn read_archive_fast(path: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    read_archive_with_options(
        path,
        destination,
        ReadOptions {
            fast: true,
            threads: None,
            buffer_bytes: None,
        },
    )
}

/// Native ZIP decoding and concurrency options.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions {
    /// Use the bounded whole-buffer libdeflate path for eligible entries.
    pub fast: bool,
    /// Override automatic concurrency with a maximum of 1–64 workers.
    /// The caller's Rayon pool remains an additional upper bound.
    pub threads: Option<usize>,
    /// Shared compressed + decoded entry-buffer budget. Default: 128 MiB on
    /// ARM64 macOS, 64 MiB elsewhere.
    /// Zero forces streaming. Metadata, worker state and archive cache are extra.
    pub buffer_bytes: Option<usize>,
}

pub fn read_archive_with_options(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    ensure!(
        options.threads.is_none_or(|n| (1..=64).contains(&n)),
        "ZIP threads must be 1..64"
    );
    let fast = options.fast;
    let budget = BufferBudget(AtomicUsize::new(options.buffer_bytes.unwrap_or(
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            128 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        },
    )));
    // Linux extraction benefits from avoiding shared input seeks/reads for tiny
    // archives. Other platforms retain caching only for integrity verification.
    // This retained-input cache is separate from the live entry-buffer budget.
    let archive = open(
        path,
        fast && (destination.is_none() || cfg!(target_os = "linux")),
    )?;
    let items = scan(&archive)?;
    let staging = if let Some(dst) = destination {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "extraction destination must not exist"
        );
        Some(tempfile::tempdir_in(parent(dst))?)
    } else {
        None
    };
    if let Some(temp) = &staging {
        let mut directories = HashSet::new();
        for item in &items {
            let path = temp.path().join(&item.entry.name);
            directories.insert(if item.entry.directory {
                path
            } else {
                parent(&path).to_owned()
            });
        }
        for directory in directories {
            fs::create_dir_all(directory)?;
        }
    }
    // ZIP entries are independent outputs. Use descending-size order and work
    // stealing instead of fixed file lanes; Rayon does not promise start order.
    let mut work: Vec<_> = (0..items.len()).collect();
    work.sort_unstable_by_key(|&index| std::cmp::Reverse(items[index].entry.size));
    let run = || {
        work.par_iter().try_for_each_init(
            || {
                (
                    archive.clone(),
                    vec![0u8; 256 * 1024],
                    fast.then(libdeflater::Decompressor::new),
                )
            },
            |(archive, buffer, codec), &index| {
                let item = &items[index];
                // Never wait for memory while holding worker resources. If a
                // reservation cannot fit now, this entry uses streaming instead.
                // Declare the lease before buffers so it outlives them on errors.
                let reservation = if fast && item.method == CompressionMethod::Deflated {
                    item.entry
                        .size
                        .checked_add(item.compressed)
                        .and_then(|bytes| usize::try_from(bytes).ok())
                        .and_then(|bytes| budget.reserve(bytes))
                } else {
                    None
                };
                let buffered = reservation.is_some();
                let mut source = if buffered {
                    archive.by_index_raw(index)?
                } else {
                    archive.by_index(index)?
                };
                ensure!(
                    source.size() == item.entry.size
                        && source.crc32() == item.crc
                        && source.compressed_size() == item.compressed
                        && source.compression() == item.method,
                    "ZIP changed during extraction"
                );
                let mut output = if let Some(temp) = &staging {
                    if !item.entry.directory {
                        Some(BufWriter::new(
                            File::options()
                                .write(true)
                                .create_new(true)
                                .open(temp.path().join(&item.entry.name))?,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if buffered {
                    // Allocate exact entry lengths while holding the shared lease.
                    // Reusing independently grown buffers would violate accounting.
                    let mut compressed = entry_buffer(item.compressed as usize)?;
                    let mut decoded = entry_buffer(item.entry.size as usize)?;
                    source.read_exact(&mut compressed)?;
                    let length = codec
                        .as_mut()
                        .unwrap()
                        .deflate_decompress(&compressed, &mut decoded)?;
                    ensure!(
                        length as u64 == item.entry.size,
                        "ZIP decoded size mismatch"
                    );
                    ensure!(crc32fast::hash(&decoded) == item.crc, "ZIP CRC mismatch");
                    if let Some(output) = &mut output {
                        output.write_all(&decoded)?;
                        output.flush()?;
                    }
                    return Ok(());
                }
                let mut written = 0u64;
                loop {
                    let len = source.read(buffer)?;
                    if len == 0 {
                        break;
                    }
                    written = written
                        .checked_add(len as u64)
                        .context("ZIP size overflow")?;
                    ensure!(
                        written <= item.entry.size,
                        "ZIP decoded size exceeds metadata"
                    );
                    if let Some(output) = &mut output {
                        output.write_all(&buffer[..len])?;
                    }
                }
                ensure!(written == item.entry.size, "ZIP decoded size mismatch");
                if let Some(output) = &mut output {
                    output.flush()?;
                }
                Ok(())
            },
        )
    };
    // On macOS, tiny-file creation saturates before all CPU workers are useful:
    // extra concurrent creates increase filesystem contention. Keep this policy
    // limited to measured tiny-file workloads; larger entries retain parallel
    // codec scaling. The caller's pool remains an upper bound.
    let automatic_limit = if cfg!(target_os = "macos")
        && staging.is_some()
        && items.iter().filter(|item| !item.entry.directory).count() >= 1024
        && items.iter().all(|item| item.entry.size <= 16 * 1024)
    {
        4
    } else {
        64
    };
    let worker_limit = options.threads.unwrap_or(automatic_limit);
    if rayon::current_num_threads() <= worker_limit {
        run()?;
    } else {
        // Also bound library callers whose Rayon pool exceeds the CLI maximum.
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_limit)
            .build()?
            .install(run)?;
    }
    if let (Some(temp), Some(dst)) = (staging, destination) {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "destination appeared during extraction"
        );
        fs::rename(temp.path(), dst)?;
    }
    Ok(items.into_iter().map(|item| item.entry).collect())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn concurrent_reservations_never_exceed_budget_and_release_on_unwind() {
        let budget = BufferBudget(AtomicUsize::new(8));
        let held = Barrier::new(16);
        let release = Barrier::new(16);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let budget = &budget;
                let held = &held;
                let release = &release;
                scope.spawn(move || {
                    let reservation = budget.reserve(1);
                    held.wait();
                    // Exactly eight leases must be live; none can release early.
                    assert_eq!(budget.0.load(Ordering::Relaxed), 0);
                    assert!(budget.reserve(1).is_none());
                    release.wait();
                    drop(reservation);
                });
            }
        });
        assert_eq!(budget.0.load(Ordering::Relaxed), 8);
        assert!(
            std::panic::catch_unwind(|| {
                let _reservation = budget.reserve(8).unwrap();
                panic!("simulated failed decode");
            })
            .is_err()
        );
        assert_eq!(budget.0.load(Ordering::Relaxed), 8);
        assert!(entry_buffer(usize::MAX).is_err());
        assert!(budget.reserve(usize::MAX).is_none());
        assert!(budget.reserve(8).is_some());
    }
}
