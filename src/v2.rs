use crate::{Entry, MAGIC, number, parent, pipeline, safe_path};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::mpsc::sync_channel,
};

const CHUNK: usize = 1024 * 1024;
const HEADER: u64 = 16;
const FOOTER: u64 = 40;
const END: &[u8; 8] = b"ZIPIDX02";
const MAX_INDEX: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;

#[derive(Clone)]
struct Item {
    entry: Entry,
    offset: u64,
}
#[derive(Clone, Copy)]
struct Block {
    raw: u32,
    stored: u32,
    codec: u8,
    checksum: u32,
}
struct Index {
    items: Vec<Item>,
    blocks: Vec<Block>,
    chunk: usize,
}
struct Packet {
    block: Block,
    data: Vec<u8>,
    offset: u64,
}

fn workers(blocks: usize) -> usize {
    rayon::current_num_threads().min(64).min(blocks.max(1))
}
fn checked_add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).context("archive length overflow")
}
fn validate_names(items: &[Item]) -> Result<()> {
    let mut names = HashMap::new();
    for item in items {
        safe_path(&item.entry.name)?;
        ensure!(
            names
                .insert(item.entry.name.as_str(), item.entry.directory)
                .is_none(),
            "duplicate archive path"
        );
    }
    for item in items {
        let mut prefix = item.entry.name.as_str();
        while let Some((p, _)) = prefix.rsplit_once('/') {
            ensure!(names.get(p) != Some(&false), "file used as directory: {p}");
            prefix = p;
        }
    }
    Ok(())
}

