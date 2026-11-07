//! In-process extraction of 7z archives whose folders use Copy, LZMA, LZMA2,
//! Deflate and the 7-Zip branch and delta filters. See
//! `docs/reference/7z-reader.md` for what it accepts and restores.
use crate::xz_filters::{self, BRANCH_BUFFER, DELTA_HISTORY, DeltaDecoder, Filter, read_through};
use crate::{Entry, counters, progress::Tracker, safe_path, volumes::ArchiveFile};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, VecDeque},
    fs,
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
const MAX_CODERS: usize = 5;
const MAX_HEADER_LAYERS: usize = 4;
const COPY_BUFFER: usize = 1024 * 1024;
const SWAP_BUFFER: usize = 64 * 1024;
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
    /// Leading path segments to drop before writing, as [`crate::Strip`]
    /// describes. Selection still matches the full archive paths.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents, as [`crate::Metadata`]
    /// describes.
    pub metadata: crate::Metadata,
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
    bcj2_branches: Vec<Folder>,
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
    streams: Vec<(u64, u64)>,
    input_starts: Vec<u64>,
    output_starts: Vec<u64>,
    bind_pairs: Vec<(u64, u64)>,
    packed_inputs: Vec<u64>,
    outputs: usize,
    several_streams: Option<Vec<u8>>,
}

const BCJ2: [u8; 4] = [0x03, 0x03, 0x01, 0x1B];
const BCJ2_INPUTS: usize = 4;
const BCJ2_BRANCH_BUFFERS: u64 = 2 * COPY_BUFFER as u64;

#[derive(Debug, PartialEq, Eq)]
struct Bcj2Branch {
    coders: Vec<usize>,
    packed: usize,
    output: Option<usize>,
}

fn bcj2_plan(shape: &FolderShape) -> Parsed<Option<Vec<Bcj2Branch>>> {
    let joins: Vec<usize> = (0..shape.coders.len())
        .filter(|&index| shape.streams[index] != (1, 1))
        .collect();
    let [bcj2] = joins[..] else {
        return Ok(None);
    };
    if shape.coders[bcj2].id != BCJ2 || shape.streams[bcj2] != (BCJ2_INPUTS as u64, 1) {
        return Ok(None);
    }
    let (input_start, output_start) = (&shape.input_starts, &shape.output_starts);
    if shape
        .bind_pairs
        .iter()
        .any(|&(_, output)| output == output_start[bcj2])
    {
        return Ok(None);
    }
    let mut used = vec![false; shape.coders.len()];
    used[bcj2] = true;
    let mut branches = Vec::with_capacity(BCJ2_INPUTS);
    for branch_input in 0..BCJ2_INPUTS as u64 {
        let mut input = input_start[bcj2] + branch_input;
        let mut branch = Bcj2Branch {
            coders: Vec::new(),
            packed: 0,
            output: None,
        };
        loop {
            if let Some(packed) = shape
                .packed_inputs
                .iter()
                .position(|&packed| packed == input)
            {
                branch.packed = packed;
                break;
            }
            let &(_, output) = shape
                .bind_pairs
                .iter()
                .find(|&&(bound, _)| bound == input)
                .ok_or(Malformed {
                    message: "7z BCJ2 input is neither bound nor packed",
                    subject: "",
                })?;
            let coder = output_start
                .iter()
                .rposition(|&start| start <= output)
                .ok_or(Malformed {
                    message: "7z BCJ2 binds a stream it does not have",
                    subject: "",
                })?;
            check(!used[coder], "7z BCJ2 uses a coder twice", "")?;
            used[coder] = true;
            branch.output.get_or_insert(output as usize);
            branch.coders.push(coder);
            input = input_start[coder];
        }
        branches.push(branch);
    }
    check(
        used.iter().all(|&used| used),
        "7z BCJ2 folder has coders it does not use",
        "",
    )?;
    Ok(Some(branches))
}

const MAX_CODER_STREAMS: u64 = 32;

