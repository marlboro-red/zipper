//! In-process extraction of 7z archives whose folders use Copy, LZMA, LZMA2,
//! Deflate and the liblzma branch and delta filters. See
//! `docs/reference/7z-reader.md` for what it accepts and restores.
use crate::{Entry, counters, progress::Tracker, safe_path};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{Receiver, sync_channel},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SIGNATURE: &[u8; 6] = b"7z\xBC\xAF\x27\x1C";
const SIGNATURE_HEADER: u64 = 32;
const MAX_HEADER: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_CODERS: usize = 4;
const MAX_HEADER_LAYERS: usize = 4;
const COPY_BUFFER: usize = 1024 * 1024;
const DECODER_MEMORY: u64 = 2 * 1024 * 1024 * 1024;
const BZIP2_DECODER_MEMORY: u64 = 4 * 1024 * 1024;
const SEGMENT_BUDGET: u64 = 512 * 1024 * 1024;
const PARALLEL_FOLDER_MIN: u64 = 8 * 1024 * 1024;
const SEGMENT_MIN_OUTPUT: u64 = 8 * 1024 * 1024;
const SEGMENT_WINDOW_MAX: usize = 64;
const SEGMENT_DECODE_STEP: usize = 8 * 1024 * 1024;
const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

const END: u8 = 0;
const HEADER: u8 = 1;
const ARCHIVE_PROPERTIES: u8 = 2;
const ADDITIONAL_STREAMS: u8 = 3;
const MAIN_STREAMS: u8 = 4;
const FILES_INFO: u8 = 5;
const PACK_INFO: u8 = 6;
const UNPACK_INFO: u8 = 7;
const SUBSTREAMS_INFO: u8 = 8;
const SIZE: u8 = 9;
const CRC: u8 = 10;
const FOLDER: u8 = 11;
const CODERS_UNPACK_SIZE: u8 = 12;
const UNPACK_STREAMS: u8 = 13;
const EMPTY_STREAM: u8 = 14;
const EMPTY_FILE: u8 = 15;
const ANTI: u8 = 16;
const NAME: u8 = 17;
const MODIFIED: u8 = 20;
const ATTRIBUTES: u8 = 21;
const ENCODED_HEADER: u8 = 23;

const ATTRIBUTE_READ_ONLY: u32 = 0x1;
const ATTRIBUTE_DIRECTORY: u32 = 0x10;
const ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;
const UNIX_TYPE: u32 = 0o170000;
const UNIX_DIRECTORY: u32 = 0o040000;
const UNIX_SYMLINK: u32 = 0o120000;
const UNIX_FILE: u32 = 0o100000;

/// 7z reading options.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions<'a> {
    /// Cap on declared output bytes. Without it, extraction must fit in the
    /// destination filesystem's free space.
    pub max_output_bytes: Option<u64>,
    /// Folders decoded at once, 1–64. Default: the Rayon pool size.
    pub threads: Option<usize>,
    /// What to do when the destination exists.
    pub existing: crate::Existing,
    /// Exact entry paths or folder subtrees to read. Empty reads every entry.
    pub paths: &'a [String],
    /// Wildcard patterns to read, written as for `ExtractOptions::include`.
    pub include: &'a [String],
    /// Wildcard patterns to leave out, written as for `ExtractOptions::include`.
    pub exclude: &'a [String],
    /// Progress reports and cancellation.
    pub control: crate::Control<'a>,
}

// EXTERNAL(Kani 0.68): building an anyhow error reaches catch_unwind, which crashes Kani's
// compiler, so the readers Kani proves return this type and convert at the boundary.
#[derive(Debug)]
struct Malformed {
    message: &'static str,
    subject: &'static str,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(self.message)?;
        formatter.write_str(self.subject)
    }
}

impl std::error::Error for Malformed {}

type Parsed<T> = std::result::Result<T, Malformed>;

fn check(holds: bool, message: &'static str, subject: &'static str) -> Parsed<()> {
    if holds {
        Ok(())
    } else {
        Err(Malformed { message, subject })
    }
}

struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    fn take(&mut self, length: usize) -> Parsed<&'a [u8]> {
        check(length <= self.remaining(), "7z header is truncated", "")?;
        let bytes = &self.data[self.at..self.at + length];
        self.at += length;
        Ok(bytes)
    }

    fn byte(&mut self) -> Parsed<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Parsed<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Parsed<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn number(&mut self) -> Parsed<u64> {
        let first = self.byte()?;
        let extra = first.leading_ones() as usize;
        let mut bytes = [0u8; 8];
        bytes[..extra].copy_from_slice(self.take(extra)?);
        let low = u64::from_le_bytes(bytes);
        let mask = 0xFFu8.checked_shr(extra as u32 + 1).unwrap_or(0);
        let high = if extra < 8 {
            u64::from(first & mask) << (8 * extra)
        } else {
            0
        };
        Ok(high | low)
    }

    fn count(&mut self, bytes_per_item: usize, what: &'static str) -> Parsed<usize> {
        let count = self.number()?;
        check(
            count <= MAX_ENTRIES as u64 && count as usize * bytes_per_item <= self.remaining(),
            "7z header declares too many ",
            what,
        )?;
        Ok(count as usize)
    }

    fn size(&mut self) -> Parsed<usize> {
        let size = self.number()?;
        check(
            size <= self.remaining() as u64,
            "7z header is truncated",
            "",
        )?;
        Ok(size as usize)
    }

    fn expect(&mut self, id: u8, what: &'static str) -> Parsed<()> {
        check(self.byte()? == id, "7z header is missing ", what)?;
        Ok(())
    }

    fn bits(&mut self, count: usize) -> Parsed<Vec<bool>> {
        let bytes = self.take(count.div_ceil(8))?;
        Ok((0..count)
            .map(|index| bytes[index / 8] & (0x80 >> (index % 8)) != 0)
            .collect())
    }

    fn defined(&mut self, count: usize) -> Parsed<Vec<bool>> {
        if self.byte()? != 0 {
            Ok(vec![true; count])
        } else {
            self.bits(count)
        }
    }

    fn digests(&mut self, count: usize) -> Parsed<Vec<Option<u32>>> {
        self.defined(count)?
            .into_iter()
            .map(|defined| defined.then(|| self.u32()).transpose())
            .collect()
    }

    fn not_external(&mut self) -> Parsed<()> {
        check(
            self.byte()? == 0,
            "7z header uses external data, which is not supported",
            "",
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Coder {
    id: Vec<u8>,
    properties: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Folder {
    output_to_packed: Vec<Coder>,
    unsupported: Option<String>,
    size: u64,
    crc: Option<u32>,
    pack_offset: u64,
    pack_size: u64,
}

#[derive(Debug, Default)]
struct Streams {
    folders: Vec<Folder>,
    files_by_folder: Vec<Vec<(u64, Option<u32>)>>,
}

struct FolderShape {
    coders: Vec<Coder>,
    bind_pairs: Vec<(u64, u64)>,
    packed_inputs: Vec<u64>,
    outputs: usize,
    several_streams: Option<Vec<u8>>,
}

const MAX_CODER_STREAMS: u64 = 32;

fn read_folder(bytes: &mut Bytes) -> Result<FolderShape> {
    let count = bytes.count(1, "coders")?;
    ensure!(
        (1..=MAX_CODERS).contains(&count),
        "7z folder has {count} coders; at most {MAX_CODERS} are supported"
    );
    let mut coders = Vec::with_capacity(count);
    let mut outputs = 0u64;
    let mut inputs = 0u64;
    let mut several_streams = None;
    for _ in 0..count {
        let flags = bytes.byte()?;
        ensure!(flags & 0xC0 == 0, "7z coder uses alternative methods");
        let id = bytes.take(usize::from(flags & 0x0F))?.to_vec();
        let (coder_inputs, coder_outputs) = if flags & 0x10 != 0 {
            (bytes.number()?, bytes.number()?)
        } else {
            (1, 1)
        };
        ensure!(
            (1..=MAX_CODER_STREAMS).contains(&coder_inputs)
                && (1..=MAX_CODER_STREAMS).contains(&coder_outputs),
            "7z coder has a malformed stream count"
        );
        if (coder_inputs, coder_outputs) != (1, 1) {
            several_streams.get_or_insert_with(|| id.clone());
        }
        let properties = if flags & 0x20 != 0 {
            let size = bytes.size()?;
            bytes.take(size)?.to_vec()
        } else {
            Vec::new()
        };
        inputs += coder_inputs;
        outputs += coder_outputs;
        coders.push(Coder { id, properties });
    }
    let bind_pairs = (0..outputs - 1)
        .map(|_| {
            let pair = (bytes.number()?, bytes.number()?);
            ensure!(
                pair.0 < inputs && pair.1 < outputs,
                "7z folder binds a stream it does not have"
            );
            Ok(pair)
        })
        .collect::<Result<Vec<_>>>()?;
    let packed_count = inputs
        .checked_sub(bind_pairs.len() as u64)
        .filter(|&count| count >= 1)
        .context("7z folder has no packed stream")?;
    let packed_inputs = if packed_count == 1 {
        vec![
            (0..inputs)
                .find(|input| bind_pairs.iter().all(|&(bound, _)| bound != *input))
                .context("7z folder binds every input")?,
        ]
    } else {
        (0..packed_count)
            .map(|_| bytes.number())
            .collect::<Parsed<Vec<_>>>()?
    };
    Ok(FolderShape {
        coders,
        bind_pairs,
        packed_inputs,
        outputs: outputs as usize,
        several_streams,
    })
}

fn chain(coders: Vec<Coder>, bind_pairs: &[(u64, u64)], packed: u64) -> Result<Vec<Coder>> {
    let count = coders.len() as u64;
    let output = (0..count)
        .find(|output| bind_pairs.iter().all(|&(_, bound)| bound != *output))
        .context("7z folder has no single output")?;
    let mut order = vec![output];
    let mut current = output;
    while current != packed {
        let &(_, next) = bind_pairs
            .iter()
            .find(|&&(input, _)| input == current)
            .context("7z folder coders do not form a chain")?;
        ensure!(
            next < count && !order.contains(&next),
            "7z folder coders do not form a chain"
        );
        order.push(next);
        current = next;
    }
    ensure!(order.len() == coders.len(), "7z folder has unused coders");
    Ok(order
        .iter()
        .map(|&index| coders[index as usize].clone())
        .collect())
}

fn read_streams(bytes: &mut Bytes) -> Result<Streams> {
    let mut pack_position = 0;
    let mut pack_sizes = Vec::new();
    let mut streams = Streams::default();
    let mut id = bytes.byte()?;
    if id == PACK_INFO {
        pack_position = bytes.number()?;
        let count = bytes.count(1, "packed streams")?;
        loop {
            match bytes.byte()? {
                END => break,
                SIZE => pack_sizes = (0..count).map(|_| bytes.number()).collect::<Parsed<_>>()?,
                CRC => {
                    bytes.digests(count)?;
                }
                other => bail!("7z pack info has unknown property {other}"),
            }
        }
        ensure!(pack_sizes.len() == count, "7z pack info has no sizes");
        id = bytes.byte()?;
    }
    if id == UNPACK_INFO {
        bytes.expect(FOLDER, "its folder list")?;
        let count = bytes.count(2, "folders")?;
        bytes.not_external()?;
        let mut shapes = Vec::with_capacity(count);
        for _ in 0..count {
            shapes.push(read_folder(bytes)?);
        }
        bytes.expect(CODERS_UNPACK_SIZE, "its unpacked sizes")?;
        let mut pack_offset = pack_position;
        let mut packs = pack_sizes.iter();
        for shape in shapes {
            let sizes = (0..shape.outputs)
                .map(|_| bytes.number())
                .collect::<Parsed<Vec<_>>>()?;
            let unbound: Vec<usize> = (0..shape.outputs)
                .filter(|&output| {
                    shape
                        .bind_pairs
                        .iter()
                        .all(|&(_, bound)| bound != output as u64)
                })
                .collect();
            ensure!(unbound.len() == 1, "7z folder has no single output");
            let size = sizes[unbound[0]];
            ensure!(size > 0, "7z folder is empty");
            let mut pack_size = 0u64;
            for _ in &shape.packed_inputs {
                let size = *packs.next().context("7z folder has no packed stream")?;
                pack_size = pack_size
                    .checked_add(size)
                    .context("7z packed streams overflow")?;
            }
            let (output_to_packed, unsupported) = match shape.several_streams {
                Some(id) => (
                    Vec::new(),
                    Some(format!(
                        "7z coder {} has several streams, as BCJ2 does, which is not supported; {BACKEND_HINT}",
                        hex(&id)
                    )),
                ),
                None => (
                    chain(shape.coders, &shape.bind_pairs, shape.packed_inputs[0])?,
                    None,
                ),
            };
            streams.folders.push(Folder {
                output_to_packed,
                unsupported,
                size,
                crc: None,
                pack_offset,
                pack_size,
            });
            pack_offset = pack_offset
                .checked_add(pack_size)
                .context("7z packed streams overflow")?;
        }
        ensure!(
            packs.next().is_none(),
            "7z has packed streams no folder uses"
        );
        loop {
            match bytes.byte()? {
                END => break,
                CRC => {
                    let digests = bytes.digests(streams.folders.len())?;
                    for (folder, crc) in streams.folders.iter_mut().zip(digests) {
                        folder.crc = crc;
                    }
                }
                other => bail!("7z unpack info has unknown property {other}"),
            }
        }
        id = bytes.byte()?;
    }
    let folders = streams.folders.len();
    let mut counts = vec![1usize; folders];
    let mut sizes: Option<Vec<Vec<u64>>> = None;
    let mut digests: Option<Vec<Option<u32>>> = None;
    if id == SUBSTREAMS_INFO {
        id = bytes.byte()?;
        if id == UNPACK_STREAMS {
            let mut total = 0usize;
            for count in &mut counts {
                *count = usize::try_from(bytes.number()?)?;
                total = total
                    .checked_add(*count)
                    .filter(|&total| total <= MAX_ENTRIES)
                    .context("7z declares too many files")?;
            }
            id = bytes.byte()?;
        }
        if id == SIZE {
            let mut all = Vec::with_capacity(folders);
            for (folder, &count) in streams.folders.iter().zip(&counts) {
                let mut listed = Vec::new();
                let mut sum = 0u64;
                for _ in 1..count.max(1) {
                    let size = bytes.number()?;
                    sum = sum.checked_add(size).context("7z file sizes overflow")?;
                    listed.push(size);
                }
                if count > 0 {
                    listed.push(
                        folder
                            .size
                            .checked_sub(sum)
                            .context("7z file sizes exceed their folder")?,
                    );
                }
                all.push(listed);
            }
            sizes = Some(all);
            id = bytes.byte()?;
        }
        let unknown = streams
            .folders
            .iter()
            .zip(&counts)
            .map(|(folder, &count)| {
                if count == 1 && folder.crc.is_some() {
                    0
                } else {
                    count
                }
            })
            .sum::<usize>();
        loop {
            match id {
                END => break,
                CRC => digests = Some(bytes.digests(unknown)?),
                other => bail!("7z substreams info has unknown property {other}"),
            }
            id = bytes.byte()?;
        }
        id = bytes.byte()?;
    }
    ensure!(id == END, "7z streams info has unknown property {id}");
    let sizes = match sizes {
        Some(sizes) => sizes,
        None => {
            ensure!(
                counts.iter().all(|&count| count <= 1),
                "7z folder holds several files without their sizes"
            );
            streams
                .folders
                .iter()
                .zip(&counts)
                .map(|(folder, &count)| {
                    if count == 1 {
                        vec![folder.size]
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        }
    };
    let mut digests = digests.unwrap_or_default().into_iter();
    for (folder, sizes) in streams.folders.iter().zip(sizes) {
        let inherits = sizes.len() == 1 && folder.crc.is_some();
        let files = sizes
            .into_iter()
            .map(|size| {
                let crc = if inherits {
                    folder.crc
                } else {
                    digests.next().flatten()
                };
                (size, crc)
            })
            .collect();
        streams.files_by_folder.push(files);
    }
    Ok(streams)
}

#[derive(Debug)]
struct Item {
    name: String,
    kind: Kind,
    mode: Option<u32>,
    windows_attributes: u32,
    modified: Option<SystemTime>,
    stream: Option<(usize, usize)>,
    size: u64,
    crc: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Symlink,
}

fn file_time(filetime: u64) -> Option<SystemTime> {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let ticks = i128::from(filetime) - i128::from(UNIX_EPOCH_AS_FILETIME);
    let since = Duration::from_nanos(u64::try_from(ticks.unsigned_abs() * 100).ok()?);
    if ticks >= 0 {
        UNIX_EPOCH.checked_add(since)
    } else {
        UNIX_EPOCH.checked_sub(since)
    }
}

fn read_names(bytes: &mut Bytes, count: usize) -> Result<Vec<String>> {
    bytes.not_external()?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let mut units = Vec::new();
        loop {
            let unit = u16::from_le_bytes(bytes.take(2)?.try_into().unwrap());
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        let name = String::from_utf16(&units).context("7z name is not valid UTF-16")?;
        names.push(name);
    }
    ensure!(bytes.remaining() == 0, "7z name list has trailing bytes");
    Ok(names)
}

fn read_files(bytes: &mut Bytes, streams: &Streams) -> Result<Vec<Item>> {
    let count = bytes.count(1, "files")?;
    let mut empty_stream = vec![false; count];
    let mut empty_stream_seen = false;
    let mut empty_file = Vec::new();
    let mut names = None;
    let mut modified = vec![None; count];
    let mut attributes = vec![None; count];
    loop {
        let id = bytes.byte()?;
        if id == END {
            break;
        }
        let size = bytes.size()?;
        let mut property = Bytes {
            data: bytes.take(size)?,
            at: 0,
        };
        let empty_count = empty_stream.iter().filter(|&&empty| empty).count();
        match id {
            EMPTY_STREAM => {
                empty_stream = property.bits(count)?;
                empty_stream_seen = true;
            }
            EMPTY_FILE | ANTI if !empty_stream_seen => {
                bail!("7z empty-file list comes before the empty-stream list")
            }
            EMPTY_FILE => empty_file = property.bits(empty_count)?,
            ANTI => ensure!(
                property.bits(empty_count)?.iter().all(|&anti| !anti),
                "7z anti-items, which delete files, are not supported"
            ),
            NAME => names = Some(read_names(&mut property, count)?),
            MODIFIED => {
                let defined = property.defined(count)?;
                property.not_external()?;
                for (slot, defined) in modified.iter_mut().zip(defined) {
                    if defined {
                        *slot = file_time(property.u64()?);
                    }
                }
            }
            ATTRIBUTES => {
                let defined = property.defined(count)?;
                property.not_external()?;
                for (slot, defined) in attributes.iter_mut().zip(defined) {
                    if defined {
                        *slot = Some(property.u32()?);
                    }
                }
            }
            _ => {}
        }
    }
    let names = names.context("7z archive has no file names")?;
    let mut streams_in_order = streams
        .files_by_folder
        .iter()
        .enumerate()
        .flat_map(|(folder, files)| (0..files.len()).map(move |index| (folder, index)));
    let mut empty_index = 0;
    let mut items = Vec::with_capacity(count);
    for (index, name) in names.into_iter().enumerate() {
        let attribute = attributes[index].unwrap_or(0);
        ensure!(
            attribute & ATTRIBUTE_REPARSE_POINT == 0,
            "7z entry {name:?} is a Windows reparse point, which is not extracted"
        );
        let unix = (attribute & ATTRIBUTE_UNIX_EXTENSION != 0).then_some(attribute >> 16);
        let (kind, stream) = if empty_stream[index] {
            let file = empty_file.get(empty_index).copied().unwrap_or(false);
            empty_index += 1;
            let directory = !file;
            ensure!(
                directory
                    == (attribute & ATTRIBUTE_DIRECTORY != 0
                        || unix.is_some_and(|mode| mode & UNIX_TYPE == UNIX_DIRECTORY))
                    || attributes[index].is_none(),
                "7z entry {name:?} disagrees with its attributes about being a folder"
            );
            (
                if directory {
                    Kind::Directory
                } else {
                    Kind::File
                },
                None,
            )
        } else {
            let stream = streams_in_order
                .next()
                .context("7z has more files than streams")?;
            let kind = match unix.map(|mode| mode & UNIX_TYPE) {
                Some(UNIX_SYMLINK) => Kind::Symlink,
                None | Some(0) | Some(UNIX_FILE) => Kind::File,
                Some(_) => bail!("7z entry {name:?} is a device, FIFO or folder with data"),
            };
            ensure!(
                attribute & ATTRIBUTE_DIRECTORY == 0,
                "7z entry {name:?} is a folder with data"
            );
            (kind, Some(stream))
        };
        let name = name.trim_end_matches('/').to_owned();
        safe_path(&name)?;
        let (size, crc) = stream.map_or((0, None), |(folder, file)| {
            streams.files_by_folder[folder][file]
        });
        let mode = match (kind, unix) {
            (_, Some(mode)) => Some(mode & 0o777),
            (Kind::Directory, None) => None,
            (_, None) if attribute & ATTRIBUTE_READ_ONLY != 0 => Some(0o444),
            (_, None) => Some(0o666),
        };
        items.push(Item {
            name,
            kind,
            mode,
            windows_attributes: crate::windows_attributes(attribute, mode),
            modified: modified[index],
            stream,
            size,
            crc,
        });
    }
    ensure!(
        streams_in_order.next().is_none(),
        "7z has streams that no file uses"
    );
    Ok(items)
}

fn check_names(items: &[Item]) -> Result<()> {
    let mut kinds: HashMap<&str, Option<Kind>> = HashMap::new();
    for item in items {
        let name = item.name.as_str();
        for (slash, _) in name.match_indices('/') {
            match kinds.entry(&name[..slash]).or_insert(None) {
                None | Some(Kind::Directory) => {}
                Some(_) => bail!("7z file used as directory: {name:?}"),
            }
        }
        match kinds.insert(name, Some(item.kind)) {
            None => {}
            Some(None | Some(Kind::Directory)) if item.kind == Kind::Directory => {}
            Some(None) => bail!("7z file used as directory: {name:?}"),
            Some(_) => bail!("duplicate 7z path: {name:?}"),
        }
    }
    Ok(())
}

struct Archive {
    folders: Vec<Folder>,
    items: Vec<Item>,
}

fn decode_header(file: &mut File, streams: &Streams) -> Result<Vec<u8>> {
    ensure!(
        streams.folders.len() == 1 && streams.files_by_folder[0].len() == 1,
        "7z encoded header must be one folder"
    );
    let folder = &streams.folders[0];
    ensure!(folder.size <= MAX_HEADER, "7z header is larger than 64 MiB");
    let mut decoded = Vec::with_capacity(folder.size as usize);
    folder_reader(file.try_clone()?, folder, None)?
        .take(folder.size + 1)
        .read_to_end(&mut decoded)?;
    ensure!(
        decoded.len() as u64 == folder.size,
        "7z header has the wrong size"
    );
    if let Some(crc) = folder.crc.or(streams.files_by_folder[0][0].1) {
        ensure!(crc32fast::hash(&decoded) == crc, "7z header CRC mismatch");
    }
    Ok(decoded)
}

fn open(path: &Path) -> Result<(File, Archive)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut start = [0u8; 32];
    file.read_exact(&mut start).context("not a 7z archive")?;
    ensure!(&start[..6] == SIGNATURE, "not a 7z archive");
    ensure!(
        start[6] == 0,
        "7z format version {}.{} is not supported",
        start[6],
        start[7]
    );
    ensure!(
        crc32fast::hash(&start[12..32]) == u32::from_le_bytes(start[8..12].try_into().unwrap()),
        "7z start header CRC mismatch"
    );
    let offset = u64::from_le_bytes(start[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(start[20..28].try_into().unwrap());
    let crc = u32::from_le_bytes(start[28..32].try_into().unwrap());
    let header_start = SIGNATURE_HEADER
        .checked_add(offset)
        .filter(|&header| header.checked_add(size).is_some_and(|end| end <= length));
    let header_start = match header_start {
        Some(start) => start,
        None if path.extension().is_some_and(|extension| extension == "001") => {
            return Err(unsupported(format!(
                "7z header is in a later volume of a split archive; {BACKEND_HINT}"
            )));
        }
        None => bail!("7z header lies outside the file"),
    };
    ensure!(size <= MAX_HEADER, "7z header is larger than 64 MiB");
    ensure!(size > 0, "empty 7z archives have no header");
    file.seek(SeekFrom::Start(header_start))?;
    let mut header = vec![0; size as usize];
    file.read_exact(&mut header)?;
    ensure!(crc32fast::hash(&header) == crc, "7z header CRC mismatch");
    for _ in 0..=MAX_HEADER_LAYERS {
        let mut bytes = Bytes {
            data: &header,
            at: 0,
        };
        match bytes.byte()? {
            ENCODED_HEADER => {
                let streams = read_streams(&mut bytes)?;
                check_packs(&streams.folders, header_start)?;
                header = decode_header(&mut file, &streams)?;
            }
            HEADER => {
                let archive = read_header(&mut bytes)?;
                check_packs(&archive.folders, header_start)?;
                return Ok((file, archive));
            }
            other => bail!("7z header starts with unknown property {other}"),
        }
    }
    bail!("7z header is encoded more than {MAX_HEADER_LAYERS} times")
}

fn check_packs(folders: &[Folder], header_start: u64) -> Result<()> {
    for folder in folders {
        let end = SIGNATURE_HEADER
            .checked_add(folder.pack_offset)
            .and_then(|start| start.checked_add(folder.pack_size));
        ensure!(
            end.is_some_and(|end| end <= header_start),
            "7z packed stream overlaps the header or lies outside the file"
        );
    }
    Ok(())
}

fn read_header(bytes: &mut Bytes) -> Result<Archive> {
    let mut id = bytes.byte()?;
    if id == ARCHIVE_PROPERTIES {
        loop {
            if bytes.byte()? == END {
                break;
            }
            let size = bytes.size()?;
            bytes.take(size)?;
        }
        id = bytes.byte()?;
    }
    ensure!(
        id != ADDITIONAL_STREAMS,
        "7z additional streams are not supported"
    );
    let streams = if id == MAIN_STREAMS {
        let streams = read_streams(bytes)?;
        id = bytes.byte()?;
        streams
    } else {
        Streams::default()
    };
    let items = if id == FILES_INFO {
        let items = read_files(bytes, &streams)?;
        id = bytes.byte()?;
        items
    } else {
        ensure!(streams.folders.is_empty(), "7z has data but no files");
        Vec::new()
    };
    ensure!(id == END, "7z header has unknown property {id}");
    ensure!(bytes.remaining() == 0, "7z header has trailing bytes");
    check_names(&items)?;
    Ok(Archive {
        folders: streams.folders,
        items,
    })
}

fn hex(id: &[u8]) -> String {
    id.iter().map(|byte| format!("{byte:02X}")).collect()
}

enum Method {
    Copy,
    Deflate,
    Deflate64,
    Bzip2,
    Lzma,
    Lzma2,
    Filter(fn(&mut liblzma::stream::Filters, &[u8]) -> Result<(), liblzma::stream::Error>),
}

fn method(coder: &Coder) -> Result<Method> {
    use liblzma::stream::Filters;
    Ok(match coder.id.as_slice() {
        [0x00] => Method::Copy,
        [0x04, 0x01, 0x08] => Method::Deflate,
        [0x04, 0x01, 0x09] => Method::Deflate64,
        [0x04, 0x02, 0x02] => Method::Bzip2,
        [0x03, 0x01, 0x01] => Method::Lzma,
        [0x21] => Method::Lzma2,
        [0x03] => {
            Method::Filter(|filters, properties| filters.delta_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x01, 0x03] => {
            Method::Filter(|filters, properties| filters.x86_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x02, 0x05] => {
            Method::Filter(|filters, properties| filters.powerpc_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x04, 0x01] => {
            Method::Filter(|filters, properties| filters.ia64_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x05, 0x01] => {
            Method::Filter(|filters, properties| filters.arm_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x07, 0x01] => {
            Method::Filter(|filters, properties| filters.arm_thumb_properties(properties).map(drop))
        }
        [0x03, 0x03, 0x08, 0x05] => {
            Method::Filter(|filters, properties| filters.sparc_properties(properties).map(drop))
        }
        [0x0A] => Method::Filter(|filters: &mut Filters, properties| {
            filters.arm64_properties(properties).map(drop)
        }),
        [0x0B] => {
            Method::Filter(|filters, properties| filters.riscv_properties(properties).map(drop))
        }
        [0x06, 0xF1, 0x07, 0x01] => {
            return Err(unsupported(
                "7z archive is encrypted, which is not supported".into(),
            ));
        }
        id => {
            return Err(unsupported(format!(
                "7z method {} is not supported; {BACKEND_HINT}",
                hex(id)
            )));
        }
    })
}

/// A method or method combination this reader does not decode, as opposed to
/// a damaged or unsafe archive.
#[derive(Debug)]
struct Unsupported(String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

fn unsupported(message: String) -> anyhow::Error {
    Unsupported(message).into()
}

/// Whether a 7z archive should go to 7-Zip because it uses a method this
/// reader does not decode. A damaged or unsafe archive returns false, so the
/// reader refuses it rather than 7-Zip extracting it.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let error = match open(path) {
        Ok((_, archive)) => match archive
            .folders
            .iter()
            .try_for_each(|folder| check_methods(folder).map(drop))
        {
            Ok(()) => return Ok(false),
            Err(error) => error,
        },
        Err(error) => error,
    };
    Ok(error.is::<Unsupported>())
}

fn check_methods(folder: &Folder) -> Result<Vec<Method>> {
    if let Some(reason) = &folder.unsupported {
        return Err(unsupported(reason.clone()));
    }
    let methods = folder
        .output_to_packed
        .iter()
        .map(method)
        .collect::<Result<Vec<_>>>()?;
    match methods.as_slice() {
        [Method::Copy] | [Method::Deflate] | [Method::Deflate64] | [Method::Bzip2] => {}
        [filters @ .., Method::Lzma | Method::Lzma2]
            if filters
                .iter()
                .all(|method| matches!(method, Method::Filter(_))) =>
        {
            lzma_filters(folder, &methods)?;
        }
        _ => {
            return Err(unsupported(format!(
                "7z folder combines methods in a way that is not supported; {BACKEND_HINT}"
            )));
        }
    }
    Ok(methods)
}

fn lzma_filters(folder: &Folder, methods: &[Method]) -> Result<liblzma::stream::Filters> {
    let mut filters = liblzma::stream::Filters::new();
    for (coder, method) in folder.output_to_packed.iter().zip(methods) {
        let properties = dictionary_capped_at_output(method, &coder.properties, folder.size)?;
        match method {
            Method::Filter(add) => add(&mut filters, &properties),
            Method::Lzma => filters.lzma1_properties(&properties).map(drop),
            Method::Lzma2 => filters.lzma2_properties(&properties).map(drop),
            Method::Copy | Method::Deflate | Method::Deflate64 | Method::Bzip2 => {
                return Err(unsupported(format!(
                    "7z folder combines methods in a way that is not supported; {BACKEND_HINT}"
                )));
            }
        }
        .map_err(|error| {
            unsupported(format!(
                "7z method {} has properties liblzma does not accept ({error}); {BACKEND_HINT}",
                hex(&coder.id)
            ))
        })?;
    }
    Ok(filters)
}

fn lzma2_dictionary(property: u8) -> u64 {
    if property >= 40 {
        u64::from(u32::MAX)
    } else {
        u64::from(2 | (property & 1)) << (property / 2 + 11)
    }
}

fn dictionary_capped_at_output(method: &Method, properties: &[u8], output: u64) -> Result<Vec<u8>> {
    let needed = output.max(4096);
    Ok(match method {
        Method::Lzma => {
            ensure!(properties.len() == 5, "7z LZMA properties are malformed");
            let declared = u64::from(u32::from_le_bytes(properties[1..5].try_into().unwrap()));
            let mut bounded = properties.to_vec();
            bounded[1..5].copy_from_slice(&(declared.min(needed) as u32).to_le_bytes());
            bounded
        }
        Method::Lzma2 => {
            ensure!(
                properties.len() == 1 && properties[0] <= 40,
                "7z LZMA2 properties are malformed"
            );
            let declared = lzma2_dictionary(properties[0]);
            let smallest = (0..=properties[0])
                .find(|&property| lzma2_dictionary(property) >= declared.min(needed))
                .unwrap_or(properties[0]);
            vec![smallest]
        }
        _ => properties.to_vec(),
    })
}

fn decoder_memory(folder: &Folder, segments_possible: bool) -> u64 {
    let lzma2_only = matches!(
        folder.output_to_packed.as_slice(),
        [coder] if coder.id == [0x21]
    );
    if segments_possible && lzma2_only && folder.size >= PARALLEL_FOLDER_MIN {
        return SEGMENT_BUDGET;
    }
    match folder
        .output_to_packed
        .last()
        .map(|coder| (method(coder), &coder.properties))
    {
        Some((Ok(Method::Lzma), properties)) if properties.len() == 5 => {
            u64::from(u32::from_le_bytes(properties[1..5].try_into().unwrap())).min(folder.size)
        }
        Some((Ok(Method::Lzma2), properties)) if properties.len() == 1 => {
            lzma2_dictionary(properties[0]).min(folder.size)
        }
        Some((Ok(Method::Bzip2), _)) => BZIP2_DECODER_MEMORY,
        _ => 0,
    }
    .max(64 * 1024)
}

struct LzmaReader<R> {
    stream: liblzma::stream::Stream,
    input: R,
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    input_done: bool,
}

impl<R: Read> Read for LzmaReader<R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.start == self.end && !self.input_done {
                self.end = self.input.read(&mut self.buffer)?;
                self.start = 0;
                self.input_done = self.end == 0;
            }
            let (read, written) = (self.stream.total_in(), self.stream.total_out());
            let action = if self.input_done {
                liblzma::stream::Action::Finish
            } else {
                liblzma::stream::Action::Run
            };
            let status = counters::time(counters::Counter::DecodeNanos, || {
                self.stream
                    .process(&self.buffer[self.start..self.end], output, action)
            })
            .map_err(std::io::Error::other)?;
            self.start += (self.stream.total_in() - read) as usize;
            let produced = (self.stream.total_out() - written) as usize;
            if produced > 0 || status == liblzma::stream::Status::StreamEnd {
                return Ok(produced);
            }
            if self.input_done {
                return Ok(0);
            }
        }
    }
}

fn folder_reader(
    mut file: File,
    folder: &Folder,
    segment_threads: Option<(&Path, usize)>,
) -> Result<Box<dyn Read + Send>> {
    let methods = check_methods(folder)?;
    if let ([Method::Lzma2], [coder]) = (methods.as_slice(), folder.output_to_packed.as_slice())
        && let Some((path, window)) = segment_threads.filter(|&(_, window)| window > 1)
        && folder.size >= PARALLEL_FOLDER_MIN
        && let Some(&properties) = coder.properties.first()
        && let Some(segments) =
            independent_lzma2_segments(&mut file, folder, properties, SEGMENT_MIN_OUTPUT)?
    {
        return Ok(Box::new(ParallelLzma2 {
            path: path.to_owned(),
            pack_start: SIGNATURE_HEADER + folder.pack_offset,
            properties,
            segments,
            window: window.min(SEGMENT_WINDOW_MAX),
            stop: Arc::new(AtomicBool::new(false)),
            next: 0,
            queue: VecDeque::new(),
            held: 0,
            current_held: 0,
            current: Vec::new(),
            position: 0,
        }));
    }
    file.seek(SeekFrom::Start(SIGNATURE_HEADER + folder.pack_offset))?;
    let packed = BufReader::with_capacity(COPY_BUFFER, file).take(folder.pack_size);
    Ok(match methods.as_slice() {
        [Method::Copy] => Box::new(packed),
        [Method::Deflate] => Box::new(flate2::read::DeflateDecoder::new(packed)),
        [Method::Deflate64] => Box::new(deflate64::Deflate64Decoder::with_buffer(packed)),
        [Method::Bzip2] => {
            crate::native_tar::decode(packed, crate::native_tar::Compression::Bzip2)?
        }
        _ => {
            let stream = liblzma::stream::Stream::new_raw_decoder(&lzma_filters(folder, &methods)?)
                .context("cannot start the 7z decoder")?;
            let reader = LzmaReader {
                stream,
                input: packed,
                buffer: vec![0; COPY_BUFFER],
                start: 0,
                end: 0,
                input_done: false,
            };
            if matches!(methods.last(), Some(Method::Lzma)) {
                Box::new(reader.take(folder.size))
            } else {
                Box::new(reader)
            }
        }
    })
}

struct Segment {
    pack_offset: u64,
    pack_length: u64,
    output: u64,
}

impl Segment {
    fn held(&self, properties: u8) -> u64 {
        self.pack_length + self.output + lzma2_dictionary(properties).min(self.output)
    }
}

struct Chunk {
    header: u64,
    unpacked: u64,
    payload: u64,
    resets_dictionary: bool,
}

fn chunk_header(control: u8, fields: [u8; 4]) -> Option<Chunk> {
    match control {
        1 | 2 => {
            let size = u64::from(u16::from_be_bytes([fields[0], fields[1]])) + 1;
            Some(Chunk {
                header: 3,
                unpacked: size,
                payload: size,
                resets_dictionary: control == 1,
            })
        }
        0x80..=0xFF => Some(Chunk {
            header: 5 + u64::from((control >> 5) & 3 >= 2),
            unpacked: (u64::from(control & 0x1F) << 16)
                + u64::from(u16::from_be_bytes([fields[0], fields[1]]))
                + 1,
            payload: u64::from(u16::from_be_bytes([fields[2], fields[3]])) + 1,
            resets_dictionary: control >= 0xE0,
        }),
        _ => None,
    }
}

fn header_field_length(control: u8) -> usize {
    match control {
        1 | 2 => 2,
        _ => 4,
    }
}

struct SegmentPlan {
    segments: Vec<Segment>,
    packed: u64,
    output: u64,
    min_output: u64,
}

impl SegmentPlan {
    fn new(min_output: u64) -> Self {
        SegmentPlan {
            segments: Vec::new(),
            packed: 0,
            output: 0,
            min_output,
        }
    }

    fn add_or_reject(&mut self, chunk: &Chunk, folder: &Folder) -> bool {
        let current_is_small = self
            .segments
            .last()
            .is_some_and(|segment| segment.output < self.min_output);
        if chunk.resets_dictionary && !current_is_small {
            self.segments.push(Segment {
                pack_offset: self.packed,
                pack_length: 0,
                output: 0,
            });
        }
        let Some(segment) = self.segments.last_mut() else {
            return false;
        };
        segment.pack_length += chunk.header + chunk.payload;
        segment.output += chunk.unpacked;
        self.packed += chunk.header + chunk.payload;
        self.output += chunk.unpacked;
        self.packed < folder.pack_size && self.output <= folder.size
    }

    fn finish(self, folder: &Folder, properties: u8) -> Option<Vec<Segment>> {
        let complete = self.output == folder.size && self.segments.len() >= 2;
        let fits = self
            .segments
            .iter()
            .all(|segment| segment.held(properties) <= SEGMENT_BUDGET);
        (complete && fits).then_some(self.segments)
    }
}

fn independent_lzma2_segments(
    source: &mut (impl Read + Seek),
    folder: &Folder,
    properties: u8,
    min_output: u64,
) -> std::io::Result<Option<Vec<Segment>>> {
    source.seek(SeekFrom::Start(SIGNATURE_HEADER + folder.pack_offset))?;
    let mut input = BufReader::new(&mut *source);
    let mut plan = SegmentPlan::new(min_output);
    loop {
        if plan.packed >= folder.pack_size {
            return Ok(None);
        }
        let mut control = [0u8; 1];
        input.read_exact(&mut control)?;
        let control = control[0];
        if control == 0 {
            return Ok(plan.finish(folder, properties));
        }
        let mut fields = [0u8; 4];
        input.read_exact(&mut fields[..header_field_length(control)])?;
        let Some(chunk) = chunk_header(control, fields) else {
            return Ok(None);
        };
        if chunk.header == 6 {
            input.read_exact(&mut [0u8; 1])?;
        }
        if !plan.add_or_reject(&chunk, folder) {
            return Ok(None);
        }
        input.seek_relative(chunk.payload as i64)?;
    }
}

// RECORD(benchmarks/native-7z/lzma2-segments/README.md): solid folders decode their LZMA2 segments in parallel.
struct ParallelLzma2 {
    path: PathBuf,
    pack_start: u64,
    properties: u8,
    segments: Vec<Segment>,
    window: usize,
    stop: Arc<AtomicBool>,
    next: usize,
    queue: VecDeque<(u64, Receiver<std::io::Result<Vec<u8>>>)>,
    held: u64,
    current_held: u64,
    current: Vec<u8>,
    position: usize,
}

impl ParallelLzma2 {
    fn start_segments(&mut self) -> std::io::Result<()> {
        while let Some(segment) = self.segments.get(self.next)
            && self.queue.len() < self.window
            && self.held + segment.held(self.properties) <= SEGMENT_BUDGET
        {
            let (sender, receiver) = sync_channel(1);
            let path = self.path.clone();
            let stop = Arc::clone(&self.stop);
            let start = self.pack_start + segment.pack_offset;
            let (length, output, properties) =
                (segment.pack_length, segment.output, self.properties);
            let spawned = std::thread::Builder::new().spawn(move || {
                let decoded = decode_segment(&path, start, length, output, properties, &stop);
                let _ = sender.send(decoded);
            });
            if let Err(error) = spawned {
                if self.queue.is_empty() {
                    return Err(error);
                }
                break;
            }
            self.queue
                .push_back((segment.held(self.properties), receiver));
            self.held += segment.held(self.properties);
            self.next += 1;
        }
        Ok(())
    }
}

impl Drop for ParallelLzma2 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Read for ParallelLzma2 {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.current.len() {
            self.held -= self.current_held;
            self.current_held = 0;
            self.current = Vec::new();
            self.position = 0;
            self.start_segments()?;
            let Some((held, receiver)) = self.queue.pop_front() else {
                return Ok(0);
            };
            self.current_held = held;
            self.current = receiver
                .recv()
                .map_err(|_| std::io::Error::other("a 7z segment decoder stopped"))??;
        }
        let length = buffer.len().min(self.current.len() - self.position);
        buffer[..length].copy_from_slice(&self.current[self.position..self.position + length]);
        self.position += length;
        Ok(length)
    }
}

fn decode_segment(
    path: &Path,
    start: u64,
    length: u64,
    output: u64,
    properties: u8,
    stop: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let invalid =
        |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned());
    let too_large = || invalid("7z segment is too large");
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut packed = vec![0u8; usize::try_from(length).map_err(|_| too_large())?];
    file.read_exact(&mut packed)?;
    packed.push(0);
    let capped = dictionary_capped_at_output(&Method::Lzma2, &[properties], output)
        .map_err(|error| invalid(&error.to_string()))?;
    let mut filters = liblzma::stream::Filters::new();
    filters
        .lzma2_properties(&capped)
        .map_err(std::io::Error::other)?;
    let mut stream =
        liblzma::stream::Stream::new_raw_decoder(&filters).map_err(std::io::Error::other)?;
    let output = usize::try_from(output).map_err(|_| too_large())?;
    let mut decoded = vec![0u8; output.checked_add(1).ok_or_else(too_large)?];
    let (mut read, mut written) = (0usize, 0usize);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("7z segment decoding stopped"));
        }
        let end = (written + SEGMENT_DECODE_STEP).min(decoded.len());
        let (before_in, before_out) = (stream.total_in(), stream.total_out());
        let status = counters::time(counters::Counter::DecodeNanos, || {
            stream.process(
                &packed[read..],
                &mut decoded[written..end],
                liblzma::stream::Action::Finish,
            )
        })
        .map_err(std::io::Error::other)?;
        read += (stream.total_in() - before_in) as usize;
        written += (stream.total_out() - before_out) as usize;
        if written > output {
            return Err(invalid("7z LZMA2 segment decodes past its declared size"));
        }
        if status == liblzma::stream::Status::StreamEnd {
            break;
        }
        if stream.total_in() == before_in && stream.total_out() == before_out {
            return Err(invalid("7z LZMA2 segment is truncated"));
        }
    }
    if written != output || read != packed.len() {
        return Err(invalid(
            "7z LZMA2 segment does not decode to its declared size",
        ));
    }
    decoded.truncate(output);
    Ok(decoded)
}

/// List the entries the 7z header describes. Payload CRCs are not checked.
pub fn list(path: &Path) -> Result<Vec<Entry>> {
    let (_, archive) = open(path)?;
    Ok(archive.items.iter().map(entry).collect())
}

fn entry(item: &Item) -> Entry {
    Entry {
        name: item.name.clone(),
        size: item.size,
        directory: item.kind == Kind::Directory,
    }
}

/// Verify and optionally extract a 7z archive into a new directory, or into an
/// existing one with `Existing::Overwrite`. It restores files, directories,
/// Unix modes, modification times and symlinks that stay inside the
/// destination.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    ensure!(
        options
            .threads
            .is_none_or(|threads| (1..=64).contains(&threads)),
        "7z threads must be 1..64"
    );
    let (_, archive) = open(path)?;
    for folder in &archive.folders {
        check_methods(folder)?;
    }
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    selection.ensure_each_path_matches(archive.items.iter().map(|item| item.name.as_str()))?;
    let chosen: Vec<bool> = archive
        .items
        .iter()
        .map(|item| selection.selects(&item.name))
        .collect();
    let entries: Vec<Entry> = archive
        .items
        .iter()
        .zip(&chosen)
        .filter(|(_, chosen)| **chosen)
        .map(|(item, _)| entry(item))
        .collect();
    let tracker = Tracker::new(
        options.control,
        Some(entries.iter().filter(|entry| !entry.directory).count() as u64),
        Some(entries.iter().map(|entry| entry.size).sum()),
    );
    tracker.check()?;
    let staging = match destination {
        Some(destination) => {
            let staging = crate::staging::stage(path, destination, options.existing)?;
            crate::check_output_size(staging.path(), &entries, options.max_output_bytes)?;
            Some(staging)
        }
        None => None,
    };
    let root = staging.as_ref().map(|staging| staging.path());
    if let Some(root) = root {
        for (item, _) in archive
            .items
            .iter()
            .zip(&chosen)
            .filter(|(_, chosen)| **chosen)
        {
            let folder = match item.kind {
                Kind::Directory => root.join(&item.name),
                _ => crate::parent(&root.join(&item.name)).to_owned(),
            };
            fs::create_dir_all(folder)?;
        }
    }
    let mut by_folder: Vec<Vec<usize>> = vec![Vec::new(); archive.folders.len()];
    for (index, item) in archive.items.iter().enumerate() {
        if let Some((folder, _)) = item.stream {
            by_folder[folder].push(index);
        }
    }
    let mut wanted: Vec<usize> = (0..archive.folders.len())
        .filter(|&folder| {
            destination.is_none() || by_folder[folder].iter().any(|&item| chosen[item])
        })
        .collect();
    wanted.sort_unstable_by_key(|&folder| std::cmp::Reverse(archive.folders[folder].size));
    let memory = crate::memory::Memory::new(DECODER_MEMORY);
    let targets = Mutex::new(Vec::new());
    let threads = options
        .threads
        .unwrap_or_else(rayon::current_num_threads)
        .min(wanted.len());
    let segment_threads =
        options.threads.unwrap_or_else(rayon::current_num_threads) / threads.max(1);
    let decode = |folder_index: usize, buffer: &mut [u8]| -> Result<()> {
        tracker.check()?;
        let folder = &archive.folders[folder_index];
        let _reservation = memory.reserve(decoder_memory(folder, segment_threads > 1));
        let mut reader = folder_reader(File::open(path)?, folder, Some((path, segment_threads)))?;
        let mut folder_crc = crc32fast::Hasher::new();
        for &index in &by_folder[folder_index] {
            let item = &archive.items[index];
            let write = root.filter(|_| chosen[index]);
            let mut target = Vec::new();
            let mut output = match (write, item.kind) {
                (Some(root), Kind::File) => {
                    let file = crate::create_new_file(
                        &root.join(&item.name),
                        item.mode.unwrap_or(0o666),
                        item.windows_attributes,
                    )
                            .map_err(|error| {
                                if error.kind() == std::io::ErrorKind::AlreadyExists {
                                    anyhow::anyhow!(
                                        "7z entry {:?} collides with another entry on this filesystem, which ignores case or Unicode normalization",
                                        item.name
                                    )
                                } else {
                                    error.into()
                                }
                            })?;
                    counters::add(counters::Counter::FilesCreated, 1);
                    Some(BufWriter::with_capacity(COPY_BUFFER, file))
                }
                _ => None,
            };
            let mut crc = crc32fast::Hasher::new();
            let mut remaining = item.size;
            while remaining > 0 {
                let want = remaining.min(buffer.len() as u64) as usize;
                let read = reader.read(&mut buffer[..want])?;
                ensure!(read > 0, "7z folder ends before its files do");
                let bytes = &buffer[..read];
                crc.update(bytes);
                folder_crc.update(bytes);
                remaining -= read as u64;
                if let Some(output) = &mut output {
                    output.write_all(bytes)?;
                } else if write.is_some() && item.kind == Kind::Symlink {
                    ensure!(
                        target.len() + read <= crate::MAX_LINK_TARGET as usize,
                        "7z symlink {:?} has an unsafe target",
                        item.name
                    );
                    target.extend_from_slice(bytes);
                }
                if chosen[index] {
                    tracker.add_bytes(read as u64)?;
                }
            }
            if let Some(expected) = item.crc {
                ensure!(
                    crc.finalize() == expected,
                    "7z CRC mismatch in {:?}",
                    item.name
                );
            }
            if let Some(mut output) = output {
                output.flush()?;
                let file = output.into_inner().map_err(|error| error.into_error())?;
                if let Some(time) = item.modified {
                    crate::set_modified_best_effort(&file, time);
                }
            }
            if write.is_some() && item.kind == Kind::Symlink {
                targets.lock().unwrap().push((index, target));
            }
            if chosen[index] {
                tracker.file_done()?;
            }
        }
        ensure!(
            reader.read(&mut buffer[..1])? == 0,
            "7z folder holds more data than its files"
        );
        if let Some(expected) = folder.crc {
            ensure!(folder_crc.finalize() == expected, "7z folder CRC mismatch");
        }
        Ok(())
    };
    let next = AtomicUsize::new(0);
    let failure = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buffer = vec![0u8; COPY_BUFFER];
                while let Some(&folder) = wanted.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Err(error) = decode(folder, &mut buffer) {
                        failure
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .get_or_insert(error);
                        next.store(wanted.len(), Ordering::Relaxed);
                    }
                }
            });
        }
    });
    if let Some(error) = failure
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner())
    {
        return Err(error);
    }
    let Some(staging) = staging else {
        tracker.finish();
        return Ok(entries);
    };
    for (index, item) in archive.items.iter().enumerate() {
        if chosen[index] && item.kind == Kind::File && item.stream.is_none() {
            let file = crate::create_new_file(
                &staging.path().join(&item.name),
                item.mode.unwrap_or(0o666),
                item.windows_attributes,
            )?;
            if let Some(time) = item.modified {
                crate::set_modified_best_effort(&file, time);
            }
            tracker.file_done()?;
        }
    }
    tracker.finish();
    create_links(
        staging.path(),
        &archive.items,
        targets.into_inner().unwrap(),
    )?;
    let names: Vec<Option<&str>> = archive
        .items
        .iter()
        .zip(&chosen)
        .map(|(item, &chosen)| {
            (chosen && item.kind == Kind::Directory).then_some(item.name.as_str())
        })
        .collect();
    let merged = crate::later_spellings_of_same_directory(staging.path(), names.iter().copied())?;
    let directories: Vec<crate::Directory> = archive
        .items
        .iter()
        .enumerate()
        .filter(|&(index, item)| {
            chosen[index] && item.kind == Kind::Directory && !merged.contains(&index)
        })
        .map(|(_, item)| crate::Directory {
            name: &item.name,
            mode: item.mode,
            modified: item.modified,
        })
        .collect();
    tracker.check()?;
    staging.publish(&directories)?;
    Ok(entries)
}