fn scan(output: &Path, inputs: &[PathBuf], chunk: usize) -> Result<(Vec<Item>, Vec<PathBuf>, u64)> {
    ensure!(!inputs.is_empty(), "at least one input is required");
    ensure!(
        fs::symlink_metadata(output).is_err(),
        "output already exists"
    );
    let output_abs = parent(output)
        .canonicalize()?
        .join(output.file_name().context("invalid output")?);
    let mut items = Vec::new();
    let mut paths = Vec::new();
    let mut total = 0;
    let mut metadata_bytes = 8usize;
    for input in inputs {
        ensure!(
            !fs::symlink_metadata(input)?.file_type().is_symlink(),
            "symlinks are not supported"
        );
        let input = input.canonicalize()?;
        ensure!(
            !output_abs.starts_with(&input),
            "archive must be outside its input tree"
        );
        let base = input.parent().context("cannot archive filesystem root")?;
        for entry in walkdir::WalkDir::new(&input).sort_by_file_name() {
            let entry = entry?;
            let directory = entry.file_type().is_dir();
            ensure!(
                directory || entry.file_type().is_file(),
                "unsupported file type: {}",
                entry.path().display()
            );
            let name = entry
                .path()
                .strip_prefix(base)?
                .to_str()
                .context("paths must be UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            safe_path(&name)?;
            metadata_bytes += 21 + name.len();
            ensure!(
                metadata_bytes <= MAX_INDEX && items.len() < MAX_ENTRIES,
                "archive metadata limit exceeded"
            );
            let size = if directory {
                0
            } else {
                entry.metadata()?.len()
            };
            items.push(Item {
                entry: Entry {
                    name,
                    size,
                    directory,
                },
                offset: total,
            });
            paths.push(entry.path().to_owned());
            total = checked_add(total, size)?;
        }
    }
    let block_count = total.div_ceil(chunk as u64);
    ensure!(
        block_count <= ((MAX_INDEX - metadata_bytes) / 13) as u64,
        "archive index limit exceeded"
    );
    validate_names(&items)?;
    Ok((items, paths, total))
}

pub(super) fn create(
    output: &Path,
    inputs: &[PathBuf],
    options: crate::CreateOptions,
) -> Result<()> {
    let crate::CreateOptions {
        level,
        block_size: chunk,
    } = options;
    ensure!(
        (-7..=22).contains(&level),
        "level must be between -7 and 22"
    );
    ensure!(
        (64 * 1024..=16 * CHUNK).contains(&chunk) && chunk.is_power_of_two(),
        "invalid block size"
    );
    let (items, paths, total) = scan(output, inputs, chunk)?;
    let count = workers(total.div_ceil(chunk as u64) as usize);
    let (recycle, recycled) = sync_channel::<Vec<u8>>(count * 2);
    let mut temp = tempfile::NamedTempFile::new_in(parent(output))?;
    let mut blocks = Vec::new();
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        writer.write_all(MAGIC)?;
        writer.write_all(&(chunk as u32).to_le_bytes())?;
        writer.write_all(&0u32.to_le_bytes())?;
        let mut physical = HEADER;
        let source_items = &items;
        pipeline::run(
            count,
            move |emit| {
                let buffer = || {
                    let mut bytes = recycled.try_recv().unwrap_or_default();
                    bytes.resize(chunk, 0);
                    bytes
                };
                let mut data = buffer();
                let mut used = 0;
                for (item, path) in source_items.iter().zip(&paths) {
                    if item.entry.directory {
                        continue;
                    }
                    let mut file = File::open(path)?;
                    ensure!(
                        file.metadata()?.len() == item.entry.size,
                        "input size changed: {}",
                        path.display()
                    );
                    let mut remaining = item.entry.size;
                    while remaining > 0 {
                        let take = remaining.min((chunk - used) as u64) as usize;
                        file.read_exact(&mut data[used..used + take])?;
                        used += take;
                        remaining -= take as u64;
                        if used == chunk {
                            emit(data)?;
                            data = buffer();
                            used = 0;
                        }
                    }
                    ensure!(
                        file.read(&mut [0])? == 0,
                        "input grew while archiving: {}",
                        path.display()
                    );
                }
                if used > 0 {
                    data.truncate(used);
                    emit(data)?;
                }
                Ok(())
            },
            || {
                Ok((
                    zstd::bulk::Compressor::new(level)?,
                    Vec::with_capacity(zstd::zstd_safe::compress_bound(chunk)),
                ))
            },
            |(codec, scratch), mut data: Vec<u8>| {
                let raw = data.len() as u32;
                let checksum = crc32fast::hash(&data);
                scratch.clear();
                scratch.reserve(zstd::zstd_safe::compress_bound(data.len()));
                codec.compress_to_buffer(&data, scratch)?;
                let compressed = scratch.len() < data.len();
                if compressed {
                    std::mem::swap(&mut data, scratch);
                }
                Ok(Packet {
                    block: Block {
                        raw,
                        stored: data.len() as u32,
                        codec: u8::from(compressed),
                        checksum,
                    },
                    data,
                    offset: 0,
                })
            },
            |packet| {
                writer.write_all(&packet.data)?;
                physical = checked_add(physical, packet.data.len() as u64)?;
                blocks.push(packet.block);
                let _ = recycle.try_send(packet.data);
                Ok(())
            },
        )?;
        ensure!(
            blocks.iter().map(|b| b.raw as u64).sum::<u64>() == total,
            "incomplete compression pipeline"
        );
        let index = encode_index(&items, &blocks)?;
        let compressed = zstd::bulk::compress(&index, 3)?;
        writer.write_all(&compressed)?;
        writer.write_all(&physical.to_le_bytes())?;
        writer.write_all(&(compressed.len() as u64).to_le_bytes())?;
        writer.write_all(&(index.len() as u64).to_le_bytes())?;
        writer.write_all(&crc32fast::hash(&index).to_le_bytes())?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(END)?;
        writer.flush()?;
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)?;
    Ok(())
}