fn read_folder(bytes: &mut Bytes) -> Result<FolderShape> {
    let count = bytes.count(1, "coders")?;
    ensure!(
        (1..=MAX_CODERS).contains(&count),
        "7z folder has {count} coders; at most {MAX_CODERS} are supported"
    );
    let mut coders = Vec::with_capacity(count);
    let mut streams = Vec::with_capacity(count);
    let mut input_starts = Vec::with_capacity(count);
    let mut output_starts = Vec::with_capacity(count);
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
        // SPEC(7zFormat.txt Folder): BindPair and PackedStream indices count each coder's streams after those of the coders before it.
        input_starts.push(inputs);
        output_starts.push(outputs);
        inputs += coder_inputs;
        outputs += coder_outputs;
        streams.push((coder_inputs, coder_outputs));
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
        streams,
        input_starts,
        output_starts,
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
            let mut packed_sizes = Vec::with_capacity(shape.packed_inputs.len());
            for _ in &shape.packed_inputs {
                let size = *packs.next().context("7z folder has no packed stream")?;
                packed_sizes.push(size);
                pack_size = pack_size
                    .checked_add(size)
                    .context("7z packed streams overflow")?;
            }
            let mut bcj2_branches = Vec::new();
            let (output_to_packed, unsupported) = match (bcj2_plan(&shape)?, &shape.several_streams)
            {
                (Some(plan), _) => {
                    let mut offsets = Vec::with_capacity(packed_sizes.len());
                    let mut offset = pack_offset;
                    for size in &packed_sizes {
                        offsets.push(offset);
                        offset += size;
                    }
                    for branch in plan {
                        let coders = if branch.coders.is_empty() {
                            vec![Coder {
                                id: vec![0x00],
                                properties: Vec::new(),
                            }]
                        } else {
                            branch
                                .coders
                                .iter()
                                .map(|&index| shape.coders[index].clone())
                                .collect()
                        };
                        bcj2_branches.push(Folder {
                            output_to_packed: coders,
                            bcj2_branches: Vec::new(),
                            unsupported: None,
                            size: branch
                                .output
                                .map_or(packed_sizes[branch.packed], |output| sizes[output]),
                            crc: None,
                            pack_offset: offsets[branch.packed],
                            pack_size: packed_sizes[branch.packed],
                        });
                    }
                    (
                        vec![Coder {
                            id: BCJ2.to_vec(),
                            properties: Vec::new(),
                        }],
                        None,
                    )
                }
                (None, Some(id)) => (
                    Vec::new(),
                    Some(format!(
                        "7z coder {} has several streams in a layout that is not supported; {BACKEND_HINT}",
                        hex(id)
                    )),
                ),
                (None, None) => (
                    chain(shape.coders, &shape.bind_pairs, shape.packed_inputs[0])?,
                    None,
                ),
            };
            streams.folders.push(Folder {
                output_to_packed,
                bcj2_branches,
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

fn decode_header(file: &mut ArchiveFile, streams: &Streams) -> Result<Vec<u8>> {
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

pub(crate) const EXECUTABLE_MAGICS: [&[u8]; 8] = [
    b"MZ",
    b"\x7FELF",
    b"\xFE\xED\xFA\xCE",
    b"\xFE\xED\xFA\xCF",
    b"\xCE\xFA\xED\xFE",
    b"\xCF\xFA\xED\xFE",
    b"\xCA\xFE\xBA\xBE",
    b"\xCA\xFE\xBA\xBF",
];
// EXTERNAL(7-Zip): it finds a 7z signature after a stub only when the signature starts at offset 8 MiB or earlier.
const STUB_SEARCH_LIMIT: u64 = 8 << 20;
const STUB_SCAN_CHUNK: usize = 64 * 1024;
const ZIP_END_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
// EXTERNAL(Python zipfile): it looks for an end record in the last 64 KiB and 22 bytes.
const ZIP_TAIL: u64 = 22 + (1 << 16);

fn read_up_to(file: &ArchiveFile, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read_at(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

fn start_header_verifies(file: &ArchiveFile, at: u64) -> std::io::Result<bool> {
    let mut start = [0u8; SIGNATURE_HEADER as usize];
    Ok(read_up_to(file, &mut start, at)? == start.len()
        && &start[..6] == SIGNATURE
        && start[6] == 0
        && crc32fast::hash(&start[12..32]) == u32::from_le_bytes(start[8..12].try_into().unwrap()))
}

fn archive_start(file: &ArchiveFile) -> Result<Option<u64>> {
    let mut head = [0u8; 6];
    let read = read_up_to(file, &mut head, 0)?;
    let head = &head[..read];
    if head == SIGNATURE {
        return Ok(Some(0));
    }
    if !EXECUTABLE_MAGICS
        .iter()
        .any(|magic| head.starts_with(magic))
    {
        return Ok(None);
    }
    let scanned = file.len()?.min(STUB_SEARCH_LIMIT + SIGNATURE.len() as u64);
    let mut found = Vec::new();
    let mut chunk = vec![0u8; STUB_SCAN_CHUNK];
    let mut at = 0u64;
    while at < scanned && found.len() < 2 {
        let wanted =
            usize::try_from(scanned - at).map_or(chunk.len(), |left| left.min(chunk.len()));
        let read = read_up_to(file, &mut chunk[..wanted], at)?;
        let window = &chunk[..read];
        for candidate in (0..window.len()).filter(|&i| window[i..].starts_with(SIGNATURE)) {
            let start = at + candidate as u64;
            let new = found.last() != Some(&start);
            if new && start_header_verifies(file, start)? {
                found.push(start);
            }
        }
        if read < SIGNATURE.len() {
            break;
        }
        at += (read - (SIGNATURE.len() - 1)) as u64;
    }
    match found[..] {
        [] => Ok(None),
        [start] => accepted_stub(file, start),
        [first, second, ..] => bail!(
            "7z signatures at offset {first} and offset {second} after an executable stub both pass their start header check, so the file holds two 7z archives"
        ),
    }
}

fn accepted_stub(file: &ArchiveFile, base: u64) -> Result<Option<u64>> {
    let mut read_at = |buffer: &mut [u8], offset: u64| file.read_at(buffer, offset);
    match crate::volumes::check_stub(&mut read_at, base, false)? {
        crate::volumes::Stub::Accepted => Ok(Some(base)),
        crate::volumes::Stub::NotAtTheOverlay { .. } if zip_ends_the_file(file)? => Ok(None),
        refused => bail!(
            "the 7z archive at offset {base} follows {base} bytes that {}",
            refused.refusal().unwrap_or_default()
        ),
    }
}

fn zip_ends_the_file(file: &ArchiveFile) -> std::io::Result<bool> {
    let length = file.len()?;
    let from = length.saturating_sub(ZIP_TAIL);
    let mut tail = vec![0u8; usize::try_from(length.saturating_sub(from)).unwrap_or(0)];
    let read = read_up_to(file, &mut tail, from)?;
    Ok(tail[..read]
        .windows(4)
        .any(|window| window == ZIP_END_SIGNATURE))
}

fn ensure_no_zip_ends_the_file(file: &ArchiveFile, base: u64) -> Result<()> {
    ensure!(
        !zip_ends_the_file(file)?,
        "the file with a 7z archive at offset {base} after an executable stub has a ZIP end record in its last 64 KiB, so other readers may see a ZIP instead"
    );
    Ok(())
}

fn open(path: &Path) -> Result<(ArchiveFile, Archive)> {
    let file = crate::volumes::open(path)?;
    let base = archive_start(&file)?.unwrap_or(0);
    if base > 0 {
        ensure_no_zip_ends_the_file(&file, base)?;
    }
    let mut file = file.after(base)?;
    let length = file.len()?;
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
    let Some(header_start) = header_start else {
        bail!("7z header lies outside the file")
    };
    file.ensure_ends_at(header_start + size)?;
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
    Ppmd,
    Lzma,
    Lzma2,
    Swap(usize),
    Filter(FilterMethod),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterMethod {
    Branch(xz_filters::Branch),
    BranchWithStart(xz_filters::Branch),
    Delta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterSetting {
    Branch {
        branch: xz_filters::Branch,
        start: u32,
    },
    Delta {
        distance: usize,
    },
}

impl FilterSetting {
    fn buffer(self) -> u64 {
        match self {
            FilterSetting::Branch { .. } => BRANCH_BUFFER as u64,
            FilterSetting::Delta { .. } => DELTA_HISTORY as u64,
        }
    }

    fn filter(self) -> Filter {
        match self {
            FilterSetting::Branch { branch, start } => Filter::branch(branch, start),
            FilterSetting::Delta { distance } => Filter::Delta(DeltaDecoder::new(distance)),
        }
    }
}

// EXTERNAL(7-Zip 7zDecode.cpp, BranchMisc.cpp, DeltaFilter.cpp): a coder without a properties
// setter refuses any property byte, ARM64 and RISC-V take an aligned 4-byte start, and Delta
// takes one byte, the distance less one.
fn filter_setting(method: FilterMethod, properties: &[u8]) -> Option<FilterSetting> {
    match (method, properties) {
        (FilterMethod::Branch(branch), []) | (FilterMethod::BranchWithStart(branch), []) => {
            Some(FilterSetting::Branch { branch, start: 0 })
        }
        (FilterMethod::BranchWithStart(branch), &[a, b, c, d]) => {
            let start = u32::from_le_bytes([a, b, c, d]);
            start
                .is_multiple_of(branch.alignment())
                .then_some(FilterSetting::Branch { branch, start })
        }
        (FilterMethod::Delta, &[distance]) => Some(FilterSetting::Delta {
            distance: usize::from(distance) + 1,
        }),
        _ => None,
    }
}

fn method(coder: &Coder) -> Result<Method> {
    Ok(match coder.id.as_slice() {
        [0x00] => Method::Copy,
        [0x04, 0x01, 0x08] => Method::Deflate,
        [0x04, 0x01, 0x09] => Method::Deflate64,
        [0x04, 0x02, 0x02] => Method::Bzip2,
        [0x03, 0x04, 0x01] => Method::Ppmd,
        [0x03, 0x01, 0x01] => Method::Lzma,
        [0x21] => Method::Lzma2,
        [0x02, 0x03, 0x02] => Method::Swap(2),
        [0x02, 0x03, 0x04] => Method::Swap(4),
        [0x03] => Method::Filter(FilterMethod::Delta),
        [0x03, 0x03, 0x01, 0x03] => Method::Filter(FilterMethod::Branch(xz_filters::Branch::X86)),
        [0x03, 0x03, 0x02, 0x05] => {
            Method::Filter(FilterMethod::Branch(xz_filters::Branch::PowerPc))
        }
        [0x03, 0x03, 0x04, 0x01] => Method::Filter(FilterMethod::Branch(xz_filters::Branch::Ia64)),
        [0x03, 0x03, 0x05, 0x01] => Method::Filter(FilterMethod::Branch(xz_filters::Branch::Arm)),
        [0x03, 0x03, 0x07, 0x01] => {
            Method::Filter(FilterMethod::Branch(xz_filters::Branch::ArmThumb))
        }
        [0x03, 0x03, 0x08, 0x05] => Method::Filter(FilterMethod::Branch(xz_filters::Branch::Sparc)),
        [0x0A] => Method::Filter(FilterMethod::BranchWithStart(xz_filters::Branch::Arm64)),
        [0x0B] => Method::Filter(FilterMethod::BranchWithStart(xz_filters::Branch::RiscV)),
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
    if !folder.bcj2_branches.is_empty() {
        for branch in &folder.bcj2_branches {
            check_methods(branch)?;
        }
        let memory = decoder_memory(folder, false);
        if memory > DECODER_MEMORY {
            return Err(unsupported(format!(
                "7z BCJ2 decoders need {memory} bytes, more than the {DECODER_MEMORY}-byte decoder budget; {BACKEND_HINT}"
            )));
        }
        return Ok(Vec::new());
    }
    if let Some((_, swapped)) = byte_swapped(folder)? {
        return check_methods(&swapped);
    }
    let methods = folder
        .output_to_packed
        .iter()
        .map(method)
        .collect::<Result<Vec<_>>>()?;
    match methods.as_slice() {
        [Method::Copy] | [Method::Deflate] | [Method::Deflate64] | [Method::Bzip2] => {}
        [Method::Ppmd] => {
            ppmd_parameters(&folder.output_to_packed[0].properties)?;
        }
        [filters @ .., Method::Lzma | Method::Lzma2]
            if filters
                .iter()
                .all(|method| matches!(method, Method::Filter(_))) =>
        {
            let (filter_coders, last) = folder.output_to_packed.split_at(filters.len());
            filter_settings(filter_coders)?;
            lzma_coder(&last[0], folder.size)?;
        }
        _ => {
            return Err(unsupported(format!(
                "7z folder combines methods in a way that is not supported; {BACKEND_HINT}"
            )));
        }
    }
    Ok(methods)
}

fn filter_settings(coders: &[Coder]) -> Result<Vec<FilterSetting>> {
    coders
        .iter()
        .map(|coder| {
            let Method::Filter(filter) = method(coder)? else {
                return Err(unsupported(format!(
                    "7z folder combines methods in a way that is not supported; {BACKEND_HINT}"
                )));
            };
            filter_setting(filter, &coder.properties).ok_or_else(|| {
                unsupported(format!(
                    "7z method {} has properties 7-Zip does not accept; {BACKEND_HINT}",
                    hex(&coder.id)
                ))
            })
        })
        .collect()
}

enum LzmaCoder {
    Lzma {
        properties: crate::lzma::Properties,
        dictionary: u32,
    },
    Lzma2 {
        dictionary: u32,
    },
}

impl LzmaCoder {
    fn memory(&self, output: u64) -> u64 {
        match *self {
            LzmaCoder::Lzma {
                properties,
                dictionary,
            } => crate::lzma::lzma_memory(properties, dictionary, output),
            LzmaCoder::Lzma2 { dictionary } => crate::lzma::lzma2_memory(dictionary),
        }
    }

    fn reader<R: Read + Send + 'static>(&self, input: R, output: u64) -> Box<dyn Read + Send> {
        match *self {
            LzmaCoder::Lzma {
                properties,
                dictionary,
            } => Box::new(Decoding(
                // EXTERNAL(7-Zip LzmaDec.c): 7z extraction finishes LZMA at the folder size, so a match may not cross it, and a range coder left unclean there must decode an end marker.
                crate::lzma::RawLzmaReader::new(input, properties, dictionary, output)
                    .requiring_a_clean_end(),
            )),
            LzmaCoder::Lzma2 { dictionary } => Box::new(Decoding(
                // EXTERNAL(7-Zip Lzma2Decoder.cpp): 7z accepts LZMA2 whose packed stream ends after a whole chunk, without the end marker.
                crate::lzma::Lzma2Reader::new(input, dictionary).ending_at_the_input_end(),
            )),
        }
    }
}

fn lzma_coder(coder: &Coder, output: u64) -> Result<LzmaCoder> {
    match method(coder)? {
        Method::Lzma => {
            let Ok([properties_byte, dictionary @ ..]) =
                <[u8; 5]>::try_from(coder.properties.as_slice())
            else {
                bail!("7z LZMA properties are malformed");
            };
            let properties =
                crate::lzma::Properties::from_byte(properties_byte).ok_or_else(|| {
                    unsupported(format!(
                        "7z LZMA properties byte {properties_byte} is out of range; {BACKEND_HINT}"
                    ))
                })?;
            Ok(LzmaCoder::Lzma {
                properties,
                dictionary: u32::from_le_bytes(dictionary),
            })
        }
        Method::Lzma2 => {
            let &[property] = coder.properties.as_slice() else {
                bail!("7z LZMA2 properties are malformed");
            };
            ensure!(property <= 40, "7z LZMA2 properties are malformed");
            Ok(LzmaCoder::Lzma2 {
                dictionary: lzma2_dictionary_within(property, output),
            })
        }
        _ => Err(unsupported(format!(
            "7z folder combines methods in a way that is not supported; {BACKEND_HINT}"
        ))),
    }
}

fn lzma2_dictionary(property: u8) -> u64 {
    if property >= 40 {
        u64::from(u32::MAX)
    } else {
        u64::from(2 | (property & 1)) << (property / 2 + 11)
    }
}

fn lzma2_dictionary_within(property: u8, output: u64) -> u32 {
    lzma2_dictionary(property).min(output.max(4096)) as u32
}

struct Decoding<R>(R);

impl<R: Read> Read for Decoding<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        counters::time(counters::Counter::DecodeNanos, || self.0.read(buffer))
    }
}

fn ppmd_parameters(properties: &[u8]) -> Result<(u32, u32)> {
    // SPEC(7-Zip PpmdDecoder.cpp): one order byte, then the model size as a little-endian u32.
    let Some((order, memory)) = ppmd7_fields(properties) else {
        return Err(unsupported(format!(
            "7z PPMd properties are not 5 bytes; {BACKEND_HINT}"
        )));
    };
    check(
        (ppmd_rust::PPMD7_MIN_ORDER..=ppmd_rust::PPMD7_MAX_ORDER).contains(&order)
            && (ppmd_rust::PPMD7_MIN_MEM_SIZE..=ppmd_rust::PPMD7_MAX_MEM_SIZE).contains(&memory),
        "7z PPMd properties are out of range",
        "",
    )?;
    if u64::from(memory) > DECODER_MEMORY {
        return Err(unsupported(format!(
            "7z PPMd model of {memory} bytes exceeds the {DECODER_MEMORY}-byte decoder budget; {BACKEND_HINT}"
        )));
    }
    Ok((order, memory))
}

fn ppmd7_fields(properties: &[u8]) -> Option<(u32, u32)> {
    let [order, a, b, c, d] = properties[..] else {
        return None;
    };
    Some((u32::from(order), u32::from_le_bytes([a, b, c, d])))
}

fn decoder_memory(folder: &Folder, segments_possible: bool) -> u64 {
    if !folder.bcj2_branches.is_empty() {
        return folder
            .bcj2_branches
            .iter()
            .map(|branch| decoder_memory(branch, false) + BCJ2_BRANCH_BUFFERS)
            .fold(0, u64::saturating_add);
    }
    if let Ok(Some((_, swapped))) = byte_swapped(folder) {
        return decoder_memory(&swapped, segments_possible);
    }
    let filter_coders = &folder.output_to_packed[..folder.output_to_packed.len().saturating_sub(1)];
    let filter_buffers = filter_settings(filter_coders).map_or(0, |settings| {
        settings.iter().map(|setting| setting.buffer()).sum()
    });
    let sequential = match folder
        .output_to_packed
        .last()
        .map(|coder| (method(coder), coder))
    {
        Some((Ok(Method::Lzma | Method::Lzma2), coder)) => lzma_coder(coder, folder.size)
            .map_or(0, |coder| coder.memory(folder.size) + filter_buffers),
        Some((Ok(Method::Bzip2), _)) => BZIP2_DECODER_MEMORY,
        Some((Ok(Method::Ppmd), coder)) => {
            ppmd_parameters(&coder.properties).map_or(0, |(_, memory)| u64::from(memory))
        }
        _ => 0,
    }
    .max(64 * 1024);
    let lzma2_only = matches!(
        folder.output_to_packed.as_slice(),
        [coder] if coder.id == [0x21]
    );
    if segments_possible && lzma2_only && folder.size >= PARALLEL_FOLDER_MIN {
        return sequential.max(SEGMENT_BUDGET);
    }
    sequential
}

struct PpmdFolder<R: Read> {
    decoder: ppmd_rust::Ppmd7Decoder<crate::EndWatched<std::io::Take<R>>>,
    remaining: u64,
}

impl<R: Read> Read for PpmdFolder<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            let input = self.decoder.get_ref();
            if input.ran_out || input.inner.limit() > 0 {
                return Err(std::io::Error::other(
                    "7z PPMd stream does not end at its packed size",
                ));
            }
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.decoder.read(&mut buffer[..wanted])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

struct ReadAt {
    file: ArchiveFile,
    position: u64,
}

impl Read for ReadAt {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.position)?;
        self.position += read as u64;
        Ok(read)
    }
}

fn byte_swapped(folder: &Folder) -> Result<Option<(usize, Folder)>> {
    let Some((first, rest)) = folder.output_to_packed.split_first() else {
        return Ok(None);
    };
    let Method::Swap(width) = method(first)? else {
        return Ok(None);
    };
    if !first.properties.is_empty() {
        return Err(unsupported(format!(
            "7z method {} has properties, which 7-Zip does not accept; {BACKEND_HINT}",
            hex(&first.id)
        )));
    }
    let output_to_packed = if rest.is_empty() {
        vec![Coder {
            id: vec![0x00],
            properties: Vec::new(),
        }]
    } else {
        rest.to_vec()
    };
    Ok(Some((
        width,
        Folder {
            output_to_packed,
            ..folder.clone()
        },
    )))
}

// SPEC(7-Zip ByteSwap.cpp): each whole group of `width` bytes is reversed, and a shorter tail passes unchanged.
struct ByteSwap<R> {
    inner: R,
    width: usize,
    buffer: Vec<u8>,
    start: usize,
    swapped: usize,
    end: usize,
    ended: bool,
}

impl<R: Read> ByteSwap<R> {
    fn new(inner: R, width: usize, capacity: usize) -> Self {
        ByteSwap {
            inner,
            width,
            buffer: vec![0; capacity.max(width)],
            start: 0,
            swapped: 0,
            end: 0,
            ended: false,
        }
    }
}

impl<R: Read> Read for ByteSwap<R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.start < self.swapped {
                let count = output.len().min(self.swapped - self.start);
                output[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
                self.start += count;
                return Ok(count);
            }
            if self.ended {
                if self.swapped == self.end {
                    return Ok(0);
                }
                self.swapped = self.end;
                continue;
            }
            self.buffer.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            let read = self.inner.read(&mut self.buffer[self.end..])?;
            if read == 0 {
                self.ended = true;
                self.swapped = 0;
                continue;
            }
            self.end += read;
            self.swapped = self.end - self.end % self.width;
            for group in self.buffer[..self.swapped].chunks_exact_mut(self.width) {
                group.reverse();
            }
        }
    }
}

struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "7z BCJ2 input stream ends before its size",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

type Branch = Exact<Box<dyn Read + Send>>;

struct Bcj2Folder {
    decoder: Option<crate::bcj2::Bcj2<Branch, Branch, Branch, Branch>>,
}

impl Read for Bcj2Folder {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let Some(decoder) = &mut self.decoder else {
            return Ok(0);
        };
        let read = decoder.read(buffer)?;
        if read == 0 && !buffer.is_empty() {
            let (main, calls, jumps, decisions) = self.decoder.take().unwrap().into_streams();
            let mut used_up = true;
            for mut stream in [main, calls, jumps, decisions] {
                used_up &= stream.remaining == 0 && stream.inner.read(&mut [0])? == 0;
            }
            if !used_up {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "7z BCJ2 input streams hold bytes the output does not use",
                ));
            }
        }
        Ok(read)
    }
}