fn create_links(root: &Path, items: &[Item], targets: Vec<(usize, Vec<u8>)>) -> Result<()> {
    for (index, target) in targets {
        let name = &items[index].name;
        let target = std::str::from_utf8(&target).context("7z symlink target is not UTF-8")?;
        crate::ensure_link_stays_inside("7z", name, target)?;
        crate::create_symlink("7z", root, name, target)?;
    }
    Ok(())
}

/// Whether the file starts with the 7z signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut start = [0u8; 6];
    let read = File::open(path)?.read(&mut start)?;
    Ok(read == 6 && &start == SIGNATURE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_decode_every_length() {
        for (bytes, value) in [
            (&[0x7F][..], 0x7F),
            (&[0x80, 0xFF], 0xFF),
            (&[0xBF, 0x34], 0x3F34),
            (&[0xC1, 0x56, 0x34], 0x01_3456),
            (&[0xFE, 1, 2, 3, 4, 5, 6, 7], 0x07_0605_0403_0201),
            (&[0xFF, 1, 2, 3, 4, 5, 6, 7, 8], 0x0807_0605_0403_0201),
        ] {
            assert_eq!(
                Bytes { data: bytes, at: 0 }.number().unwrap(),
                value,
                "{bytes:?}"
            );
        }
        assert!(
            Bytes {
                data: &[0xFF, 1],
                at: 0
            }
            .number()
            .is_err()
        );
    }

    fn uncompressed_lzma2(chunks: usize, chunk: usize) -> (tempfile::NamedTempFile, Folder) {
        let mut bytes = vec![0u8; SIGNATURE_HEADER as usize];
        for index in 0..chunks {
            bytes.push(1);
            bytes.extend_from_slice(&((chunk - 1) as u16).to_be_bytes());
            bytes.extend(std::iter::repeat_n(index as u8, chunk));
        }
        bytes.push(0);
        let pack_size = bytes.len() as u64 - SIGNATURE_HEADER;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        let folder = Folder {
            output_to_packed: vec![Coder {
                id: vec![0x21],
                properties: vec![18],
            }],
            unsupported: None,
            size: (chunks * chunk) as u64,
            crc: None,
            pack_offset: 0,
            pack_size: pack_size + 1,
        };
        (file, folder)
    }

    #[test]
    fn segments_merge_up_to_the_minimum_output() {
        let (file, folder) = uncompressed_lzma2(400, 65_536);
        let segments = independent_lzma2_segments(
            &mut file.reopen().unwrap(),
            &folder,
            18,
            SEGMENT_MIN_OUTPUT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(segments.len(), 4);
        assert!(
            segments[..3]
                .iter()
                .all(|segment| segment.output >= SEGMENT_MIN_OUTPUT)
        );
        assert_eq!(
            segments.iter().map(|segment| segment.output).sum::<u64>(),
            folder.size
        );
        let (file, folder) = uncompressed_lzma2(1_000_000, 1);
        assert!(
            independent_lzma2_segments(
                &mut file.reopen().unwrap(),
                &folder,
                18,
                SEGMENT_MIN_OUTPUT
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn segment_memory_counts_the_dictionary_and_the_shared_budget() {
        let segment = Segment {
            pack_offset: 0,
            pack_length: 10,
            output: 1 << 20,
        };
        assert_eq!(segment.held(40), 10 + (1 << 20) + (1 << 20));
        let (_, folder) = uncompressed_lzma2(400, 65_536);
        assert_eq!(decoder_memory(&folder, true), SEGMENT_BUDGET);
        assert!(decoder_memory(&folder, false) < SEGMENT_BUDGET);
    }

    #[test]
    fn segment_plans_start_at_resets_and_cover_the_folder_for_every_short_stream() {
        let kinds: Vec<Chunk> = [1u8, 2, 0x80, 0xA0, 0xC0, 0xE0]
            .iter()
            .flat_map(|&control| {
                [0u8, 2].map(|size| chunk_header(control, [0, size, 0, size]).unwrap())
            })
            .collect();
        let mut checked = 0u64;
        for length in 1..=4usize {
            for code in 0..kinds.len().pow(length as u32) {
                let stream: Vec<&Chunk> = (0..length)
                    .map(|position| &kinds[code / kinds.len().pow(position as u32) % kinds.len()])
                    .collect();
                let packed: u64 = stream
                    .iter()
                    .map(|chunk| chunk.header + chunk.payload)
                    .sum();
                let output: u64 = stream.iter().map(|chunk| chunk.unpacked).sum();
                for min_output in [0, 1, 2, 3, 4, 6, 9] {
                    for (size, pack_size) in [
                        (output, packed + 1),
                        (output, packed),
                        (output + 1, packed + 1),
                        (output.saturating_sub(1), packed + 1),
                    ] {
                        let folder = Folder {
                            output_to_packed: Vec::new(),
                            unsupported: None,
                            size,
                            crc: None,
                            pack_offset: 0,
                            pack_size,
                        };
                        let mut plan = SegmentPlan::new(min_output);
                        let mut starts = Vec::new();
                        let accepted = stream.iter().all(|chunk| {
                            starts.push((plan.packed, chunk.resets_dictionary));
                            plan.add_or_reject(chunk, &folder)
                        });
                        checked += 1;
                        let Some(segments) = accepted.then(|| plan.finish(&folder, 18)).flatten()
                        else {
                            continue;
                        };
                        assert!(segments.len() >= 2);
                        let mut end = 0;
                        for (index, segment) in segments.iter().enumerate() {
                            assert_eq!(segment.pack_offset, end, "{code} {min_output}");
                            assert!(starts.contains(&(segment.pack_offset, true)));
                            if index + 1 < segments.len() {
                                assert!(segment.output >= min_output);
                            }
                            end += segment.pack_length;
                        }
                        assert!(end < folder.pack_size);
                        assert_eq!(segments.iter().map(|s| s.output).sum::<u64>(), folder.size);
                    }
                }
            }
        }
        assert!(checked > 500_000, "{checked}");
    }

    #[test]
    fn dictionaries_shrink_to_the_output() {
        let bounded = dictionary_capped_at_output(&Method::Lzma2, &[40], 100).unwrap();
        assert_eq!(lzma2_dictionary(bounded[0]), 4096);
        let bounded = dictionary_capped_at_output(&Method::Lzma2, &[18], 1 << 30).unwrap();
        assert_eq!(bounded, [18]);
        let mut lzma = vec![0x5D];
        lzma.extend_from_slice(&u32::MAX.to_le_bytes());
        let bounded = dictionary_capped_at_output(&Method::Lzma, &lzma, 5).unwrap();
        assert_eq!(bounded[1..], 4096u32.to_le_bytes());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(10)]
    fn number_never_panics_and_fits_its_length() {
        let data: [u8; 9] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= data.len());
        let mut bytes = Bytes {
            data: &data[..length],
            at: 0,
        };
        if let Ok(value) = bytes.number() {
            let extra = data[0].leading_ones() as usize;
            assert_eq!(bytes.at, 1 + extra);
            assert!(extra == 8 || value >> (7 * (extra + 1)) == 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(11)]
    fn header_readers_never_panic() {
        let data: [u8; 5] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= data.len());
        let count: usize = kani::any();
        kani::assume(count <= 9);
        let mut bytes = Bytes {
            data: &data[..length],
            at: 0,
        };
        let _ = match kani::any::<u8>() % 4 {
            0 => bytes.count(kani::any::<u8>() as usize, "items").map(drop),
            1 => bytes.size().map(drop),
            2 => bytes.bits(count).map(drop),
            _ => bytes.u32().map(drop),
        };
        assert!(bytes.at <= length);
    }

    #[kani::proof]
    fn chunk_headers_stay_within_lzma2_limits() {
        let control: u8 = kani::any();
        if let Some(chunk) = chunk_header(control, kani::any()) {
            assert!(matches!(chunk.header, 3 | 5 | 6));
            assert!((1..=1 << 21).contains(&chunk.unpacked));
            assert!((1..=1 << 16).contains(&chunk.payload));
            assert_eq!(chunk.resets_dictionary, control == 1 || control >= 0xE0);
            assert_eq!(chunk.header == 6, control >= 0xC0);
        } else {
            assert!(control == 0 || (3..0x80).contains(&control));
        }
    }
}