fn encode_index(items: &[Item], blocks: &[Block]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for item in items {
        bytes.push(if item.entry.directory { 1 } else { 2 });
        bytes.extend_from_slice(&(item.entry.name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(item.entry.name.as_bytes());
        bytes.extend_from_slice(&item.entry.size.to_le_bytes());
        bytes.extend_from_slice(&item.offset.to_le_bytes());
    }
    bytes.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for b in blocks {
        bytes.extend_from_slice(&b.raw.to_le_bytes());
        bytes.extend_from_slice(&b.stored.to_le_bytes());
        bytes.push(b.codec);
        bytes.extend_from_slice(&b.checksum.to_le_bytes());
    }
    ensure!(bytes.len() <= MAX_INDEX, "archive index limit exceeded");
    Ok(bytes)
}

fn read_index(file: &mut File) -> Result<Index> {
    file.seek(SeekFrom::Start(0))?;
    ensure!(&number::<8>(file)? == MAGIC, "invalid v2 archive magic");
    let chunk = u32::from_le_bytes(number(file)?) as usize;
    ensure!(
        (64 * 1024..=16 * CHUNK).contains(&chunk) && chunk.is_power_of_two(),
        "invalid block size"
    );
    ensure!(
        u32::from_le_bytes(number(file)?) == 0,
        "unsupported header flags"
    );
    let length = file.metadata()?.len();
    ensure!(length >= HEADER + FOOTER, "truncated archive");
    file.seek(SeekFrom::Start(length - FOOTER))?;
    let start = u64::from_le_bytes(number(file)?);
    let stored = u64::from_le_bytes(number(file)?);
    let raw = u64::from_le_bytes(number(file)?);
    let checksum = u32::from_le_bytes(number(file)?);
    ensure!(
        u32::from_le_bytes(number(file)?) == 0,
        "unsupported index flags"
    );
    ensure!(&number::<8>(file)? == END, "invalid archive footer");
    ensure!(
        start >= HEADER && raw >= 8 && raw <= MAX_INDEX as u64,
        "invalid index size"
    );
    ensure!(
        stored > 0 && stored <= zstd::zstd_safe::compress_bound(MAX_INDEX) as u64,
        "invalid stored index size"
    );
    ensure!(
        checked_add(start, stored)? == length - FOOTER,
        "invalid index extent"
    );
    file.seek(SeekFrom::Start(start))?;
    let mut compressed = vec![0; stored as usize];
    file.read_exact(&mut compressed)?;
    let bytes = zstd::bulk::decompress(&compressed, raw as usize)?;
    ensure!(
        bytes.len() as u64 == raw && crc32fast::hash(&bytes) == checksum,
        "index checksum/length mismatch"
    );
    let mut r = Cursor::new(bytes.as_slice());
    let n = u32::from_le_bytes(number(&mut r)?) as usize;
    ensure!(
        n <= MAX_ENTRIES && n <= bytes.len() / 22,
        "invalid entry count"
    );
    let mut items = Vec::new();
    let mut logical = 0;
    for _ in 0..n {
        let kind = number::<1>(&mut r)?[0];
        ensure!(kind == 1 || kind == 2, "invalid entry type");
        let len = u32::from_le_bytes(number(&mut r)?) as usize;
        ensure!((1..=4096).contains(&len), "invalid path length");
        let mut name = vec![0; len];
        r.read_exact(&mut name)?;
        let name = String::from_utf8(name)?;
        let size = u64::from_le_bytes(number(&mut r)?);
        let offset = u64::from_le_bytes(number(&mut r)?);
        ensure!(offset == logical, "noncontiguous file extents");
        ensure!(kind == 2 || size == 0, "directory has payload");
        logical = checked_add(logical, size)?;
        items.push(Item {
            entry: Entry {
                name,
                size,
                directory: kind == 1,
            },
            offset,
        });
    }
    validate_names(&items)?;
    let n = u32::from_le_bytes(number(&mut r)?) as usize;
    ensure!(
        n as u64 == logical.div_ceil(chunk as u64),
        "block count disagrees with file extents"
    );
    ensure!(
        n <= (bytes.len() - r.position() as usize) / 13,
        "invalid block count"
    );
    let mut blocks = Vec::new();
    let mut physical = HEADER;
    let mut decoded = 0;
    for i in 0..n {
        let raw = u32::from_le_bytes(number(&mut r)?);
        let stored = u32::from_le_bytes(number(&mut r)?);
        let codec = number::<1>(&mut r)?[0];
        let checksum = u32::from_le_bytes(number(&mut r)?);
        ensure!(raw > 0 && raw as usize <= chunk, "invalid block length");
        ensure!(i == n - 1 || raw as usize == chunk, "short non-final block");
        ensure!(
            codec <= 1 && stored > 0 && stored <= raw,
            "invalid stored block"
        );
        ensure!(codec != 0 || stored == raw, "raw block length mismatch");
        decoded = checked_add(decoded, raw as u64)?;
        physical = checked_add(physical, stored as u64)?;
        blocks.push(Block {
            raw,
            stored,
            codec,
            checksum,
        });
    }
    ensure!(
        decoded == logical && physical == start,
        "payload extents disagree with index"
    );
    ensure!(r.position() == bytes.len() as u64, "unexpected index bytes");
    Ok(Index {
        items,
        blocks,
        chunk,
    })
}

pub(super) fn list(path: &Path) -> Result<Vec<Entry>> {
    Ok(read_index(&mut File::open(path)?)?
        .items
        .into_iter()
        .map(|i| i.entry)
        .collect())
}

pub(super) fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    selected: &[String],
) -> Result<Vec<Entry>> {
    let mut file = File::open(path)?;
    let index = read_index(&mut file)?;
    for name in selected {
        safe_path(name)?;
        ensure!(
            index.items.iter().any(|i| &i.entry.name == name),
            "selected path not found: {name}"
        );
    }
    let items: Vec<_> = index
        .items
        .iter()
        .filter(|i| {
            selected.is_empty()
                || selected.iter().any(|s| {
                    i.entry.name == *s
                        || i.entry
                            .name
                            .strip_prefix(s)
                            .is_some_and(|tail| tail.starts_with('/'))
                })
        })
        .cloned()
        .collect();
    let mut needed = vec![selected.is_empty(); index.blocks.len()];
    for item in &items {
        if item.entry.size == 0 {
            continue;
        }
        let first = item.offset / index.chunk as u64;
        let last = (item.offset + item.entry.size - 1) / index.chunk as u64;
        needed[first as usize..=last as usize].fill(true);
    }
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
            if item.entry.directory {
                directories.insert(path);
            } else {
                directories.insert(parent(&path).to_owned());
            }
        }
        for path in directories {
            fs::create_dir_all(path)?;
        }
        for item in &items {
            if !item.entry.directory && item.entry.size == 0 {
                File::options()
                    .write(true)
                    .create_new(true)
                    .open(temp.path().join(&item.entry.name))?;
            }
        }
    }
    let files: Vec<_> = items
        .iter()
        .filter(|i| !i.entry.directory && i.entry.size > 0)
        .collect();
    let count = workers(needed.iter().filter(|&&b| b).count());
    let (recycle, recycled) = sync_channel::<Vec<u8>>(count * 2);
    let mut current: Option<(usize, BufWriter<File>)> = None;
    let mut verified = 0usize;
    let expected = needed.iter().filter(|&&b| b).count();
    pipeline::run(
        count,
        move |emit| {
            let mut reader = BufReader::new(file);
            reader.seek(SeekFrom::Start(HEADER))?;
            let mut physical = HEADER;
            let mut offset = 0;
            let mut at = HEADER;
            for (b, wanted) in index.blocks.iter().zip(&needed) {
                if *wanted {
                    if at != physical {
                        reader.seek(SeekFrom::Start(physical))?;
                    }
                    let mut data = recycled.try_recv().unwrap_or_default();
                    data.resize(b.stored as usize, 0);
                    reader.read_exact(&mut data)?;
                    at = physical + b.stored as u64;
                    emit(Packet {
                        block: *b,
                        data,
                        offset,
                    })?;
                }
                physical += b.stored as u64;
                offset += b.raw as u64;
            }
            Ok(())
        },
        || Ok((zstd::bulk::Decompressor::new()?, vec![0u8; index.chunk])),
        |(codec, scratch), mut packet: Packet| {
            if packet.block.codec == 1 {
                scratch.resize(packet.block.raw as usize, 0);
                let len = codec.decompress_to_buffer(&packet.data, &mut scratch[..])?;
                ensure!(
                    len == packet.block.raw as usize,
                    "decoded block length mismatch"
                );
                std::mem::swap(&mut packet.data, scratch);
            }
            ensure!(
                crc32fast::hash(&packet.data) == packet.block.checksum,
                "block checksum mismatch"
            );
            Ok(packet)
        },
        |packet| {
            verified += 1;
            if let Some(temp) = &staging {
                let end = packet.offset + packet.data.len() as u64;
                let first = files.partition_point(|i| i.offset + i.entry.size <= packet.offset);
                for (n, item) in files
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take_while(|(_, i)| i.offset < end)
                {
                    if current.as_ref().is_none_or(|(id, _)| *id != n) {
                        if let Some((_, mut w)) = current.take() {
                            w.flush()?;
                        }
                        let f = File::options()
                            .write(true)
                            .create_new(true)
                            .open(temp.path().join(&item.entry.name))?;
                        current = Some((n, BufWriter::new(f)));
                    }
                    let start = item.offset.max(packet.offset) - packet.offset;
                    let stop = (item.offset + item.entry.size).min(end) - packet.offset;
                    current
                        .as_mut()
                        .unwrap()
                        .1
                        .write_all(&packet.data[start as usize..stop as usize])?;
                }
            }
            let _ = recycle.try_send(packet.data);
            Ok(())
        },
    )?;
    ensure!(verified == expected, "incomplete decompression pipeline");
    if let Some((_, mut w)) = current {
        w.flush()?;
    }
    if let (Some(temp), Some(dst)) = (staging, destination) {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "destination appeared during extraction"
        );
        fs::rename(temp.path(), dst)?;
    }
    Ok(items.into_iter().map(|i| i.entry).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn index_file(bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
        let mut f = tempfile::NamedTempFile::new()?;
        f.write_all(MAGIC)?;
        f.write_all(&(CHUNK as u32).to_le_bytes())?;
        f.write_all(&0u32.to_le_bytes())?;
        f.write_all(b"x")?;
        let compressed = zstd::bulk::compress(bytes, 1)?;
        f.write_all(&compressed)?;
        f.write_all(&17u64.to_le_bytes())?;
        f.write_all(&(compressed.len() as u64).to_le_bytes())?;
        f.write_all(&(bytes.len() as u64).to_le_bytes())?;
        f.write_all(&crc32fast::hash(bytes).to_le_bytes())?;
        f.write_all(&0u32.to_le_bytes())?;
        f.write_all(END)?;
        Ok(f)
    }
    #[test]
    fn reject_invalid_metadata_even_with_valid_checksum() -> Result<()> {
        let items = vec![Item {
            entry: Entry {
                name: "file".into(),
                size: 1,
                directory: false,
            },
            offset: 0,
        }];
        let blocks = vec![Block {
            raw: 1,
            stored: 1,
            codec: 0,
            checksum: crc32fast::hash(b"x"),
        }];
        let valid = index_file(&encode_index(&items, &blocks)?)?;
        assert!(read_index(&mut valid.reopen()?).is_ok());
        for case in 0..10 {
            let mut items = items.clone();
            let mut blocks = blocks.clone();
            match case {
                0 => items[0].entry.name = "../escape".into(),
                1 => items[0].offset = 1,
                2 => items[0].entry.size = u64::MAX,
                3 => items[0].entry.directory = true,
                4 => blocks[0].stored = 2,
                5 => blocks[0].codec = 3,
                6 => blocks[0].raw = 0,
                7 => items.push(Item {
                    entry: Entry {
                        name: "file/child".into(),
                        size: 0,
                        directory: false,
                    },
                    offset: 1,
                }),
                8 => items.push(Item {
                    entry: Entry {
                        name: "file".into(),
                        size: 0,
                        directory: false,
                    },
                    offset: 1,
                }),
                _ => {}
            }
            let mut bytes = encode_index(&items, &blocks)?;
            if case == 9 {
                bytes[..4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            let f = index_file(&bytes)?;
            assert!(
                read_index(&mut f.reopen()?).is_err(),
                "accepted invalid metadata case {case}"
            );
        }
        Ok(())
    }
}