fn bcj2_reader(file: &ArchiveFile, folder: &Folder) -> Result<Box<dyn Read + Send>> {
    let mut branches = Vec::with_capacity(BCJ2_INPUTS);
    for branch in &folder.bcj2_branches {
        branches.push(Exact {
            inner: folder_reader(file.try_clone()?, branch, None)?,
            remaining: branch.size,
        });
    }
    let [main, calls, jumps, decisions]: [Branch; BCJ2_INPUTS] = branches
        .try_into()
        .map_err(|_| anyhow::anyhow!("7z BCJ2 folder does not have four inputs"))?;
    Ok(Box::new(Bcj2Folder {
        decoder: Some(crate::bcj2::Bcj2::new(
            main,
            calls,
            jumps,
            decisions,
            folder.size,
        )),
    }))
}

fn folder_reader(
    mut file: ArchiveFile,
    folder: &Folder,
    segment_threads: Option<(&Path, usize)>,
) -> Result<Box<dyn Read + Send>> {
    let methods = check_methods(folder)?;
    if !folder.bcj2_branches.is_empty() {
        return bcj2_reader(&file, folder);
    }
    if let Some((width, swapped)) = byte_swapped(folder)? {
        return Ok(Box::new(ByteSwap::new(
            folder_reader(file, &swapped, segment_threads)?,
            width,
            SWAP_BUFFER,
        )));
    }
    if let ([Method::Lzma2], [coder]) = (methods.as_slice(), folder.output_to_packed.as_slice())
        && let Some((path, window)) = segment_threads.filter(|&(_, window)| window > 1)
        && folder.size >= PARALLEL_FOLDER_MIN
        && let Some(&properties) = coder.properties.first()
        && let Some(segments) =
            independent_lzma2_segments(&mut file, folder, properties, SEGMENT_MIN_OUTPUT)?
    {
        return Ok(Box::new(ParallelLzma2 {
            path: path.to_owned(),
            input: file,
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
    let packed = ReadAt {
        file,
        position: SIGNATURE_HEADER + folder.pack_offset,
    };
    if let [filters @ .., Method::Lzma | Method::Lzma2] = methods.as_slice() {
        let (filter_coders, last) = folder.output_to_packed.split_at(filters.len());
        let decoded =
            lzma_coder(&last[0], folder.size)?.reader(packed.take(folder.pack_size), folder.size);
        if filters.is_empty() {
            return Ok(decoded);
        }
        return filtered(filter_coders, decoded);
    }
    let packed = BufReader::with_capacity(COPY_BUFFER, packed).take(folder.pack_size);
    Ok(match methods.as_slice() {
        [Method::Copy] => Box::new(packed),
        [Method::Deflate] => Box::new(flate2::read::DeflateDecoder::new(packed)),
        [Method::Deflate64] => Box::new(deflate64::Deflate64Decoder::with_buffer(packed)),
        [Method::Bzip2] => {
            crate::native_tar::decode(packed, crate::native_tar::Compression::Bzip2)?
        }
        [Method::Ppmd] => {
            let (order, memory) = ppmd_parameters(&folder.output_to_packed[0].properties)?;
            let packed = crate::EndWatched {
                inner: packed,
                ran_out: false,
            };
            let decoder = ppmd_rust::Ppmd7Decoder::new(packed, order, memory)
                .map_err(|error| anyhow::anyhow!("cannot start the 7z PPMd decoder: {error:?}"))?;
            Box::new(PpmdFolder {
                decoder,
                remaining: folder.size,
            })
        }
        _ => bail!("7z folder combines methods in a way that is not supported"),
    })
}

fn filtered(coders: &[Coder], decoded: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>> {
    let filters = filter_settings(coders)?
        .into_iter()
        .map(FilterSetting::filter)
        .collect();
    Ok(Box::new(FilterChain { filters, decoded }))
}

struct FilterChain {
    filters: Vec<Filter>,
    decoded: Box<dyn Read + Send>,
}

impl Read for FilterChain {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        read_through(&mut self.filters, &mut self.decoded, output)
    }
}

struct Segment {
    pack_offset: u64,
    pack_length: u64,
    output: u64,
}

impl Segment {
    fn held(&self, properties: u8) -> u64 {
        self.pack_length
            + self.output
            + crate::lzma::lzma2_memory(lzma2_dictionary_within(properties, self.output))
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
    input: ArchiveFile,
    pack_start: u64,
    properties: u8,
    segments: Vec<Segment>,
    window: usize,
    stop: Arc<AtomicBool>,
    next: usize,
    queue: VecDeque<SegmentThread>,
    held: u64,
    current_held: u64,
    current: Vec<u8>,
    position: usize,
}

struct SegmentThread {
    held: u64,
    decoded: Receiver<std::io::Result<Vec<u8>>>,
    thread: std::thread::JoinHandle<()>,
}

impl SegmentThread {
    fn finish(self) -> std::io::Result<Vec<u8>> {
        let decoded = self.decoded.recv();
        self.thread
            .join()
            .map_err(|_| std::io::Error::other("a 7z segment decoder panicked"))?;
        decoded.map_err(|_| std::io::Error::other("a 7z segment decoder stopped"))?
    }
}

impl ParallelLzma2 {
    fn start_segments(&mut self) -> std::io::Result<()> {
        while let Some(segment) = self.segments.get(self.next)
            && self.queue.len() < self.window
            && self.held + segment.held(self.properties) <= SEGMENT_BUDGET
        {
            let (sender, receiver) = sync_channel(1);
            let input = self.input.reopen(&self.path)?;
            let stop = Arc::clone(&self.stop);
            let start = self.pack_start + segment.pack_offset;
            let (length, output, properties) =
                (segment.pack_length, segment.output, self.properties);
            let spawned = std::thread::Builder::new().spawn(move || {
                let decoded = decode_segment(input, start, length, output, properties, &stop);
                let _ = sender.send(decoded);
            });
            let thread = match spawned {
                Ok(thread) => thread,
                Err(error) if self.queue.is_empty() => return Err(error),
                Err(_) => break,
            };
            self.queue.push_back(SegmentThread {
                held: segment.held(self.properties),
                decoded: receiver,
                thread,
            });
            self.held += segment.held(self.properties);
            self.next += 1;
        }
        Ok(())
    }
}

impl Drop for ParallelLzma2 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for segment in self.queue.drain(..) {
            let _ = segment.thread.join();
        }
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
            let Some(segment) = self.queue.pop_front() else {
                return Ok(0);
            };
            self.current_held = segment.held;
            self.current = segment.finish()?;
        }
        let length = buffer.len().min(self.current.len() - self.position);
        buffer[..length].copy_from_slice(&self.current[self.position..self.position + length]);
        self.position += length;
        Ok(length)
    }
}

fn decode_segment(
    mut file: ArchiveFile,
    start: u64,
    length: u64,
    output: u64,
    properties: u8,
    stop: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let invalid =
        |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned());
    let too_large = || invalid("7z segment is too large");
    file.seek(SeekFrom::Start(start))?;
    let mut packed = vec![0u8; usize::try_from(length).map_err(|_| too_large())?];
    file.read_exact(&mut packed)?;
    packed.push(0);
    let mut decoder = Decoding(crate::lzma::Lzma2Reader::new(
        packed.as_slice(),
        lzma2_dictionary_within(properties, output),
    ));
    let mut decoded = vec![0u8; usize::try_from(output).map_err(|_| too_large())?];
    let mut written = 0;
    while written < decoded.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("7z segment decoding stopped"));
        }
        let end = (written + SEGMENT_DECODE_STEP).min(decoded.len());
        while written < end {
            match decoder.read(&mut decoded[written..end])? {
                0 => return Err(invalid("7z LZMA2 segment ends before its declared size")),
                read => written += read,
            }
        }
    }
    if decoder.read(&mut [0])? != 0 {
        return Err(invalid("7z LZMA2 segment decodes past its declared size"));
    }
    let (unread, held) = decoder.0.into_input();
    if !unread.is_empty() || !held.is_empty() {
        return Err(invalid("7z LZMA2 segment ends before its packed bytes"));
    }
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
    let (file, mut archive) = open(path)?;
    for folder in &archive.folders {
        check_methods(folder)?;
    }
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    selection.ensure_each_path_matches(archive.items.iter().map(|item| item.name.as_str()))?;
    let placement = crate::placement::Placement::new(options.strip)?;
    placement.ensure_prefix_is_a_folder(
        archive
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.kind == Kind::Directory)),
    )?;
    let prefix_selected = archive
        .items
        .iter()
        .any(|item| placement.is_prefix(&item.name) && selection.selects(&item.name));
    let mut chosen: Vec<bool> = archive
        .items
        .iter()
        .map(|item| selection.selects(&item.name) && placement.place(&item.name).is_some())
        .collect();
    if placement.strips() {
        placement.ensure_something_placed(chosen.contains(&true) || prefix_selected)?;
        for (item, chosen) in archive.items.iter_mut().zip(&mut chosen) {
            if *chosen {
                item.name = placement.place(&item.name).expect("placed").to_owned();
            }
        }
        crate::placement::ensure_placed_names_fit(
            archive
                .items
                .iter()
                .zip(&chosen)
                .filter(|(_, chosen)| **chosen)
                .map(|(item, _)| (item.name.as_str(), item.kind == Kind::Directory)),
        )?;
    }
    let metadata = options.metadata;
    for item in &mut archive.items {
        item.mode = if item.kind == Kind::Directory {
            metadata.directory_mode(item.mode)
        } else {
            metadata.file_mode(item.mode)
        };
        item.windows_attributes = metadata.windows_attributes(item.windows_attributes);
        item.modified = metadata.time(item.modified);
    }
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
            let staging =
                crate::staging::stage(path, destination, options.existing, options.metadata)?;
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
        let mut reader = folder_reader(file.reopen(path)?, folder, Some((path, segment_threads)))?;
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
            let mut held = 0u64;
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
                    tracker.add_bytes(std::mem::replace(&mut held, read as u64))?;
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
                tracker.add_bytes(held)?;
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
        let workers: Vec<_> = (0..threads)
            .map(|_| {
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
                })
            })
            .collect();
        for worker in workers {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
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
            drop(file);
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

/// Whether the file starts with the 7z signature, or starts with a PE, ELF or
/// Mach-O header and holds one 7z start header, with version byte 0 and a
/// matching CRC, at offset 8 MiB or earlier. The stub before it must pass the
/// rules in `docs/reference/7z-reader.md`, and the 7z must start where the
/// executable's own data ends. Returns an error when two start headers both
/// pass, or when the stub is refused, and `false` when the 7z does not start
/// there but a ZIP end record ends the file, so that the file is read as a
/// ZIP.
pub fn sniff(path: &Path) -> Result<bool> {
    Ok(archive_start(&crate::volumes::open(path)?)?.is_some())
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
            bcj2_branches: Vec::new(),
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
        assert_eq!(
            segment.held(40),
            10 + (1 << 20) + crate::lzma::lzma2_memory(1 << 20)
        );
        assert!(crate::lzma::lzma2_memory(1 << 20) > 1 << 20);
        let (_, folder) = uncompressed_lzma2(400, 65_536);
        assert_eq!(decoder_memory(&folder, true), SEGMENT_BUDGET);
        assert!(decoder_memory(&folder, false) < SEGMENT_BUDGET);
        let large_dictionary = Folder {
            output_to_packed: vec![Coder {
                id: vec![0x21],
                properties: vec![40],
            }],
            size: 1 << 30,
            ..folder
        };
        let sequential = decoder_memory(&large_dictionary, false);
        assert!(sequential > 1 << 30);
        assert_eq!(decoder_memory(&large_dictionary, true), sequential);
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
                            bcj2_branches: Vec::new(),
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
        assert_eq!(lzma2_dictionary_within(40, 100), 4096);
        assert_eq!(lzma2_dictionary_within(40, 1 << 40), u32::MAX);
        assert_eq!(lzma2_dictionary_within(16, 1 << 30), 1 << 20);
        assert_eq!(lzma2_dictionary_within(18, 300_000), 300_000);
        let mut lzma = vec![0x5D];
        lzma.extend_from_slice(&u32::MAX.to_le_bytes());
        let coder = Coder {
            id: vec![0x03, 0x01, 0x01],
            properties: lzma,
        };
        let memory = lzma_coder(&coder, 5).unwrap().memory(5);
        assert!(memory < 1 << 20, "{memory}");
    }

    #[test]
    fn decoder_memory_counts_the_window_and_the_decoder_buffers() {
        let (_, mut folder) = uncompressed_lzma2(400, 65_536);
        let window = lzma2_dictionary(18);
        assert!(decoder_memory(&folder, false) > window);
        folder.output_to_packed[0] = Coder {
            id: vec![0x03, 0x01, 0x01],
            properties: [vec![134], (1u32 << 20).to_le_bytes().to_vec()].concat(),
        };
        let wide = decoder_memory(&folder, false);
        folder.output_to_packed[0].properties[0] = 0x5D;
        let narrow = decoder_memory(&folder, false);
        assert!(narrow > 1 << 20);
        assert!(
            wide >= narrow + ((0x400 << 12) - (0x400 << 3)) * 2,
            "{wide} {narrow}"
        );
        folder.output_to_packed.insert(
            0,
            Coder {
                id: vec![0x03, 0x03, 0x01, 0x03],
                properties: Vec::new(),
            },
        );
        assert_eq!(decoder_memory(&folder, false), narrow + (1 << 16));
        folder.output_to_packed.insert(
            0,
            Coder {
                id: vec![0x03],
                properties: vec![0],
            },
        );
        assert_eq!(decoder_memory(&folder, false), narrow + (1 << 16) + 256);
        folder.output_to_packed.insert(
            0,
            Coder {
                id: vec![0x0A],
                properties: vec![0x00, 0x10, 0x00, 0x00],
            },
        );
        assert_eq!(decoder_memory(&folder, false), narrow + (2 << 16) + 256);
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
                let status = counters::time(counters::Counter::DecodeNanos, || {
                    self.stream.process(
                        &self.buffer[self.start..self.end],
                        output,
                        liblzma::stream::Action::Run,
                    )
                })
                .map_err(std::io::Error::other)?;
                self.start += (self.stream.total_in() - read) as usize;
                let produced = (self.stream.total_out() - written) as usize;
                if produced > 0 || status == liblzma::stream::Status::StreamEnd {
                    return Ok(produced);
                }
                if self.input_done {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "7z folder has a truncated LZMA stream",
                    ));
                }
            }
        }
    }

    fn raw_decoder<R: Read>(filters: &liblzma::stream::Filters, input: R) -> Result<LzmaReader<R>> {
        Ok(LzmaReader {
            stream: liblzma::stream::Stream::new_raw_decoder(filters)
                .context("cannot start the 7z decoder")?,
            input,
            buffer: vec![0; COPY_BUFFER],
            start: 0,
            end: 0,
            input_done: false,
        })
    }

    const STORED_CHUNK: usize = 1 << 16;

    // EXTERNAL(liblzma lzma2_decoder.c): control 1 resets the dictionary and stores up to 64 KiB, its size less one big-endian; control 0 ends the stream.
    struct StoredLzma2<R> {
        inner: R,
        chunk: Vec<u8>,
        start: usize,
        finished: bool,
    }

    impl<R: Read> StoredLzma2<R> {
        fn new(inner: R) -> Self {
            StoredLzma2 {
                inner,
                chunk: Vec::with_capacity(3 + STORED_CHUNK),
                start: 0,
                finished: false,
            }
        }

        fn fill(&mut self) -> std::io::Result<()> {
            self.chunk.clear();
            self.start = 0;
            self.chunk.extend_from_slice(&[1, 0, 0]);
            let mut stored = 0;
            while stored < STORED_CHUNK {
                self.chunk.resize(3 + STORED_CHUNK, 0);
                let read = self.inner.read(&mut self.chunk[3 + stored..])?;
                if read == 0 {
                    break;
                }
                stored += read;
            }
            self.chunk.truncate(3 + stored);
            if stored == 0 {
                self.chunk = vec![0];
                self.finished = true;
            } else {
                self.chunk[1..3].copy_from_slice(&((stored - 1) as u16).to_be_bytes());
            }
            Ok(())
        }
    }

    impl<R: Read> Read for StoredLzma2<R> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.start == self.chunk.len() {
                if self.finished {
                    return Ok(0);
                }
                self.fill()?;
            }
            let count = output.len().min(self.chunk.len() - self.start);
            output[..count].copy_from_slice(&self.chunk[self.start..self.start + count]);
            self.start += count;
            Ok(count)
        }
    }

    fn liblzma_dictionary_capped_at_output(
        method: &Method,
        properties: &[u8],
        output: u64,
    ) -> Vec<u8> {
        let needed = output.max(4096);
        match method {
            Method::Lzma => {
                let declared = u64::from(u32::from_le_bytes(properties[1..5].try_into().unwrap()));
                let mut bounded = properties.to_vec();
                bounded[1..5].copy_from_slice(&(declared.min(needed) as u32).to_le_bytes());
                bounded
            }
            Method::Lzma2 => {
                let declared = lzma2_dictionary(properties[0]);
                let smallest = (0..=properties[0])
                    .find(|&property| lzma2_dictionary(property) >= declared.min(needed))
                    .unwrap_or(properties[0]);
                vec![smallest]
            }
            _ => properties.to_vec(),
        }
    }

    fn liblzma_filters(folder: &Folder, methods: &[Method]) -> Result<liblzma::stream::Filters> {
        let mut filters = liblzma::stream::Filters::new();
        for (coder, method) in folder.output_to_packed.iter().zip(methods) {
            ensure!(
                match method {
                    Method::Lzma => coder.properties.len() == 5,
                    Method::Lzma2 => coder.properties.len() == 1 && coder.properties[0] <= 40,
                    _ => true,
                },
                "malformed properties"
            );
            let properties =
                liblzma_dictionary_capped_at_output(method, &coder.properties, folder.size);
            match method {
                Method::Filter(_) => {
                    liblzma_filter(&mut filters, filter_name(&coder.id), &properties)
                }
                Method::Lzma => filters.lzma1_properties(&properties).map(drop),
                Method::Lzma2 => filters.lzma2_properties(&properties).map(drop),
                _ => bail!("not an LZMA folder"),
            }?;
        }
        Ok(filters)
    }

    fn liblzma_folder_reader(file: ArchiveFile, folder: &Folder) -> Result<Box<dyn Read + Send>> {
        let methods = folder
            .output_to_packed
            .iter()
            .map(method)
            .collect::<Result<Vec<_>>>()?;
        let packed = BufReader::with_capacity(
            COPY_BUFFER,
            ReadAt {
                file,
                position: SIGNATURE_HEADER + folder.pack_offset,
            },
        )
        .take(folder.pack_size);
        Ok(match methods.as_slice() {
            [filters @ .., Method::Lzma] if !filters.is_empty() => {
                let (filter_coders, lzma) = folder.output_to_packed.split_at(filters.len());
                let lzma_only = Folder {
                    output_to_packed: lzma.to_vec(),
                    ..folder.clone()
                };
                let decoded = raw_decoder(&liblzma_filters(&lzma_only, &[Method::Lzma])?, packed)?
                    .take(folder.size);
                let mut chain = liblzma::stream::Filters::new();
                for coder in filter_coders {
                    liblzma_filter(&mut chain, filter_name(&coder.id), &coder.properties)?;
                }
                chain.lzma2_properties(&[0])?;
                Box::new(raw_decoder(&chain, StoredLzma2::new(decoded))?)
            }
            [Method::Lzma] => Box::new(
                raw_decoder(&liblzma_filters(folder, &methods)?, packed)?.take(folder.size),
            ),
            _ => Box::new(LiblzmaToItsEnd {
                stream: liblzma::stream::Stream::new_raw_decoder(&liblzma_filters(
                    folder, &methods,
                )?)?,
                input: packed.chain(&[0u8][..]),
                buffer: vec![0; COPY_BUFFER],
                start: 0,
                end: 0,
                ended: false,
            }),
        })
    }

    struct LiblzmaToItsEnd<R> {
        stream: liblzma::stream::Stream,
        input: R,
        buffer: Vec<u8>,
        start: usize,
        end: usize,
        ended: bool,
    }

    impl<R: Read> Read for LiblzmaToItsEnd<R> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            while !self.ended {
                if self.start == self.end {
                    self.end = self.input.read(&mut self.buffer)?;
                    self.start = 0;
                }
                let (read, written) = (self.stream.total_in(), self.stream.total_out());
                let status = self
                    .stream
                    .process(
                        &self.buffer[self.start..self.end],
                        output,
                        liblzma::stream::Action::Run,
                    )
                    .map_err(std::io::Error::other)?;
                self.start += (self.stream.total_in() - read) as usize;
                let produced = (self.stream.total_out() - written) as usize;
                self.ended = status == liblzma::stream::Status::StreamEnd;
                if produced > 0 {
                    return Ok(produced);
                }
                if self.stream.total_in() == read && self.end == 0 {
                    return Err(std::io::Error::other("liblzma stream ends early"));
                }
            }
            Ok(0)
        }
    }

    fn liblzma_decode_segment(
        path: &Path,
        start: u64,
        length: u64,
        output: u64,
        properties: u8,
    ) -> std::io::Result<Vec<u8>> {
        let invalid = |message: &str| std::io::Error::other(message.to_owned());
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut packed = vec![0u8; length as usize];
        file.read_exact(&mut packed)?;
        packed.push(0);
        let capped = liblzma_dictionary_capped_at_output(&Method::Lzma2, &[properties], output);
        let mut filters = liblzma::stream::Filters::new();
        filters
            .lzma2_properties(&capped)
            .map_err(std::io::Error::other)?;
        let mut stream =
            liblzma::stream::Stream::new_raw_decoder(&filters).map_err(std::io::Error::other)?;
        let output = output as usize;
        let mut decoded = vec![0u8; output + 1];
        let (mut read, mut written) = (0usize, 0usize);
        loop {
            let (before_in, before_out) = (stream.total_in(), stream.total_out());
            let status = stream
                .process(
                    &packed[read..],
                    &mut decoded[written..],
                    liblzma::stream::Action::Finish,
                )
                .map_err(std::io::Error::other)?;
            read += (stream.total_in() - before_in) as usize;
            written += (stream.total_out() - before_out) as usize;
            if written > output {
                return Err(invalid("past its size"));
            }
            if status == liblzma::stream::Status::StreamEnd {
                break;
            }
            if stream.total_in() == before_in && stream.total_out() == before_out {
                return Err(invalid("truncated"));
            }
        }
        if written != output || read != packed.len() {
            return Err(invalid("wrong size"));
        }
        decoded.truncate(output);
        Ok(decoded)
    }

    fn lzma_folders(folder: &Folder, found: &mut Vec<Folder>) {
        for branch in &folder.bcj2_branches {
            lzma_folders(branch, found);
        }
        if !folder.bcj2_branches.is_empty() {
            return;
        }
        if let Ok(Some((_, swapped))) = byte_swapped(folder) {
            lzma_folders(&swapped, found);
            return;
        }
        if let Some(Ok(Method::Lzma | Method::Lzma2)) = folder.output_to_packed.last().map(method) {
            found.push(folder.clone());
        }
    }

    type Outcome = std::result::Result<Vec<u8>, String>;

    fn outcome(reader: Result<Box<dyn Read + Send>>, size: u64) -> Outcome {
        let mut reader = reader.map_err(|error| error.to_string())?;
        let mut decoded = Vec::new();
        let mut buffer = vec![0u8; 100_003];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                if decoded.len() as u64 != size {
                    return Err(format!("ends after {} bytes", decoded.len()));
                }
                return Ok(decoded);
            }
            decoded.extend_from_slice(&buffer[..read]);
            if decoded.len() as u64 > size {
                return Err("decodes past its size".into());
            }
        }
    }

    fn tally_agreement(old: Outcome, new: Outcome, case: &str, tally: &mut Tally) {
        match (old, new) {
            (Ok(old), Ok(new)) if old == new => tally.same_bytes += 1,
            (Err(_), Err(_)) => tally.both_refused += 1,
            (old, new) => panic!(
                "{case}: liblzma {:?}, Zipper {:?}",
                old.map(|bytes| bytes.len()),
                new.map(|bytes| bytes.len())
            ),
        }
    }

    #[derive(Default)]
    struct Tally {
        same_bytes: usize,
        both_refused: usize,
        liblzma_refuses_the_properties: usize,
        empty_lzma_left_to_7zip_tests: usize,
        refused_at_the_size_as_7zip_does: usize,
        filtered_same_bytes: usize,
    }

    fn archive_file(path: &Path) -> ArchiveFile {
        std::fs::File::open(path).unwrap().into()
    }

    fn compare_folder(path: &Path, folder: &Folder, case: &str, tally: &mut Tally) {
        let lzma = matches!(
            folder.output_to_packed.last().map(method),
            Some(Ok(Method::Lzma))
        );
        if lzma && folder.size == 0 {
            tally.empty_lzma_left_to_7zip_tests += 1;
            return;
        }
        let Ok(old) = liblzma_folder_reader(archive_file(path), folder) else {
            tally.liblzma_refuses_the_properties += 1;
            return;
        };
        let old = outcome(Ok(old), folder.size);
        let new = outcome(folder_reader(archive_file(path), folder, None), folder.size);
        if let (true, Ok(_), Err(refusal)) = (lzma, &old, &new)
            && [
                "LZMA match runs past the declared size",
                "LZMA stream goes on past its declared size without an end marker",
            ]
            .contains(&refusal.as_str())
        {
            tally.refused_at_the_size_as_7zip_does += 1;
            return;
        }
        if folder.output_to_packed.len() > 1
            && matches!((&old, &new), (Ok(old), Ok(new)) if old == new)
        {
            tally.filtered_same_bytes += 1;
        }
        tally_agreement(old, new, case, tally);
    }

    fn compare_segments(path: &Path, folder: &Folder, case: &str, tally: &mut Tally) -> bool {
        let [coder] = folder.output_to_packed.as_slice() else {
            return false;
        };
        let (Ok(Method::Lzma2), &[properties]) = (method(coder), coder.properties.as_slice())
        else {
            return false;
        };
        let Ok(Some(segments)) =
            independent_lzma2_segments(&mut archive_file(path), folder, properties, 1)
        else {
            return false;
        };
        let stop = AtomicBool::new(false);
        for (index, segment) in segments.iter().enumerate() {
            let start = SIGNATURE_HEADER + folder.pack_offset + segment.pack_offset;
            let old = liblzma_decode_segment(
                path,
                start,
                segment.pack_length,
                segment.output,
                properties,
            )
            .map_err(|error| error.to_string());
            let new = decode_segment(
                archive_file(path),
                start,
                segment.pack_length,
                segment.output,
                properties,
                &stop,
            )
            .map_err(|error| error.to_string());
            tally_agreement(old, new, &format!("{case} segment {index}"), tally);
        }
        segments.len() > 1
    }

    fn corpus_archives() -> Vec<PathBuf> {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut archives = Vec::new();
        for (folder, prefix) in [
            ("7z-corpus", ""),
            ("7z-hostile", ""),
            ("parity/archives", "7z"),
        ] {
            for entry in fs::read_dir(data.join(folder)).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.starts_with(prefix) && name.ends_with(".7z") {
                    archives.push(path);
                }
            }
        }
        archives.sort();
        archives
    }

    #[test]
    fn a_segment_must_decode_to_its_size_from_all_its_packed_bytes() {
        let scratch = tempfile::tempdir().unwrap();
        let stored = |data: &[u8]| {
            let mut chunk = vec![1];
            chunk.extend_from_slice(&((data.len() - 1) as u16).to_be_bytes());
            chunk.extend_from_slice(data);
            chunk
        };
        let stop = AtomicBool::new(false);
        for (case, packed, output, decodes) in [
            ("exact", stored(b"abcdef"), 6, true),
            ("longer than declared", stored(b"abcdef"), 5, false),
            ("shorter than declared", stored(b"abcdef"), 7, false),
            (
                "bytes after the end marker",
                [stored(b"abcdef"), vec![0, 7]].concat(),
                6,
                false,
            ),
        ] {
            let path = scratch.path().join("segment");
            fs::write(&path, &packed).unwrap();
            let length = packed.len() as u64;
            let ours = decode_segment(archive_file(&path), 0, length, output, 18, &stop);
            let liblzma = liblzma_decode_segment(&path, 0, length, output, 18);
            assert_eq!(ours.is_ok(), decodes, "{case}: {ours:?}");
            assert_eq!(liblzma.is_ok(), decodes, "liblzma, {case}");
            if decodes {
                assert_eq!(ours.unwrap(), b"abcdef");
            }
        }
    }

    fn segmented_lzma2(scratch: &Path) -> (PathBuf, Vec<Folder>) {
        let mut packed = Vec::new();
        let mut data = Vec::new();
        for part in 0..3u32 {
            let plain: Vec<u8> = (0..150_000u32)
                .map(|index| {
                    b"segment text "[(index % 13) as usize]
                        ^ (index.wrapping_mul(2_654_435_761) >> 29) as u8
                        ^ part as u8
                })
                .collect();
            let mut filters = liblzma::stream::Filters::new();
            filters.lzma2(&liblzma::stream::LzmaOptions::new_preset(1).unwrap());
            let mut encoder = liblzma::stream::Stream::new_raw_encoder(&filters).unwrap();
            let mut encoded = Vec::with_capacity(plain.len());
            encoder
                .process_vec(&plain, &mut encoded, liblzma::stream::Action::Finish)
                .unwrap();
            assert_eq!(encoded.pop(), Some(0));
            packed.extend_from_slice(&encoded);
            data.extend_from_slice(&plain);
        }
        packed.push(0);
        let path = scratch.join("segmented-lzma2.7z");
        fs::write(
            &path,
            [vec![0u8; SIGNATURE_HEADER as usize], packed.clone()].concat(),
        )
        .unwrap();
        let folder = Folder {
            bcj2_branches: Vec::new(),
            output_to_packed: vec![Coder {
                id: vec![0x21],
                properties: vec![18],
            }],
            unsupported: None,
            size: data.len() as u64,
            crc: Some(crc32fast::hash(&data)),
            pack_offset: 0,
            pack_size: packed.len() as u64,
        };
        (path, vec![folder])
    }

    #[test]
    fn every_lzma_folder_in_the_corpora_decodes_as_liblzma_does_whole_damaged_or_in_segments() {
        let scratch = tempfile::tempdir().unwrap();
        let mut state = 0x2545_F491u32;
        let mut next = move |bound: u64| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            u64::from(state >> 1) % bound.max(1)
        };
        let (mut tally, mut folders, mut segmented) = (Tally::default(), 0, 0);
        let mut sources: Vec<(PathBuf, Vec<Folder>)> = corpus_archives()
            .into_iter()
            .filter_map(|archive| {
                let (_, parsed) = open(&archive).ok()?;
                let mut found = Vec::new();
                for folder in &parsed.folders {
                    lzma_folders(folder, &mut found);
                }
                Some((archive, found))
            })
            .collect();
        sources.push(segmented_lzma2(scratch.path()));
        for (archive, found) in sources {
            let bytes = fs::read(&archive).unwrap();
            let name = archive.file_name().unwrap().to_string_lossy().into_owned();
            for (index, folder) in found.iter().enumerate() {
                folders += 1;
                compare_folder(
                    &archive,
                    folder,
                    &format!("{name} folder {index}"),
                    &mut tally,
                );
                segmented += usize::from(compare_segments(
                    &archive,
                    folder,
                    &format!("{name} folder {index}"),
                    &mut tally,
                ));
                let packed_start = (SIGNATURE_HEADER + folder.pack_offset) as usize;
                for damage in 0..12 {
                    let mut damaged = bytes.clone();
                    let mut changed = folder.clone();
                    let what = match damage {
                        0..6 => {
                            let at = packed_start + next(folder.pack_size) as usize;
                            damaged[at] ^= 1 << next(8);
                            format!("bit flip at {at}")
                        }
                        6..9 => {
                            changed.pack_size = next(folder.pack_size);
                            format!("packed size cut to {}", changed.pack_size)
                        }
                        9 => {
                            changed.size = folder.size + 1 + next(1000);
                            format!("output size grown to {}", changed.size)
                        }
                        10 => {
                            let coder = changed.output_to_packed.last_mut().unwrap();
                            if coder.properties.len() == 5 {
                                coder.properties[1..5].copy_from_slice(&4096u32.to_le_bytes());
                            } else {
                                coder.properties = vec![0];
                            }
                            "dictionary of 4 KiB".to_owned()
                        }
                        _ => {
                            let coder = changed.output_to_packed.last_mut().unwrap();
                            if coder.properties.len() == 5 {
                                coder.properties[0] = next(225) as u8;
                            }
                            format!("properties {:?}", coder.properties)
                        }
                    };
                    let path = scratch.path().join("damaged.7z");
                    fs::write(&path, &damaged).unwrap();
                    let case = format!("{name} folder {index}, {what}");
                    compare_folder(&path, &changed, &case, &mut tally);
                    compare_segments(&path, &changed, &case, &mut tally);
                }
            }
        }
        assert!(folders >= 60, "{folders} LZMA folders");
        assert!(segmented >= 1, "{segmented} folders split into segments");
        assert!(tally.same_bytes >= 300, "{} same", tally.same_bytes);
        assert!(tally.both_refused >= 300, "{} refused", tally.both_refused);
        assert!(tally.liblzma_refuses_the_properties >= 1);
        assert!(tally.empty_lzma_left_to_7zip_tests >= 1);
        assert!(tally.refused_at_the_size_as_7zip_does >= 1);
        assert!(
            tally.filtered_same_bytes >= 60,
            "{}",
            tally.filtered_same_bytes
        );
    }

    fn shape(
        streams: &[(u64, u64)],
        bind_pairs: Vec<(u64, u64)>,
        packed_inputs: Vec<u64>,
    ) -> FolderShape {
        FolderShape {
            coders: streams
                .iter()
                .map(|&streams| Coder {
                    id: if streams == (4, 1) {
                        BCJ2.to_vec()
                    } else {
                        vec![0x21]
                    },
                    properties: Vec::new(),
                })
                .collect(),
            streams: streams.to_vec(),
            input_starts: streams
                .iter()
                .scan(0, |next, streams| {
                    Some(std::mem::replace(next, *next + streams.0))
                })
                .collect(),
            output_starts: streams
                .iter()
                .scan(0, |next, streams| {
                    Some(std::mem::replace(next, *next + streams.1))
                })
                .collect(),
            bind_pairs,
            packed_inputs,
            outputs: streams.iter().map(|streams| streams.1 as usize).sum(),
            several_streams: None,
        }
    }

    fn plan_is_sound(shape: &FolderShape, branches: &[Bcj2Branch]) -> bool {
        let mut input_start = Vec::new();
        let mut output_start = Vec::new();
        let (mut inputs, mut outputs) = (0, 0);
        for &(coder_inputs, coder_outputs) in &shape.streams {
            input_start.push(inputs);
            output_start.push(outputs);
            inputs += coder_inputs;
            outputs += coder_outputs;
        }
        let joins: Vec<usize> = (0..shape.streams.len())
            .filter(|&coder| shape.streams[coder] != (1, 1))
            .collect();
        let [bcj2] = joins[..] else { return false };
        if shape.streams[bcj2] != (4, 1)
            || shape
                .bind_pairs
                .iter()
                .any(|&(_, output)| output == output_start[bcj2])
            || branches.len() != 4
        {
            return false;
        }
        let mut coders_seen = vec![bcj2];
        let mut packed_seen = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let mut input = input_start[bcj2] + index as u64;
            for &coder in &branch.coders {
                if !shape.bind_pairs.contains(&(input, output_start[coder])) {
                    return false;
                }
                input = input_start[coder];
            }
            if shape.packed_inputs.get(branch.packed) != Some(&input)
                || branch.output
                    != branch
                        .coders
                        .first()
                        .map(|&coder| output_start[coder] as usize)
            {
                return false;
            }
            coders_seen.extend(&branch.coders);
            packed_seen.push(branch.packed);
        }
        coders_seen.sort_unstable();
        packed_seen.sort_unstable();
        coders_seen == (0..shape.streams.len()).collect::<Vec<_>>()
            && packed_seen == (0..shape.packed_inputs.len()).collect::<Vec<_>>()
    }

    fn sequences(length: usize, values: u64) -> Vec<Vec<u64>> {
        (0..length).fold(vec![Vec::new()], |sequences, _| {
            sequences
                .iter()
                .flat_map(|sequence| {
                    (0..values).map(move |value| {
                        let mut next = sequence.clone();
                        next.push(value);
                        next
                    })
                })
                .collect()
        })
    }

    #[test]
    fn every_small_folder_graph_plans_only_sound_bcj2_branches() {
        let kinds = [(4, 1), (1, 1), (2, 1)];
        let (mut planned, mut checked) = (0, 0);
        for count in 1..=3 {
            for choice in sequences(count, kinds.len() as u64) {
                let streams: Vec<(u64, u64)> =
                    choice.iter().map(|&kind| kinds[kind as usize]).collect();
                let inputs: u64 = streams.iter().map(|streams| streams.0).sum();
                let outputs: u64 = streams.iter().map(|streams| streams.1).sum();
                if inputs > 6 {
                    continue;
                }
                let pairs: Vec<(u64, u64)> = (0..inputs)
                    .flat_map(|input| (0..outputs).map(move |output| (input, output)))
                    .collect();
                for bind in sequences(outputs as usize - 1, pairs.len() as u64) {
                    let bind_pairs: Vec<(u64, u64)> =
                        bind.iter().map(|&pair| pairs[pair as usize]).collect();
                    for packed_inputs in sequences((inputs - (outputs - 1)) as usize, inputs + 1) {
                        let shape = shape(&streams, bind_pairs.clone(), packed_inputs);
                        if let Ok(Some(branches)) = bcj2_plan(&shape) {
                            assert!(
                                plan_is_sound(&shape, &branches),
                                "{:?} {:?} {:?}",
                                shape.streams,
                                shape.bind_pairs,
                                shape.packed_inputs
                            );
                            planned += 1;
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            planned > 1000 && checked > 1_000_000,
            "{planned} of {checked}"
        );
    }

    struct Trickle<'a> {
        data: &'a [u8],
        most: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = buffer.len().min(self.most).min(self.data.len());
            buffer[..count].copy_from_slice(&self.data[..count]);
            self.data = &self.data[count..];
            Ok(count)
        }
    }

    #[test]
    fn byte_swap_reverses_every_whole_group_under_every_read_split() {
        let mut checked = 0;
        for width in [2, 4] {
            for length in 0..=13u8 {
                let data: Vec<u8> = (0..length).collect();
                let mut expected = data.clone();
                for group in expected.chunks_exact_mut(width) {
                    group.reverse();
                }
                for (most, chunk, capacity) in (1..=6).flat_map(|most| {
                    (1..=6).flat_map(move |chunk| {
                        [1, 4, 5, 7, 64].map(|capacity| (most, chunk, capacity))
                    })
                }) {
                    {
                        let mut swap =
                            ByteSwap::new(Trickle { data: &data, most }, width, capacity);
                        let mut output = Vec::new();
                        let mut buffer = vec![0; chunk];
                        loop {
                            let read = swap.read(&mut buffer).unwrap();
                            if read == 0 {
                                break;
                            }
                            output.extend_from_slice(&buffer[..read]);
                        }
                        assert_eq!(
                            output, expected,
                            "width {width} length {length} most {most} chunk {chunk} capacity {capacity}"
                        );
                        assert_eq!(swap.read(&mut buffer).unwrap(), 0);
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 2 * 14 * 36 * 5);
    }

    #[test]
    fn a_byte_swap_in_front_of_segmented_lzma2_reserves_the_segment_budget() {
        let coder = |id: &[u8], properties: &[u8]| Coder {
            id: id.to_vec(),
            properties: properties.to_vec(),
        };
        let folder = Folder {
            output_to_packed: vec![coder(&[0x02, 0x03, 0x02], &[]), coder(&[0x21], &[16])],
            bcj2_branches: Vec::new(),
            unsupported: None,
            size: PARALLEL_FOLDER_MIN,
            crc: None,
            pack_offset: 0,
            pack_size: 1,
        };
        assert_eq!(decoder_memory(&folder, true), SEGMENT_BUDGET);
    }

    #[test]
    fn a_coder_above_bcj2_is_left_to_7zip() {
        let shape = shape(&[(1, 1), (4, 1)], vec![(0, 1)], vec![1, 2, 3, 4]);
        assert!(matches!(bcj2_plan(&shape), Ok(None)));
    }

    #[test]
    fn four_filters_chain_in_front_of_lzma_and_a_filter_with_bad_properties_goes_to_7zip() {
        let coder = |id: &[u8], properties: &[u8]| Coder {
            id: id.to_vec(),
            properties: properties.to_vec(),
        };
        let lzma = coder(&[0x03, 0x01, 0x01], &[0x5D, 0, 0, 1, 0]);
        let filters = [
            coder(&[0x03], &[3]),
            coder(&[0x03, 0x03, 0x01, 0x03], &[]),
            coder(&[0x0A], &[]),
            coder(&[0x03, 0x03, 0x08, 0x05], &[]),
        ];
        let folder = |coders: &[Coder]| Folder {
            output_to_packed: [coders, std::slice::from_ref(&lzma)].concat(),
            bcj2_branches: Vec::new(),
            unsupported: None,
            size: 1,
            crc: None,
            pack_offset: 0,
            pack_size: 1,
        };
        for count in 1..=4 {
            check_methods(&folder(&filters[..count]))
                .unwrap_or_else(|error| panic!("{count} filters: {error}"));
        }
        let start_on_x86 = [
            filters[0].clone(),
            coder(&[0x03, 0x03, 0x01, 0x03], &[4, 0, 0, 0]),
        ];
        let error = check_methods(&folder(&start_on_x86)).err().unwrap();
        assert!(error.is::<Unsupported>(), "{error}");
        assert!(
            error
                .to_string()
                .contains("properties 7-Zip does not accept"),
            "{error}"
        );
    }

    #[test]
    fn bcj2_branches_share_one_decoder_budget() {
        let ppmd = |memory: u32| {
            let mut properties = vec![6];
            properties.extend_from_slice(&memory.to_le_bytes());
            Folder {
                output_to_packed: vec![Coder {
                    id: vec![0x03, 0x04, 0x01],
                    properties,
                }],
                bcj2_branches: Vec::new(),
                unsupported: None,
                size: 1,
                crc: None,
                pack_offset: 0,
                pack_size: 1,
            }
        };
        let folder = |memory: u32| Folder {
            output_to_packed: vec![Coder {
                id: BCJ2.to_vec(),
                properties: Vec::new(),
            }],
            bcj2_branches: vec![ppmd(memory), ppmd(memory), ppmd(1 << 20), ppmd(1 << 20)],
            unsupported: None,
            size: 1,
            crc: None,
            pack_offset: 0,
            pack_size: 4,
        };
        assert!(check_methods(&folder(1 << 29)).is_ok());
        let Err(error) = check_methods(&folder(3 << 29)) else {
            panic!("two 1.5 GiB PPMd branches fit the budget");
        };
        assert!(error.is::<Unsupported>(), "{error}");
    }

    #[test]
    fn every_bcj2_layout_with_up_to_four_chained_coders_is_planned() {
        let mut checked = 0;
        for chained in 0..=4usize {
            let count = chained + 1;
            for bcj2 in 0..count {
                let others: Vec<usize> = (0..count).filter(|&coder| coder != bcj2).collect();
                for owners in sequences(chained, 4) {
                    let mut branches: Vec<Vec<usize>> = vec![Vec::new(); 4];
                    for (&coder, &owner) in others.iter().zip(&owners) {
                        branches[owner as usize].push(coder);
                    }
                    let mut streams = vec![(1, 1); count];
                    streams[bcj2] = (4, 1);
                    let input_start =
                        |coder: usize| -> u64 { (0..coder).map(|before| streams[before].0).sum() };
                    let output_start =
                        |coder: usize| -> u64 { (0..coder).map(|before| streams[before].1).sum() };
                    let mut bind_pairs = Vec::new();
                    let mut ends = Vec::new();
                    for (index, branch) in branches.iter().enumerate() {
                        let mut input = input_start(bcj2) + index as u64;
                        for &coder in branch {
                            bind_pairs.push((input, output_start(coder)));
                            input = input_start(coder);
                        }
                        ends.push(input);
                    }
                    for rotation in 0..4 {
                        let mut packed_inputs = ends.clone();
                        packed_inputs.rotate_left(rotation);
                        let mut pairs = bind_pairs.clone();
                        let turn = rotation % pairs.len().max(1);
                        pairs.rotate_left(turn);
                        let shape = shape(&streams, pairs, packed_inputs);
                        let plan = bcj2_plan(&shape).unwrap().unwrap();
                        assert!(plan_is_sound(&shape, &plan));
                        let planned: Vec<Vec<usize>> =
                            plan.into_iter().map(|branch| branch.coders).collect();
                        assert_eq!(planned, branches);
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 5000, "{checked}");
    }

    fn text(seed: u8) -> Vec<u8> {
        (0..3000u32)
            .map(|index| {
                b"abcdefgh"[((index * 7 + u32::from(seed)) % 8) as usize] ^ (index / 97) as u8
            })
            .collect()
    }

    fn encoded(data: &[u8], filters: &liblzma::stream::Filters) -> Vec<u8> {
        let mut compressed = Vec::new();
        liblzma::read::XzEncoder::new_stream(
            data,
            liblzma::stream::Stream::new_raw_encoder(filters).unwrap(),
        )
        .read_to_end(&mut compressed)
        .unwrap();
        compressed
    }

    fn lzma2_only() -> liblzma::stream::Filters {
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma2_properties(&[18]).unwrap();
        filters
    }

    fn own_decoder(coder: &Coder, input: &[u8], size: u64) -> Box<dyn Read + Send> {
        lzma_coder(coder, size)
            .unwrap()
            .reader(std::io::Cursor::new(input.to_vec()), size)
    }

    fn decoded(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        assert_eq!(reader.read(&mut [0; 8])?, 0);
        Ok(output)
    }

    #[test]
    fn lzma2_may_stop_only_after_a_whole_chunk() {
        let mut encoder = liblzma::stream::Filters::new();
        encoder.lzma2(&liblzma::stream::LzmaOptions::new_preset(0).unwrap());
        let (first, second) = (text(1), text(2));
        let mut stream = encoded(&first, &encoder);
        let first_end = stream.len() - 1;
        stream.truncate(first_end);
        stream.extend_from_slice(&encoded(&second, &encoder));
        let whole = [first.as_slice(), &second].concat();
        let chunk_ends = [
            (0, &whole[..0]),
            (first_end, &first[..]),
            (stream.len() - 1, &whole[..]),
            (stream.len(), &whole[..]),
        ];
        let coder = Coder {
            id: vec![0x21],
            properties: vec![18],
        };
        for cut in 0..=stream.len() {
            let with_an_end_marker = (&stream[..cut]).chain(&[0u8][..]);
            let liblzma = decoded(raw_decoder(&lzma2_only(), with_an_end_marker).unwrap());
            let own = decoded(own_decoder(&coder, &stream[..cut], whole.len() as u64));
            for result in [liblzma, own] {
                match chunk_ends.iter().find(|&&(end, _)| end == cut) {
                    Some(&(_, expected)) => assert_eq!(result.unwrap(), expected, "cut {cut}"),
                    None => assert!(result.is_err(), "cut {cut} of {}", stream.len()),
                }
            }
        }
    }

    fn lzma_folder(size: u64) -> Folder {
        let mut properties = vec![0x5D];
        properties.extend_from_slice(&(1u32 << 16).to_le_bytes());
        Folder {
            bcj2_branches: Vec::new(),
            output_to_packed: vec![Coder {
                id: vec![0x03, 0x01, 0x01],
                properties,
            }],
            unsupported: None,
            size,
            crc: None,
            pack_offset: 0,
            pack_size: 0,
        }
    }

    #[test]
    fn lzma_ends_exactly_at_its_declared_size() {
        let data = text(3);
        let mut encoder = liblzma::stream::Filters::new();
        let mut options = liblzma::stream::LzmaOptions::new_preset(0).unwrap();
        options
            .dict_size(1 << 16)
            .literal_context_bits(3)
            .literal_position_bits(0)
            .position_bits(2);
        encoder.lzma1(&options);
        let stream = encoded(&data, &encoder);
        let size = data.len() as u64;
        let read = |input: &[u8], size| {
            decoded(own_decoder(
                &lzma_folder(size).output_to_packed[0],
                input,
                size,
            ))
        };
        assert_eq!(read(&stream, size).unwrap(), data);
        let early = read(&stream, size - 1).unwrap_err().to_string();
        assert!(
            [
                "LZMA match runs past the declared size",
                "LZMA stream goes on past its declared size without an end marker",
            ]
            .contains(&early.as_str()),
            "{early}"
        );
        assert!(read(&stream, size + 1).is_err());
        for cut in 0..stream.len() {
            let error = read(&stream[..cut], size).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidData,
                "cut {cut}: {error}"
            );
        }
    }

    const SEVEN_ZIP_FILTERS: [(&str, &[u8]); 9] = [
        ("x86", &[0x03, 0x03, 0x01, 0x03]),
        ("PowerPC", &[0x03, 0x03, 0x02, 0x05]),
        ("IA-64", &[0x03, 0x03, 0x04, 0x01]),
        ("ARM", &[0x03, 0x03, 0x05, 0x01]),
        ("ARM Thumb", &[0x03, 0x03, 0x07, 0x01]),
        ("SPARC", &[0x03, 0x03, 0x08, 0x05]),
        ("ARM64", &[0x0A]),
        ("RISC-V", &[0x0B]),
        ("Delta", &[0x03]),
    ];
    const BRANCH_BUFFER: usize = 64 * 1024;

    fn filter_name(id: &[u8]) -> &'static str {
        SEVEN_ZIP_FILTERS
            .iter()
            .find(|(_, filter)| *filter == id)
            .map(|(name, _)| *name)
            .unwrap()
    }

    fn filter_coder(name: &str, properties: &[u8]) -> Coder {
        let (_, id) = SEVEN_ZIP_FILTERS
            .iter()
            .find(|(filter, _)| *filter == name)
            .unwrap();
        Coder {
            id: id.to_vec(),
            properties: properties.to_vec(),
        }
    }

    fn liblzma_filter(
        filters: &mut liblzma::stream::Filters,
        name: &str,
        properties: &[u8],
    ) -> std::result::Result<(), liblzma::stream::Error> {
        match name {
            "x86" => filters.x86_properties(properties),
            "PowerPC" => filters.powerpc_properties(properties),
            "IA-64" => filters.ia64_properties(properties),
            "ARM" => filters.arm_properties(properties),
            "ARM Thumb" => filters.arm_thumb_properties(properties),
            "SPARC" => filters.sparc_properties(properties),
            "ARM64" => filters.arm64_properties(properties),
            "RISC-V" => filters.riscv_properties(properties),
            _ => filters.delta_properties(properties),
        }
        .map(drop)
    }

    fn seven_zip_accepts_filter_properties(name: &str, properties: &[u8]) -> bool {
        let start = <[u8; 4]>::try_from(properties).map(u32::from_le_bytes);
        match name {
            "Delta" => properties.len() == 1,
            _ if properties.is_empty() => true,
            "ARM64" => start.is_ok_and(|start| start % 4 == 0),
            "RISC-V" => start.is_ok_and(|start| start % 2 == 0),
            _ => false,
        }
    }

    fn stored_lzma2(data: &[u8]) -> Vec<u8> {
        let mut stored = Vec::with_capacity(data.len() + data.len() / STORED_CHUNK * 3 + 4);
        StoredLzma2::new(data).read_to_end(&mut stored).unwrap();
        stored
    }

    fn liblzma_unfiltered(coders: &[Coder], filtered: &[u8]) -> Vec<u8> {
        let mut data = filtered.to_vec();
        for coder in coders.iter().rev() {
            let mut filters = liblzma::stream::Filters::new();
            liblzma_filter(&mut filters, filter_name(&coder.id), &coder.properties).unwrap();
            filters.lzma2_properties(&[0]).unwrap();
            let mut stream = liblzma::stream::Stream::new_raw_decoder(&filters).unwrap();
            let mut out = Vec::with_capacity(data.len() + 64);
            let status = stream
                .process_vec(
                    &stored_lzma2(&data),
                    &mut out,
                    liblzma::stream::Action::Finish,
                )
                .unwrap();
            assert_eq!(status, liblzma::stream::Status::StreamEnd);
            data = out;
        }
        data
    }

    struct Reads {
        data: Vec<u8>,
        at: usize,
        most: usize,
        random: bool,
        state: u32,
    }

    impl Reads {
        fn new(data: Vec<u8>, most: usize, random: bool, seed: u32) -> Self {
            Reads {
                data,
                at: 0,
                most,
                random,
                state: seed | 1,
            }
        }

        fn next_size(&mut self) -> usize {
            if !self.random {
                return self.most;
            }
            self.state = self.state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            1 + (self.state >> 8) as usize % self.most
        }
    }

    impl Read for Reads {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = buffer
                .len()
                .min(self.next_size())
                .min(self.data.len() - self.at);
            buffer[..count].copy_from_slice(&self.data[self.at..self.at + count]);
            self.at += count;
            Ok(count)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Split {
        input_most: usize,
        output_most: usize,
        random: bool,
    }

    fn ours_unfiltered(coders: &[Coder], filtered_data: &[u8], split: Split, seed: u32) -> Vec<u8> {
        let input = Reads::new(filtered_data.to_vec(), split.input_most, split.random, seed);
        let mut sizes = Reads::new(Vec::new(), split.output_most, split.random, !seed);
        let mut reader = filtered(coders, Box::new(input)).unwrap();
        let mut out = Vec::with_capacity(filtered_data.len());
        let mut buffer = vec![0u8; split.output_most.min(1 << 20)];
        loop {
            let want = sizes.next_size().min(buffer.len());
            match reader.read(&mut buffer[..want]).unwrap() {
                0 => break,
                read => out.extend_from_slice(&buffer[..read]),
            }
        }
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
        out
    }

    fn code_like(name: &str, length: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        let mut next = move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            state.rotate_right(13)
        };
        let mut code = Vec::with_capacity(length + 16);
        while code.len() < length {
            let (pick, word) = (next(), next());
            if pick % 5 == 0 {
                code.push(word as u8);
                continue;
            }
            match name {
                "x86" => {
                    let top = if word >> 31 == 0 { 0x00 } else { 0xFF };
                    code.push(0xE8 | (pick >> 3 & 1) as u8);
                    code.extend_from_slice(&word.to_le_bytes()[..3]);
                    code.push(if pick & 0x40 == 0 { top } else { word as u8 });
                }
                "PowerPC" => {
                    code.extend((0x4800_0001 | (word & 0x03FF_FFFC) | (pick & 2)).to_be_bytes())
                }
                "IA-64" => {
                    let template = [0x10u128, 0x12, 0x16, 0x18, 0x1C, 0x11][pick as usize % 6];
                    let mut bundle = template;
                    for slot in 0..3 {
                        let offset = u128::from(next() & 0x1F_FFFF);
                        let btype = u128::from(next() % 3 == 0);
                        let instruction = (5u128 << 37)
                            | ((offset >> 20) << 36)
                            | ((offset & 0xF_FFFF) << 13)
                            | (btype << 9);
                        bundle |= instruction << (5 + 41 * slot);
                    }
                    code.extend(bundle.to_le_bytes());
                }
                "ARM" => code.extend([word as u8, (word >> 8) as u8, (word >> 16) as u8, 0xEB]),
                "ARM Thumb" => code.extend([
                    word as u8,
                    0xF0 | (word >> 8 & 7) as u8,
                    (word >> 16) as u8,
                    0xF8 | (word >> 24 & 7) as u8,
                ]),
                "SPARC" => {
                    let call = if pick & 1 == 0 {
                        0x4000_0000
                    } else {
                        0x7FC0_0000
                    };
                    code.extend((call | (word & 0x003F_FFFF)).to_be_bytes())
                }
                "ARM64" => {
                    let instruction = if pick & 1 == 0 {
                        0x9400_0000 | (word & 0x03FF_FFFF)
                    } else {
                        0x9000_0000 | (word & 0x60FF_FFFF)
                    };
                    code.extend(instruction.to_le_bytes())
                }
                "RISC-V" => {
                    let rd = [1u32, 5, 2, 10][pick as usize % 4];
                    match pick >> 4 & 3 {
                        0 => code.extend((0xEF | (rd << 7) | (word & 0xFFFF_F000)).to_le_bytes()),
                        1 => {
                            code.extend((0x17 | (rd << 7) | (word & 0xFFFF_F000)).to_le_bytes());
                            code.extend((0x13 | (rd << 15) | (next() << 20)).to_le_bytes());
                        }
                        2 => {
                            code.extend((0x3117 | (word & 0xF800_0000)).to_le_bytes());
                            code.extend(next().to_le_bytes());
                        }
                        _ => code.extend([0x01, 0x00]),
                    }
                }
                _ => code
                    .extend((0..4).map(|step| (pick as u8).wrapping_add(step * (word as u8 & 7)))),
            }
        }
        code.truncate(length);
        code
    }

    fn random_bytes(length: usize, seed: u32) -> Vec<u8> {
        let mut state = seed | 1;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    fn accepted_filter_properties(name: &str) -> Vec<Vec<u8>> {
        let starts: &[u32] = match name {
            "ARM64" => &[0, 4, 8, 0x1000, 0x1234_5678, 0x7FFF_FFFC, 0xFFFF_FFFC],
            "RISC-V" => &[0, 2, 4, 6, 0x1000, 0x1234_5676, 0xFFFF_FFFE],
            "Delta" => return (0..=255u8).map(|distance| vec![distance]).collect(),
            _ => return vec![Vec::new()],
        };
        std::iter::once(Vec::new())
            .chain(starts.iter().map(|start| start.to_le_bytes().to_vec()))
            .collect()
    }

    fn short_splits() -> [Split; 3] {
        [
            Split {
                input_most: 1,
                output_most: 1,
                random: false,
            },
            Split {
                input_most: 7,
                output_most: 9,
                random: true,
            },
            Split {
                input_most: usize::MAX,
                output_most: 1 << 20,
                random: false,
            },
        ]
    }

    #[test]
    fn every_filter_decodes_every_short_input_as_liblzma_does_under_every_read_split() {
        let mut checked = 0;
        for (name, _) in SEVEN_ZIP_FILTERS {
            for properties in accepted_filter_properties(name) {
                let coders = [filter_coder(name, &properties)];
                let splits: &[Split] = if name == "Delta" {
                    &short_splits()[1..2]
                } else {
                    &short_splits()
                };
                for length in 0..=64 {
                    for (pattern, data) in [
                        ("code", code_like(name, length, length as u32)),
                        ("random", random_bytes(length, length as u32 + 7)),
                    ] {
                        let expected = liblzma_unfiltered(&coders, &data);
                        for &split in splits {
                            let ours = ours_unfiltered(&coders, &data, split, length as u32);
                            assert_eq!(
                                ours, expected,
                                "{name} {properties:?} {pattern} length {length} {split:?}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 40_000, "{checked}");
    }

    #[test]
    fn every_filter_decodes_inputs_around_its_buffer_as_liblzma_does() {
        let mut checked = 0;
        let splits = [
            Split {
                input_most: 1,
                output_most: 70_000,
                random: true,
            },
            Split {
                input_most: 70_000,
                output_most: 1,
                random: false,
            },
            Split {
                input_most: 100_003,
                output_most: 65_537,
                random: true,
            },
            Split {
                input_most: usize::MAX,
                output_most: 1 << 20,
                random: false,
            },
        ];
        for (name, _) in SEVEN_ZIP_FILTERS {
            let all = accepted_filter_properties(name);
            let properties: Vec<&Vec<u8>> = all
                .iter()
                .step_by(if name == "Delta" { 51 } else { 1 })
                .collect();
            for (index, properties) in properties.into_iter().enumerate() {
                let coders = [filter_coder(name, properties)];
                for length in [
                    BRANCH_BUFFER - 9,
                    BRANCH_BUFFER - 1,
                    BRANCH_BUFFER,
                    BRANCH_BUFFER + 1,
                    BRANCH_BUFFER + 7,
                    2 * BRANCH_BUFFER + 3,
                    300_001,
                ] {
                    let seed = (length + index) as u32;
                    let data = if length % 2 == 0 {
                        code_like(name, length, seed)
                    } else {
                        [
                            code_like(name, length / 2, seed),
                            random_bytes(length - length / 2, seed),
                        ]
                        .concat()
                    };
                    let expected = liblzma_unfiltered(&coders, &data);
                    let split = splits[(length + index) % splits.len()];
                    assert_eq!(
                        ours_unfiltered(&coders, &data, split, seed),
                        expected,
                        "{name} {properties:?} length {length} {split:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 7 * 28, "{checked}");
    }

    #[test]
    fn chains_of_up_to_four_filters_decode_as_liblzma_applied_in_turn() {
        let names: Vec<&str> = SEVEN_ZIP_FILTERS.iter().map(|(name, _)| *name).collect();
        let first_properties = |name: &str, pick: usize| {
            let all = accepted_filter_properties(name);
            all[pick % all.len()].clone()
        };
        let mut chains: Vec<Vec<Coder>> = Vec::new();
        for (outer, &first) in names.iter().enumerate() {
            for (inner, &second) in names.iter().enumerate() {
                chains.push(vec![
                    filter_coder(first, &first_properties(first, inner)),
                    filter_coder(second, &first_properties(second, outer + 3)),
                ]);
            }
        }
        chains.push(vec![
            filter_coder("ARM64", &0x1000u32.to_le_bytes()),
            filter_coder("RISC-V", &[]),
            filter_coder("Delta", &[0]),
        ]);
        chains.push(vec![
            filter_coder("Delta", &[3]),
            filter_coder("x86", &[]),
            filter_coder("PowerPC", &[]),
            filter_coder("SPARC", &[]),
        ]);
        chains.push(vec![
            filter_coder("x86", &[]),
            filter_coder("x86", &[]),
            filter_coder("ARM Thumb", &[]),
            filter_coder("IA-64", &[]),
        ]);
        let mut checked = 0;
        for (index, chain) in chains.iter().enumerate() {
            let methods = chain
                .iter()
                .map(method)
                .collect::<Result<Vec<_>>>()
                .unwrap();
            let lzma2 = Coder {
                id: vec![0x21],
                properties: vec![18],
            };
            let folder = Folder {
                output_to_packed: [chain.clone(), vec![lzma2]].concat(),
                bcj2_branches: Vec::new(),
                unsupported: None,
                size: 1,
                crc: None,
                pack_offset: 0,
                pack_size: 1,
            };
            check_methods(&folder).unwrap_or_else(|error| panic!("chain {index}: {error}"));
            assert_eq!(methods.len(), chain.len());
            for length in [0, 1, 5, 17, 64, BRANCH_BUFFER + 3, 150_001] {
                let seed = (index * 31 + length) as u32;
                let data: Vec<u8> = chain
                    .iter()
                    .flat_map(|coder| {
                        code_like(filter_name(&coder.id), length / chain.len() + 1, seed)
                    })
                    .take(length)
                    .collect();
                let split = short_splits()[(index + length) % 3];
                assert_eq!(
                    ours_unfiltered(chain, &data, split, seed),
                    liblzma_unfiltered(chain, &data),
                    "chain {index} length {length}"
                );
                checked += 1;
            }
        }
        assert!(checked >= 84 * 7, "{checked}");
    }

    fn filter_property_variants() -> Vec<Vec<u8>> {
        let mut variants: Vec<Vec<u8>> = vec![Vec::new(), vec![0, 0], vec![0; 5], vec![1, 0, 0]];
        variants.extend((0..=255u8).map(|byte| vec![byte]));
        variants.extend(
            [
                0u32,
                1,
                2,
                3,
                4,
                6,
                16,
                0x1000,
                0x1001,
                0xFFFF_FFFC,
                0xFFFF_FFFE,
                u32::MAX,
            ]
            .map(|start| start.to_le_bytes().to_vec()),
        );
        variants
    }

    #[test]
    fn filter_properties_are_accepted_exactly_as_7zip_accepts_them() {
        let lzma2 = Coder {
            id: vec![0x21],
            properties: vec![18],
        };
        let (mut accepted, mut refused) = (0, 0);
        for (name, _) in SEVEN_ZIP_FILTERS {
            for properties in filter_property_variants() {
                let coder = filter_coder(name, &properties);
                let folder = Folder {
                    output_to_packed: vec![coder.clone(), lzma2.clone()],
                    bcj2_branches: Vec::new(),
                    unsupported: None,
                    size: 1,
                    crc: None,
                    pack_offset: 0,
                    pack_size: 1,
                };
                let checked = check_methods(&folder);
                let built = filtered(&[coder], Box::new(std::io::empty()));
                let case = format!("{name} {properties:?}");
                if seven_zip_accepts_filter_properties(name, &properties) {
                    checked.unwrap_or_else(|error| panic!("{case}: {error}"));
                    built.unwrap_or_else(|error| panic!("{case}: {error}"));
                    accepted += 1;
                } else {
                    let error = checked
                        .err()
                        .unwrap_or_else(|| panic!("{case} is accepted"));
                    assert!(error.is::<Unsupported>(), "{case}: {error}");
                    assert!(built.is_err(), "{case} builds");
                    refused += 1;
                }
            }
        }
        assert!(accepted >= 256 + 6 + 2 * 6, "{accepted}");
        assert!(refused >= 2000, "{refused}");
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

    #[kani::proof]
    #[kani::unwind(7)]
    fn filter_properties_are_read_as_7zip_reads_them() {
        use xz_filters::Branch::*;
        let branches = [X86, PowerPc, Ia64, Arm, ArmThumb, Sparc, Arm64, RiscV];
        let branch = branches[kani::any::<usize>() % branches.len()];
        let method = match kani::any::<u8>() % 3 {
            0 => FilterMethod::Branch(branch),
            1 => FilterMethod::BranchWithStart(branch),
            _ => FilterMethod::Delta,
        };
        let bytes: [u8; 5] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let properties = &bytes[..length];
        let start = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let expected = match method {
            FilterMethod::Delta if length == 1 => Some(FilterSetting::Delta {
                distance: usize::from(bytes[0]) + 1,
            }),
            FilterMethod::Delta => None,
            FilterMethod::Branch(_) | FilterMethod::BranchWithStart(_) if length == 0 => {
                Some(FilterSetting::Branch { branch, start: 0 })
            }
            FilterMethod::BranchWithStart(_) if length == 4 && start % branch.alignment() == 0 => {
                Some(FilterSetting::Branch { branch, start })
            }
            _ => None,
        };
        let setting = filter_setting(method, properties);
        assert!(setting == expected);
        if let Some(setting) = setting {
            assert!(setting.buffer() <= BRANCH_BUFFER as u64);
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn ppmd_properties_read_only_five_bytes() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= 6);
        let read = ppmd7_fields(&bytes[..length]);
        assert!(read.is_some() == (length == 5));
        if let Some((order, memory)) = read {
            assert!(order == u32::from(bytes[0]));
            assert!(memory == u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]));
        }
    }
}
