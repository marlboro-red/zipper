//! In-process extraction of plain, gzip, zstd and xz tar archives, published
//! only after the whole archive reads cleanly. See
//! `docs/reference/tar-reader.md` for what it accepts and restores.
use crate::progress::Tracker;
use crate::{BadField, Field, field_check};
use crate::{
    Entry,
    counters::{self, Counter},
    safe_path,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::SystemTime,
};

const BLOCK: usize = 512;
const COPY_BUFFER: usize = 1024 * 1024;
const MAX_EXTENSION: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_METADATA: usize = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;
const QUEUED_FILES: usize = 16;
const QUEUED_CHUNKS_PER_FILE: usize = 4;
const BYTES_IN_FLIGHT: usize = 16 * 1024 * 1024;
const DEFAULT_WRITERS: usize = 4;
// RECORD(benchmarks/cross-platform/tar-folder-lanes/README.md): one writer per folder is faster on Windows.
const ONE_WRITER_PER_FOLDER: bool = cfg!(windows);
const LINKS_PER_REPORT: usize = 1024;
const READ_ERROR: &str = "cannot read the tar stream";
const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

/// The compression around a tar stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Zstd,
    Xz,
    Bzip2,
    Lzma,
    Lzip,
    Lz4,
    Compress,
}

/// Tar extraction options.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions<'a> {
    /// Cap on declared output bytes. Without it, extraction must fit in the
    /// destination filesystem's free space.
    pub max_output_bytes: Option<u64>,
    /// Threads that create and write files while one thread decodes the
    /// stream, 1–64. Default: 4.
    pub writers: Option<usize>,
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

#[derive(Default)]
struct InFlight {
    bytes: Mutex<usize>,
    freed: Condvar,
    spare: Mutex<Vec<Vec<u8>>>,
}

impl InFlight {
    fn take(&self, length: usize) -> Chunk<'_> {
        let mut bytes = self.bytes.lock().unwrap();
        while *bytes > 0 && *bytes + length > BYTES_IN_FLIGHT {
            bytes = self.freed.wait(bytes).unwrap();
        }
        *bytes += length;
        drop(bytes);
        let recycled = (length == CHUNK)
            .then(|| self.spare.lock().unwrap().pop())
            .flatten();
        Chunk {
            data: recycled.unwrap_or_else(|| vec![0; length]),
            flight: self,
        }
    }
}

struct Chunk<'f> {
    data: Vec<u8>,
    flight: &'f InFlight,
}

impl Drop for Chunk<'_> {
    fn drop(&mut self) {
        let data = std::mem::take(&mut self.data);
        let length = data.len();
        if length == CHUNK {
            self.flight.spare.lock().unwrap().push(data);
        }
        *self.flight.bytes.lock().unwrap() -= length;
        self.flight.freed.notify_all();
    }
}

struct Job<'f> {
    path: PathBuf,
    name: String,
    mode: u32,
    modified: Option<SystemTime>,
    size: u64,
    first: Chunk<'f>,
    rest: Option<Receiver<Chunk<'f>>>,
}

struct Writers<'f> {
    lanes: Mutex<Vec<SyncSender<Job<'f>>>>,
    flight: &'f InFlight,
    pending: Mutex<usize>,
    idle: Condvar,
    failure: Mutex<Option<anyhow::Error>>,
    callback_panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

#[derive(Clone, Copy)]
enum Written {
    Bytes(u64),
    File,
}

impl<'f> Writers<'f> {
    fn send(&self, job: Job<'f>) -> Result<()> {
        *self.pending.lock().unwrap() += 1;
        let lanes = self.lanes.lock().unwrap();
        let lane = match lanes.len() {
            0 | 1 => 0,
            count => {
                let folder = job.name.rfind('/').map_or("", |slash| &job.name[..slash]);
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&folder.to_lowercase(), &mut hasher);
                std::hash::Hasher::finish(&hasher) as usize % count
            }
        };
        let sent = lanes.get(lane).is_some_and(|lane| lane.send(job).is_ok());
        ensure!(sent, "tar writers stopped");
        self.check()
    }

    fn check(&self) -> Result<()> {
        match self.failure.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn stopped(&self) -> anyhow::Error {
        self.check()
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("tar writers stopped"))
    }

    fn drain(&self) -> Result<()> {
        let mut pending = self.pending.lock().unwrap();
        while *pending > 0 {
            pending = self.idle.wait(pending).unwrap();
        }
        drop(pending);
        self.check()
    }

    fn close(&self) {
        self.lanes.lock().unwrap().clear();
    }

    fn report(&self, count: &dyn Fn(Written) -> Result<()>, written: Written) {
        let counted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = count(written);
        }));
        if let Err(panic) = counted {
            self.callback_panic.lock().unwrap().get_or_insert(panic);
            self.failure
                .lock()
                .unwrap()
                .get_or_insert_with(|| anyhow::anyhow!("the progress callback panicked"));
        }
    }

    fn work(&self, jobs: &Mutex<Receiver<Job<'f>>>, count: &dyn Fn(Written) -> Result<()>) {
        loop {
            let next = jobs.lock().unwrap().recv();
            let Ok(mut job) = next else {
                return;
            };
            if self.failure.lock().unwrap().is_none() {
                let report = |bytes: u64| self.report(count, Written::Bytes(bytes));
                let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (total, held) = write_job(&mut job, &report)?;
                    self.report(count, Written::Bytes(held));
                    if total == job.size {
                        self.report(count, Written::File);
                    }
                    Ok(())
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "a tar writer panicked writing {:?}",
                        job.name
                    ))
                });
                if let Err(error) = written {
                    self.failure.lock().unwrap().get_or_insert(error);
                }
            }
            drop(job);
            let mut pending = self.pending.lock().unwrap();
            *pending -= 1;
            if *pending == 0 {
                self.idle.notify_all();
            }
        }
    }
}

fn write_job(job: &mut Job, written: &dyn Fn(u64)) -> Result<(u64, u64)> {
    let name = &job.name;
    let mut file = crate::create_new_file(&job.path, job.mode, crate::windows_attributes(0, Some(job.mode))).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(
                "tar entry {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
            )
        } else {
            anyhow::Error::from(error).context(format!("cannot create tar entry {name:?}"))
        }
    })?;
    counters::add(Counter::FilesCreated, 1);
    let mut total = 0;
    let mut held = 0;
    let mut write = |bytes: &[u8]| -> Result<()> {
        total += bytes.len() as u64;
        if !bytes.is_empty() {
            file.write_all(bytes)
                .with_context(|| format!("cannot write tar entry {name:?}"))?;
            written(std::mem::replace(&mut held, bytes.len() as u64));
            counters::add(Counter::OutputWrites, 1);
            counters::add(Counter::OutputBytesWritten, bytes.len() as u64);
        }
        Ok(())
    };
    write(&job.first.data)?;
    if let Some(rest) = &job.rest {
        for chunk in rest {
            write(&chunk.data)?;
        }
    }
    if let Some(time) = job.modified {
        crate::set_modified_best_effort(&file, time);
    }
    drop(file);
    Ok((total, held))
}

/// Returns the compression of a tar archive, or `None` when the file does not
/// start with a tar header, plain or inside gzip, zstd or xz.
pub fn sniff(path: &Path) -> Result<Option<Compression>> {
    sniff_part(&Part::whole(path)?)
}

pub(crate) struct Part {
    path: PathBuf,
    start: u64,
    length: u64,
}

impl Part {
    pub(crate) fn new(path: &Path, start: u64, length: u64) -> Self {
        Part {
            path: path.to_owned(),
            start,
            length,
        }
    }

    fn whole(path: &Path) -> Result<Self> {
        Ok(Part::new(path, 0, crate::volumes::open(path)?.len()?))
    }

    fn open(&self) -> Result<PartReader> {
        let mut file = crate::volumes::open(&self.path)?;
        file.seek(SeekFrom::Start(self.start))?;
        Ok(PartReader {
            file,
            start: self.start,
            end: self.start.saturating_add(self.length),
            position: self.start,
        })
    }
}

struct PartReader {
    file: crate::volumes::ArchiveFile,
    start: u64,
    end: u64,
    position: u64,
}

impl Read for PartReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.end.saturating_sub(self.position);
        let want = buffer
            .len()
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        let read = self.file.read(&mut buffer[..want])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for PartReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => self.start.checked_add(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.end.checked_add_signed(offset),
        }
        .filter(|&target| target >= self.start)
        .ok_or_else(|| std::io::Error::other("seek outside the tar"))?;
        self.position = self.file.seek(SeekFrom::Start(target))?;
        Ok(self.position - self.start)
    }
}

pub(crate) fn sniff_part(part: &Part) -> Result<Option<Compression>> {
    let compression = compression_of(&mut part.open()?)?;
    let input = BufReader::new(part.open()?.take(SNIFF_INPUT));
    let mut stream = match decode(input, compression) {
        Ok(stream) => stream,
        Err(error) if is_over_memory(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    let mut first = [0u8; BLOCK];
    match stream.read_exact(&mut first) {
        Err(error) if error.kind() == std::io::ErrorKind::OutOfMemory => return Err(error.into()),
        Err(_) => return Ok(None),
        Ok(()) => {}
    }
    let tar = first != [0; BLOCK] && (checksum_matches(&first) || &first[257..262] == b"ustar");
    Ok(tar.then_some(compression))
}

const SKIPPABLE_FRAMES_SNIFFED: usize = 16;

pub(crate) fn compression_by_signature(path: &Path) -> Result<Compression> {
    compression_of(&mut crate::volumes::open(path)?)
}

fn compression_of(file: &mut (impl Read + Seek)) -> Result<Compression> {
    for skipped in 0..SKIPPABLE_FRAMES_SNIFFED {
        let mut magic = [0u8; 8];
        let read = read_up_to(file, &mut magic)?;
        let compression = match &magic[..read] {
            [0x1f, 0x8b, ..] => Compression::Gzip,
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Compression::Zstd,
            // SPEC(RFC 8878 3.1.2 and LZ4 Frame Format): zstd and LZ4 share the skippable frame, so the next frame decides.
            [0x50..=0x5f, 0x2a, 0x4d, 0x18, a, b, c, d] => {
                let size = u32::from_le_bytes([*a, *b, *c, *d]);
                if file.seek(SeekFrom::Current(i64::from(size))).is_err() {
                    return Ok(Compression::Zstd);
                }
                continue;
            }
            [0x50..=0x5f, 0x2a, 0x4d, 0x18, ..] => Compression::Zstd,
            [0xfd, b'7', b'z', b'X', b'Z', 0x00, ..] => Compression::Xz,
            [b'B', b'Z', b'h', b'1'..=b'9', ..] => Compression::Bzip2,
            [b'L', b'Z', b'I', b'P', 0 | 1, ..] => Compression::Lzip,
            [0x04, 0x22, 0x4D, 0x18, ..] | [0x02, 0x21, 0x4C, 0x18, ..] => Compression::Lz4,
            [0x1F, 0x9D, ..] => Compression::Compress,
            _ if skipped > 0 => Compression::Zstd,
            _ => Compression::None,
        };
        return Ok(compression);
    }
    Ok(Compression::Zstd)
}

fn read_up_to(file: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(filled)
}

const SNIFF_INPUT: u64 = crate::lz4::FIRST_BLOCK_INPUT_MAX;
const BZIP2_OUTPUT_PER_EXTRA_STREAM: u64 = 4096;
const LARGEST_DICTIONARY: u64 = 256 * 1024 * 1024;
#[cfg(test)]
const DECODER_OVERHEAD: u64 = 16 * 1024 * 1024;
#[cfg(test)]
const DECODER_MEMORY: u64 = LARGEST_DICTIONARY + DECODER_OVERHEAD;

pub(crate) fn decoder(path: &Path, compression: Compression) -> Result<Box<dyn Read + Send>> {
    decode(
        BufReader::with_capacity(COPY_BUFFER, crate::volumes::open(path)?),
        compression,
    )
}

pub(crate) fn decode<R: BufRead + Send + 'static>(
    input: R,
    compression: Compression,
) -> Result<Box<dyn Read + Send>> {
    Ok(match compression {
        Compression::None => Box::new(input),
        Compression::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(input)),
        Compression::Zstd => Box::new(zstd::stream::read::Decoder::with_buffer(input)?),
        Compression::Xz => Box::new(DecoderMemory(
            crate::lzma::XzReader::new(input).with_dictionary_limit(LARGEST_DICTIONARY),
        )),
        Compression::Bzip2 => Box::new(Bzip2Streams {
            decoder: Some(bzip2::bufread::BzDecoder::new(input)),
            streams: 1,
            decoded: 0,
        }),
        Compression::Lzma => Box::new(lzma_decoder(input)?),
        Compression::Lzip => Box::new(crate::lzma::LzipReader::new(input, |dictionary| {
            ensure_dictionary_fits("lzip", dictionary)
        })?),
        Compression::Lz4 => Box::new(crate::lz4::Lz4Frames::new(input)),
        Compression::Compress => Box::new(crate::lzw::Unlzw::new(input)),
    })
}

pub(crate) fn lzma_decoder<R: BufRead>(mut input: R) -> Result<crate::lzma::RawLzmaReader<R>> {
    if let Some(field) = input.fill_buf()?.get(1..5) {
        let dictionary = u32::from_le_bytes(field.try_into().unwrap());
        ensure_dictionary_fits("LZMA", u64::from(dictionary))?;
    }
    Ok(crate::lzma::lzma_file(input))
}

fn is_over_memory(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::OutOfMemory)
}

fn largest_dictionary() -> String {
    format!(
        "the largest dictionary Zipper accepts, {} MiB",
        LARGEST_DICTIONARY >> 20
    )
}

fn ensure_dictionary_fits(format: &str, dictionary: u64) -> std::io::Result<()> {
    if dictionary <= LARGEST_DICTIONARY {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::OutOfMemory,
        format!(
            "{format} stream declares a {dictionary}-byte dictionary, more than {}; {BACKEND_HINT}",
            largest_dictionary()
        ),
    ))
}

pub(crate) struct DecoderMemory<R>(pub(crate) R);

impl<R: Read> Read for DecoderMemory<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer).map_err(|error| {
            let over = error
                .get_ref()
                .is_some_and(|inner| inner.is::<crate::lzma::DictionaryOverLimit>());
            if over {
                std::io::Error::new(
                    std::io::ErrorKind::OutOfMemory,
                    format!(
                        "the stream needs a dictionary larger than {}; {BACKEND_HINT}",
                        largest_dictionary()
                    ),
                )
            } else {
                error
            }
        })
    }
}

struct Bzip2Streams<R: BufRead> {
    decoder: Option<bzip2::bufread::BzDecoder<R>>,
    streams: u64,
    decoded: u64,
}

impl<R: BufRead> Read for Bzip2Streams<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while let Some(decoder) = &mut self.decoder {
            let read = decoder.read(buffer)?;
            if read > 0 || buffer.is_empty() {
                self.decoded += read as u64;
                return Ok(read);
            }
            let mut input = self.decoder.take().expect("a decoder").into_inner();
            if input.fill_buf()?.is_empty() {
                return Ok(0);
            }
            if self.streams > self.decoded / BZIP2_OUTPUT_PER_EXTRA_STREAM {
                return Err(std::io::Error::other(format!(
                    "bzip2 has more streams than {BZIP2_OUTPUT_PER_EXTRA_STREAM} decoded bytes each; {BACKEND_HINT}"
                )));
            }
            self.streams += 1;
            self.decoder = Some(bzip2::bufread::BzDecoder::new(input));
        }
        Ok(0)
    }
}

fn checksum_matches(block: &[u8; BLOCK]) -> bool {
    let Ok(stored) = octal(&block[148..156]) else {
        return false;
    };
    let field = 148..156;
    let (mut unsigned, mut signed) = (0u64, 0i64);
    for (index, &byte) in block.iter().enumerate() {
        let byte = if field.contains(&index) { b' ' } else { byte };
        unsigned += u64::from(byte);
        signed += i64::from(byte as i8);
    }
    stored == unsigned || i64::try_from(stored) == Ok(signed)
}

fn octal(field: &[u8]) -> Field<u64> {
    let digits = field.trim_ascii_start();
    let end = digits
        .iter()
        .position(|&byte| byte == 0 || byte == b' ')
        .unwrap_or(digits.len());
    field_check(
        digits[end..].iter().all(|&byte| byte == 0 || byte == b' '),
        "tar numeric field has bytes after its terminator",
    )?;
    digits[..end].iter().try_fold(0u64, |value, &digit| {
        field_check(
            (b'0'..=b'7').contains(&digit),
            "tar numeric field is not octal",
        )?;
        value
            .checked_mul(8)
            .map(|value| value + u64::from(digit - b'0'))
            .ok_or(BadField("tar numeric field overflows"))
    })
}

fn number(field: &[u8]) -> Field<i64> {
    match field[0] {
        0x80 => field[1..].iter().try_fold(0i64, |value, &byte| {
            value
                .checked_mul(256)
                .map(|value| value | i64::from(byte))
                .ok_or(BadField("tar base-256 field overflows"))
        }),
        0xff => {
            let mut value = -1i64;
            for &byte in &field[1..] {
                field_check(value >= i64::MIN >> 8, "tar base-256 field overflows")?;
                value = (value << 8) | i64::from(byte);
            }
            Ok(value)
        }
        _ => i64::try_from(octal(field)?).map_err(|_| BadField("tar numeric field overflows")),
    }
}

fn size_field(field: &[u8]) -> Field<u64> {
    u64::try_from(number(field)?).map_err(|_| BadField("tar entry size is negative"))
}

fn text(field: &[u8]) -> &[u8] {
    &field[..field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len())]
}

fn utf8<'a>(bytes: &'a [u8], what: &str) -> Result<&'a str> {
    std::str::from_utf8(bytes).with_context(|| format!("tar {what} is not UTF-8; {BACKEND_HINT}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    V7,
    Ustar,
    Gnu,
}

fn format(block: &[u8; BLOCK]) -> Result<Format> {
    match (&block[257..263], &block[263..265]) {
        (b"ustar\0", _) => Ok(Format::Ustar),
        (b"ustar ", b" \0") => Ok(Format::Gnu),
        (magic, version) if magic.iter().chain(version).all(|&byte| byte == 0) => Ok(Format::V7),
        _ => bail!("tar header has an unknown magic"),
    }
}

#[derive(Clone, Copy)]
struct Time {
    seconds: i64,
    nanos: u32,
}

impl Time {
    fn system_time(self) -> Option<std::time::SystemTime> {
        let whole = std::time::Duration::from_secs(self.seconds.unsigned_abs());
        let epoch = std::time::UNIX_EPOCH;
        let seconds = if self.seconds >= 0 {
            epoch.checked_add(whole)
        } else {
            epoch.checked_sub(whole)
        };
        seconds?.checked_add(std::time::Duration::from_nanos(self.nanos.into()))
    }
}

fn pax_time(value: &str) -> Result<Time> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let seconds: i64 = whole.parse().context("tar pax mtime is not a number")?;
    ensure!(
        fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "tar pax mtime is not a number"
    );
    let digits: String = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(9)
        .collect();
    let mut nanos: u32 = digits.parse()?;
    let mut seconds = seconds;
    if whole.starts_with('-') && nanos > 0 {
        seconds = seconds.checked_sub(1).context("tar pax mtime overflows")?;
        nanos = 1_000_000_000 - nanos;
    }
    Ok(Time { seconds, nanos })
}

#[derive(Default)]
struct Extensions {
    pax: bool,
    path: Option<String>,
    linkpath: Option<String>,
    size: Option<u64>,
    mtime: Option<Time>,
    long_name: Option<Vec<u8>>,
    long_link: Option<Vec<u8>>,
}

impl Extensions {
    fn is_empty(&self) -> bool {
        !self.pax && self.long_name.is_none() && self.long_link.is_none()
    }
}

fn pax_records(data: &[u8]) -> Result<Vec<(&str, &[u8])>> {
    let mut records = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&byte| byte == b' ')
            .context("tar pax record has no length")?;
        let length: usize = utf8(&rest[..space], "pax record length")?
            .parse()
            .context("tar pax record length is not a number")?;
        ensure!(
            length > space + 1 && length <= rest.len() && rest[length - 1] == b'\n',
            "tar pax record has a wrong length"
        );
        let record = &rest[space + 1..length - 1];
        let equals = record
            .iter()
            .position(|&byte| byte == b'=')
            .context("tar pax record has no key")?;
        let key = utf8(&record[..equals], "pax key")?;
        ensure!(!key.is_empty(), "tar pax record has an empty key");
        records.push((key, &record[equals + 1..]));
        rest = &rest[length..];
    }
    Ok(records)
}

fn ignored_pax_key(key: &str) -> Result<bool> {
    ensure!(
        !key.starts_with("GNU.sparse."),
        "tar sparse files are not supported; {BACKEND_HINT}"
    );
    Ok(matches!(
        key,
        "atime" | "ctime" | "uid" | "gid" | "uname" | "gname" | "comment" | "charset"
    ) || key.contains('.'))
}

fn apply_pax(extensions: &mut Extensions, data: &[u8]) -> Result<()> {
    ensure!(!extensions.pax, "tar has two pax headers for one entry");
    extensions.pax = true;
    let mut seen = HashSet::new();
    for (key, value) in pax_records(data)? {
        ensure!(seen.insert(key), "tar pax header repeats {key:?}");
        match key {
            "path" => extensions.path = Some(utf8(value, "pax path")?.to_owned()),
            "linkpath" => extensions.linkpath = Some(utf8(value, "pax linkpath")?.to_owned()),
            "size" => {
                let value = utf8(value, "pax size")?;
                ensure!(
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
                    "tar pax size is not a number"
                );
                extensions.size = Some(value.parse().context("tar pax size overflows")?);
            }
            "mtime" => extensions.mtime = Some(pax_time(utf8(value, "pax mtime")?)?),
            "hdrcharset" => ensure!(
                value == b"ISO-IR 10646 2000 UTF-8",
                "tar pax header declares binary names; {BACKEND_HINT}"
            ),
            _ => ensure!(
                ignored_pax_key(key)?,
                "tar pax key {key:?} is not supported; {BACKEND_HINT}"
            ),
        }
    }
    Ok(())
}

fn check_global_pax(data: &[u8]) -> Result<()> {
    for (key, _) in pax_records(data)? {
        ensure!(
            ignored_pax_key(key)?,
            "tar global pax key {key:?} is not supported; {BACKEND_HINT}"
        );
    }
    Ok(())
}

struct Input {
    reader: Box<dyn Read + Send>,
    buffer: Vec<u8>,
}

impl Input {
    fn block(&mut self) -> Result<Option<[u8; BLOCK]>> {
        let mut block = [0u8; BLOCK];
        let mut filled = 0;
        while filled < BLOCK {
            let read = self.reader.read(&mut block[filled..]).context(READ_ERROR)?;
            if read == 0 {
                ensure!(filled == 0, "truncated tar header");
                return Ok(None);
            }
            filled += read;
        }
        counters::add(Counter::ArchiveBytesRead, BLOCK as u64);
        Ok(Some(block))
    }

    fn copy(&mut self, size: u64, mut sink: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        let mut remaining = padded;
        while remaining > 0 {
            let want = remaining.min(self.buffer.len() as u64) as usize;
            let read = self
                .reader
                .read(&mut self.buffer[..want])
                .context(READ_ERROR)?;
            ensure!(read > 0, "truncated tar entry");
            counters::add(Counter::ArchiveReads, 1);
            counters::add(Counter::ArchiveBytesRead, read as u64);
            let data_end = size.saturating_sub(padded - remaining).min(read as u64) as usize;
            if data_end > 0 {
                sink(&self.buffer[..data_end])?;
            }
            remaining -= read as u64;
        }
        Ok(())
    }

    fn fill(&mut self, buffer: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while filled < buffer.len() {
            let read = self
                .reader
                .read(&mut buffer[filled..])
                .context(READ_ERROR)?;
            ensure!(read > 0, "truncated tar entry");
            counters::add(Counter::ArchiveReads, 1);
            counters::add(Counter::ArchiveBytesRead, read as u64);
            filled += read;
        }
        Ok(())
    }

    fn skip_padding(&mut self, size: u64) -> Result<()> {
        let padding = (size.next_multiple_of(BLOCK as u64) - size) as usize;
        let mut buffer = [0u8; BLOCK];
        self.fill(&mut buffer[..padding])
    }

    fn data(&mut self, size: u64, what: &str) -> Result<Vec<u8>> {
        ensure!(size <= MAX_EXTENSION, "tar {what} is larger than 1 MiB");
        let mut data = Vec::with_capacity(size as usize);
        self.copy(size, |bytes| {
            data.extend_from_slice(bytes);
            Ok(())
        })?;
        Ok(data)
    }

    fn finish(&mut self) -> Result<()> {
        loop {
            let read = self.reader.read(&mut self.buffer).context(READ_ERROR)?;
            if read == 0 {
                return Ok(());
            }
            ensure!(
                self.buffer[..read].iter().all(|&byte| byte == 0),
                "tar has data after its end-of-archive marker"
            );
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Node {
    ImpliedDirectory,
    Directory,
    File,
    LinkedFile,
    HardLink,
    Symlink,
}

struct DirectoryMetadata {
    name: String,
    mode: u32,
    modified: Option<Time>,
}

struct Budget {
    limit: Option<u64>,
    free: u64,
    declared: u64,
    estimate: u64,
}

impl Budget {
    fn charge(&mut self, size: u64) -> Result<()> {
        self.declared = self
            .declared
            .checked_add(size)
            .context("archive output size overflows")?;
        self.estimate = self
            .estimate
            .saturating_add(size.div_ceil(4096).saturating_mul(4096));
        match self.limit {
            Some(limit) => ensure!(
                self.declared <= limit,
                crate::OverLimit(format!(
                    "archive expands to more than --max-extracted-bytes {limit}"
                ))
            ),
            None => ensure!(
                self.estimate <= self.free,
                crate::OverLimit(format!(
                    "archive needs more than the {} bytes free; pass --max-extracted-bytes to override",
                    self.free
                ))
            ),
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Output<'a, 'f> {
    root: &'a Path,
    writers: &'a Writers<'f>,
}

struct Extraction<'a, 'f> {
    output: Option<Output<'a, 'f>>,
    nodes: HashMap<String, (Node, u32)>,
    metadata_bytes: usize,
    created_directories: HashSet<String>,
    directories: Vec<DirectoryMetadata>,
    directory_index: HashMap<String, usize>,
    hard_links: Vec<(String, String)>,
    symlinks: Vec<(String, String)>,
    entries: Vec<Entry>,
    budget: Budget,
    tracker: &'a Tracker<'a>,
    selection: crate::Selection<'a>,
    placement: crate::placement::Placement<'a>,
    placed: crate::placement::PlacedNames,
    placed_any: bool,
    metadata: crate::Metadata,
}

fn leading_dots(raw: &str) -> usize {
    (raw.len() - entry_name(raw).len()) / 2
}

fn entry_name(raw: &str) -> &str {
    let mut name = raw;
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    name
}

impl<'a, 'f> Extraction<'a, 'f> {
    fn new(
        output: Option<Output<'a, 'f>>,
        budget: Budget,
        tracker: &'a Tracker<'a>,
        selection: crate::Selection<'a>,
        placement: crate::placement::Placement<'a>,
        metadata: crate::Metadata,
    ) -> Self {
        Extraction {
            output,
            nodes: HashMap::new(),
            metadata_bytes: 0,
            created_directories: HashSet::new(),
            directories: Vec::new(),
            directory_index: HashMap::new(),
            hard_links: Vec::new(),
            symlinks: Vec::new(),
            entries: Vec::new(),
            budget,
            tracker,
            selection,
            placement,
            placed: crate::placement::PlacedNames::default(),
            placed_any: false,
            metadata,
        }
    }

    fn ensure_each_selected_path_matches(&self) -> Result<()> {
        self.selection
            .ensure_each_path_matches(self.nodes.keys().map(String::as_str))?;
        self.placement.ensure_something_placed(self.placed_any)?;
        self.placement
            .ensure_prefix_is_a_folder(self.nodes.iter().map(|(name, (node, _))| {
                (
                    name.as_str(),
                    matches!(node, Node::Directory | Node::ImpliedDirectory),
                )
            }))
    }

    fn place(
        &mut self,
        name: &str,
        dots: usize,
        directory: bool,
        replaces: bool,
    ) -> Result<Option<(String, bool)>> {
        if !self.selection.selects(name) {
            return Ok(None);
        }
        self.placed_any |= self.placement.is_prefix(name);
        let Some(placed) = self.placement.place_after_dots(name, dots) else {
            return Ok(None);
        };
        self.placed_any = true;
        let replaces = if self.placement.strips() {
            self.placed
                .add_entry(placed, directory, self.nodes[name].1)?
        } else {
            replaces
        };
        Ok(Some((placed.to_owned(), replaces)))
    }

    fn root(&self) -> Option<&'a Path> {
        self.output.map(|output| output.root)
    }

    fn charge_name(&mut self, bytes: usize) -> Result<()> {
        self.metadata_bytes += bytes;
        ensure!(
            self.metadata_bytes <= MAX_METADATA,
            "tar names and link targets exceed 64 MiB"
        );
        Ok(())
    }

    fn insert_node(&mut self, name: &str, node: Node) -> Result<Option<Node>> {
        ensure!(
            self.nodes.len() < MAX_ENTRIES,
            "tar has more than {MAX_ENTRIES} entries"
        );
        self.charge_name(name.len())?;
        let id = self.nodes.len() as u32;
        Ok(match self.nodes.get_mut(name) {
            Some((earlier, _)) => Some(std::mem::replace(earlier, node)),
            None => {
                self.nodes.insert(name.to_owned(), (node, id));
                None
            }
        })
    }

    fn claim(&mut self, name: &str, node: Node) -> Result<bool> {
        let mut ancestor = name;
        while let Some(slash) = ancestor.rfind('/') {
            ancestor = &ancestor[..slash];
            match self.nodes.get(ancestor).map(|(node, _)| node) {
                Some(Node::ImpliedDirectory | Node::Directory) => break,
                Some(_) => bail!("tar entry {name:?} is inside a file or link"),
                None => {
                    self.insert_node(ancestor, Node::ImpliedDirectory)?;
                }
            }
        }
        match (self.insert_node(name, node)?, node) {
            (None, _) | (Some(Node::ImpliedDirectory), Node::Directory) => Ok(false),
            (Some(Node::Directory), Node::Directory) | (Some(Node::File), Node::File) => Ok(true),
            (Some(Node::ImpliedDirectory), _) => bail!("tar entry {name:?} is also a directory"),
            (Some(Node::LinkedFile), Node::File) => {
                bail!("tar replaces {name:?}, which an earlier hard link names")
            }
            _ => bail!("tar has two entries of different kinds named {name:?}"),
        }
    }

    fn create_parent(&mut self, name: &str) -> Result<()> {
        let (Some(root), Some(slash)) = (self.root(), name.rfind('/')) else {
            return Ok(());
        };
        let directory = &name[..slash];
        if self.created_directories.insert(directory.to_owned()) {
            fs::create_dir_all(root.join(directory))
                .with_context(|| format!("cannot create the folder of tar entry {name:?}"))?;
            counters::add(Counter::DirectoryCreateCalls, 1);
        }
        Ok(())
    }

    fn write_file(
        &mut self,
        input: &mut Input,
        name: &str,
        size: u64,
        mode: u32,
        modified: Option<Time>,
        replaces: bool,
    ) -> Result<()> {
        self.budget.charge(size)?;
        let Some(Output { root, writers }) = self.output else {
            return input.copy(size, |bytes| self.tracker.add_bytes(bytes.len() as u64));
        };
        self.create_parent(name)?;
        if replaces {
            writers.drain()?;
            fs::remove_file(root.join(name))?;
        }
        let mut job = Some((root.join(name), name.to_owned()));
        let mut stream: Option<SyncSender<Chunk>> = None;
        let modified = modified.and_then(Time::system_time);
        let mut remaining = size;
        loop {
            let mut chunk = writers.flight.take(remaining.min(CHUNK as u64) as usize);
            input.fill(&mut chunk.data)?;
            remaining -= chunk.data.len() as u64;
            self.tracker.check()?;
            if let Some(chunks) = &stream {
                chunks.send(chunk).map_err(|_| writers.stopped())?;
            } else {
                let (path, name) = job.take().expect("a job until it is sent");
                let rest = (remaining > 0).then(|| {
                    let (chunks, rest) = sync_channel(QUEUED_CHUNKS_PER_FILE);
                    stream = Some(chunks);
                    rest
                });
                writers.send(Job {
                    path,
                    name,
                    mode,
                    modified,
                    size,
                    first: chunk,
                    rest,
                })?;
            }
            if remaining == 0 {
                break;
            }
        }
        drop(stream);
        input.skip_padding(size)
    }

    fn create_links(&self) -> Result<()> {
        let Some(root) = self.root() else {
            return Ok(());
        };
        for (index, (name, target)) in self.hard_links.iter().enumerate() {
            if index % LINKS_PER_REPORT == 0 {
                self.tracker.report()?;
            }
            fs::hard_link(root.join(target), root.join(name)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    anyhow::anyhow!(
                        "tar hard link {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                    )
                } else {
                    anyhow::Error::from(error).context(format!("cannot create tar hard link {name:?}"))
                }
            })?;
        }
        for (index, (name, target)) in self.symlinks.iter().enumerate() {
            if index % LINKS_PER_REPORT == 0 {
                self.tracker.report()?;
            }
            crate::create_symlink("tar", root, name, target)?;
        }
        Ok(())
    }

    fn member(
        &mut self,
        input: &mut Input,
        block: &[u8; BLOCK],
        extensions: Extensions,
    ) -> Result<()> {
        let format = format(block)?;
        let kind = block[156];
        let raw_name = match (&extensions.path, &extensions.long_name) {
            (Some(_), Some(_)) => bail!("tar entry has both a pax path and a GNU long name"),
            (Some(path), None) => path.clone(),
            (None, Some(long)) => utf8(text(long), "long name")?.to_owned(),
            (None, None) => {
                let name = text(&block[..100]);
                let prefix = text(&block[345..500]);
                if format == Format::Ustar && !prefix.is_empty() {
                    format!("{}/{}", utf8(prefix, "name")?, utf8(name, "name")?)
                } else {
                    utf8(name, "name")?.to_owned()
                }
            }
        };
        let raw_target = match (&extensions.linkpath, &extensions.long_link) {
            (Some(_), Some(_)) => bail!("tar entry has both a pax linkpath and a GNU long link"),
            (Some(path), None) => path.clone(),
            (None, Some(long)) => utf8(text(long), "long link")?.to_owned(),
            (None, None) => utf8(text(&block[157..257]), "link target")?.to_owned(),
        };
        let size = match extensions.size {
            Some(size) => size,
            None => size_field(&block[124..136])?,
        };
        let mode = (octal(&block[100..108])? & 0o777) as u32;
        let modified = match extensions.mtime {
            Some(time) => Some(time),
            None => Some(Time {
                seconds: number(&block[136..148])?,
                nanos: 0,
            }),
        };
        let modified = self.metadata.time(modified);
        let mut name = entry_name(&raw_name);
        let dots = leading_dots(&raw_name);
        let directory = match kind {
            b'5' => true,
            b'0' | 0 | b'7' if name.ends_with('/') => {
                ensure!(size == 0, "tar file entry {name:?} ends with a slash");
                true
            }
            _ => false,
        };
        let mode = if directory {
            mode
        } else {
            self.metadata.file_mode(Some(mode)).unwrap_or(mode)
        };
        if directory {
            name = name.trim_end_matches('/');
            if name.is_empty() || name == "." {
                ensure!(size == 0, "tar directory entry has data");
                return Ok(());
            }
        }
        safe_path(name)?;
        let name = name.to_owned();
        match kind {
            _ if directory => {
                ensure!(size == 0, "tar directory {name:?} has data");
                let replaces = self.claim(&name, Node::Directory)?;
                let Some((name, _)) = self.place(&name, dots, true, replaces)? else {
                    return Ok(());
                };
                if self.output.is_some() {
                    self.create_parent(&format!("{name}/"))?;
                }
                let metadata = DirectoryMetadata {
                    name: name.clone(),
                    mode,
                    modified,
                };
                match self.directory_index.get(&name) {
                    Some(&index) => self.directories[index] = metadata,
                    None => {
                        self.directory_index
                            .insert(name.clone(), self.directories.len());
                        self.directories.push(metadata);
                    }
                }
            }
            b'0' | 0 | b'7' => {
                let replaces = self.claim(&name, Node::File)?;
                let Some((name, replaces)) = self.place(&name, dots, false, replaces)? else {
                    return input.copy(size, |_| Ok(()));
                };
                self.write_file(input, &name, size, mode, modified, replaces)?;
                self.entries.push(Entry {
                    name,
                    size,
                    directory: false,
                });
                return match self.output {
                    Some(_) => self.tracker.check(),
                    None => self.tracker.file_done(),
                };
            }
            b'1' => {
                ensure!(size == 0, "tar hard link {name:?} has data");
                let target = entry_name(&raw_target).to_owned();
                let target_dots = leading_dots(&raw_target);
                safe_path(&target)?;
                ensure!(
                    matches!(
                        self.nodes.get(&target).map(|(node, _)| node),
                        Some(Node::File | Node::LinkedFile | Node::HardLink)
                    ),
                    "tar hard link {name:?} does not name an earlier file"
                );
                self.charge_name(target.len())?;
                if let Some((node @ Node::File, _)) = self.nodes.get_mut(&target) {
                    *node = Node::LinkedFile;
                }
                self.claim(&name, Node::HardLink)?;
                let placed_target = self
                    .placement
                    .place_after_dots(&target, target_dots)
                    .filter(|_| self.selection.selects(&target))
                    .map(str::to_owned);
                let Some((name, _)) = self.place(&name, dots, false, false)? else {
                    return Ok(());
                };
                let target = placed_target.with_context(|| {
                    format!("tar hard link {name:?} needs {target:?}, which the selection or strip leaves out")
                })?;
                self.create_parent(&name)?;
                self.hard_links.push((name.clone(), target));
            }
            b'2' => {
                ensure!(size == 0, "tar symlink {name:?} has data");
                crate::ensure_link_stays_inside("tar", &name, &raw_target)?;
                self.charge_name(raw_target.len())?;
                self.claim(&name, Node::Symlink)?;
                let Some((name, _)) = self.place(&name, dots, false, false)? else {
                    return Ok(());
                };
                crate::ensure_link_stays_inside("tar", &name, &raw_target)?;
                self.create_parent(&name)?;
                self.symlinks.push((name.clone(), raw_target));
            }
            b'3' | b'4' | b'6' => {
                bail!("tar entry {name:?} is a device or FIFO, which is not extracted")
            }
            other => bail!(
                "tar entry type {:?} is not supported; {BACKEND_HINT}",
                char::from(other)
            ),
        }
        self.entries.push(Entry {
            name,
            size: 0,
            directory,
        });
        if !directory {
            self.tracker.file_done()?;
        }
        Ok(())
    }
}

fn parse(extraction: &mut Extraction, input: &mut Input) -> Result<()> {
    let mut extensions = Extensions::default();
    loop {
        extraction.tracker.check()?;
        let block = input
            .block()?
            .context("tar ends without its end-of-archive marker")?;
        if block == [0; BLOCK] {
            ensure!(extensions.is_empty(), "tar ends after an extended header");
            if let Some(next) = input.block()? {
                ensure!(next == [0; BLOCK], "tar has a header after a zero block");
            }
            input.finish()?;
            break;
        }
        ensure!(checksum_matches(&block), "tar header checksum mismatch");
        let size = size_field(&block[124..136])?;
        match block[156] {
            b'x' => {
                let data = input.data(size, "pax header")?;
                apply_pax(&mut extensions, &data)?;
            }
            b'g' => {
                ensure!(
                    extensions.is_empty(),
                    "tar global header follows an extended header"
                );
                check_global_pax(&input.data(size, "global pax header")?)?;
            }
            b'L' => {
                ensure!(
                    extensions.long_name.is_none(),
                    "tar has two GNU long names for one entry"
                );
                extensions.long_name = Some(input.data(size, "GNU long name")?);
            }
            b'K' => {
                ensure!(
                    extensions.long_link.is_none(),
                    "tar has two GNU long links for one entry"
                );
                extensions.long_link = Some(input.data(size, "GNU long link")?);
            }
            _ => extraction.member(input, &block, std::mem::take(&mut extensions))?,
        }
    }
    Ok(())
}

/// Verify and optionally extract a plain or compressed tar archive into a new
/// directory. It restores files, directories, hard links, Unix modes,
/// modification times and symlinks that stay inside the destination.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    read_part(path, &Part::whole(path)?, destination, options)
}

pub(crate) fn read_part(
    path: &Path,
    part: &Part,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let compression = sniff_part(part)?.context("not a tar archive")?;
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    let placement = crate::placement::Placement::new(options.strip)?;
    let staging = destination
        .map(|destination| {
            crate::staging::stage(path, destination, options.existing, options.metadata)
        })
        .transpose()?;
    let free = match (&staging, options.max_output_bytes) {
        (Some(staging), None) => {
            let folder = staging.path();
            crate::available_bytes(folder)
                .with_context(|| format!("cannot measure free space at {}", folder.display()))?
        }
        _ => u64::MAX,
    };
    let budget = Budget {
        limit: options.max_output_bytes,
        free,
        declared: 0,
        estimate: 0,
    };
    let mut input = Input {
        reader: decode(
            BufReader::with_capacity(COPY_BUFFER, part.open()?),
            compression,
        )?,
        buffer: vec![0; COPY_BUFFER],
    };
    let tracker = Tracker::new(options.control, None, None);
    let count_written = |written: Written| match written {
        Written::Bytes(bytes) => tracker.add_bytes(bytes),
        Written::File => tracker.file_done(),
    };
    let Some(staging) = staging else {
        let mut extraction = Extraction::new(
            None,
            budget,
            &tracker,
            selection,
            placement,
            options.metadata,
        );
        parse(&mut extraction, &mut input)?;
        extraction.ensure_each_selected_path_matches()?;
        extraction.tracker.finish();
        return Ok(extraction.entries);
    };
    let count = options.writers.unwrap_or(DEFAULT_WRITERS);
    ensure!(
        (1..=64).contains(&count),
        "tar writers must be between 1 and 64"
    );
    let flight = InFlight::default();
    let lane_count = if ONE_WRITER_PER_FOLDER { count } else { 1 };
    let (lanes, queues): (Vec<_>, Vec<_>) = (0..lane_count)
        .map(|_| {
            let (lane, queue) = sync_channel(QUEUED_FILES);
            (lane, Mutex::new(queue))
        })
        .unzip();
    let writers = Writers {
        lanes: Mutex::new(lanes),
        flight: &flight,
        pending: Mutex::new(0),
        idle: Condvar::new(),
        failure: Mutex::new(None),
        callback_panic: Mutex::new(None),
    };
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..count)
            .map(|index| {
                let queue = &queues[index % lane_count];
                let writers = &writers;
                let count_written = &count_written;
                scope.spawn(move || writers.work(queue, count_written))
            })
            .collect();
        let closing = ClosesOnDrop(&writers);
        let output = Output {
            root: staging.path(),
            writers: &writers,
        };
        let mut extraction = Extraction::new(
            Some(output),
            budget,
            &tracker,
            selection,
            placement,
            options.metadata,
        );
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            parse(&mut extraction, &mut input)
        }));
        drop(closing);
        let joined: Vec<_> = threads.into_iter().map(|thread| thread.join()).collect();
        if let Some(panic) = writers.callback_panic.lock().unwrap().take() {
            std::panic::resume_unwind(panic);
        }
        parsed.unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
        ensure!(joined.iter().all(Result::is_ok), "a tar writer panicked");
        writers.check()?;
        extraction.tracker.finish();
        extraction.tracker.check()?;
        extraction.ensure_each_selected_path_matches()?;
        publish(&extraction, &staging)?;
        Ok(extraction.entries)
    })
}

struct ClosesOnDrop<'w, 'f>(&'w Writers<'f>);

impl Drop for ClosesOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn publish(extraction: &Extraction, staging: &crate::staging::Staging) -> Result<()> {
    extraction.create_links()?;
    let merged = crate::later_spellings_of_same_directory(
        staging.path(),
        extraction
            .directories
            .iter()
            .map(|directory| Some(directory.name.as_str())),
    )?;
    let directories: Vec<crate::Directory> = extraction
        .directories
        .iter()
        .enumerate()
        .filter(|(index, _)| !merged.contains(index))
        .map(|(_, directory)| crate::Directory {
            name: &directory.name,
            mode: extraction.metadata.directory_mode(Some(directory.mode)),
            modified: directory.modified.and_then(Time::system_time),
        })
        .collect();
    staging.publish(&directories)
}

#[cfg(test)]
mod in_flight_tests {
    use super::*;

    #[test]
    fn chunks_in_flight_never_exceed_the_budget_and_release_on_drop() {
        let flight = InFlight::default();
        let peak = Mutex::new(0usize);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for round in 0..200 {
                        let held = flight.take(CHUNK + round % 2);
                        let now = *flight.bytes.lock().unwrap();
                        let mut peak = peak.lock().unwrap();
                        *peak = (*peak).max(now);
                        drop(peak);
                        std::thread::yield_now();
                        drop(held);
                    }
                });
            }
        });
        assert!(*peak.lock().unwrap() <= BYTES_IN_FLIGHT);
        assert_eq!(*flight.bytes.lock().unwrap(), 0);
        let spare = flight.spare.lock().unwrap();
        assert!(!spare.is_empty() && spare.len() <= BYTES_IN_FLIGHT / CHUNK);
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn every_string(alphabet: &[u8], longest: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut previous = vec![String::new()];
        for _ in 0..longest {
            previous = previous
                .iter()
                .flat_map(|text| {
                    alphabet
                        .iter()
                        .map(move |byte| format!("{text}{}", *byte as char))
                })
                .collect();
            all.extend(previous.iter().cloned());
        }
        all
    }

    fn decimal_nanos(text: &str) -> Option<i128> {
        let (negative, body) = match text.strip_prefix('-') {
            Some(body) => (true, body),
            None => (false, text),
        };
        let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
        let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || !digits(whole) || !digits(fraction) {
            return None;
        }
        let fraction: String = fraction
            .chars()
            .chain(std::iter::repeat('0'))
            .take(9)
            .collect();
        let nanos = whole.parse::<i128>().ok()? * 1_000_000_000 + fraction.parse::<i128>().ok()?;
        Some(if negative { -nanos } else { nanos })
    }

    #[test]
    fn pax_times_equal_their_decimal_value_on_every_short_string() {
        let mut strings = every_string(b"-0.59", 8);
        strings
            .extend(["1.0000000009", "-1.0000000009", "-9223372036854775808.5"].map(String::from));
        for text in strings {
            match (pax_time(&text), decimal_nanos(&text)) {
                (Ok(time), Some(nanos)) => {
                    assert!(time.nanos < 1_000_000_000, "{text:?}");
                    assert_eq!(
                        i128::from(time.seconds) * 1_000_000_000 + i128::from(time.nanos),
                        nanos,
                        "{text:?}"
                    );
                }
                (Err(_), None) => {}
                (Err(_), Some(_)) if text.starts_with("-9223372036854775808.") => {}
                (time, nanos) => panic!("{text:?}: {:?} against {nanos:?}", time.is_ok()),
            }
        }
    }

    #[test]
    fn pax_records_split_back_into_their_exact_bytes_on_every_short_string() {
        for text in every_string(b"456 =\na", 7) {
            let Ok(records) = pax_records(text.as_bytes()) else {
                continue;
            };
            let mut rebuilt = Vec::new();
            for (key, value) in records {
                assert!(!key.is_empty() && !key.contains('='), "{text:?}");
                let body = 1 + key.len() + 1 + value.len() + 1;
                let length = (1..=3)
                    .map(|digits| body + digits)
                    .find(|length| length.to_string().len() + body == *length)
                    .unwrap();
                rebuilt.extend(format!("{length} {key}=").bytes());
                rebuilt.extend(value);
                rebuilt.push(b'\n');
            }
            assert_eq!(rebuilt, text.as_bytes(), "{text:?}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn base_256(value: i64) -> [u8; 12] {
        let mut field = if value < 0 { [0xff; 12] } else { [0; 12] };
        field[0] = if value < 0 { 0xff } else { 0x80 };
        field[4..].copy_from_slice(&value.to_be_bytes());
        field
    }

    #[kani::proof]
    #[kani::unwind(13)]
    fn numeric_fields_never_panic_on_any_bytes() {
        let bytes: [u8; 12] = kani::any();
        let length: usize = kani::any();
        kani::assume((1..=12).contains(&length));
        let field = &bytes[..length];
        let _ = octal(field);
        if let Ok(size) = size_field(field) {
            assert!(i64::try_from(size).is_ok());
        }
    }

    #[kani::proof]
    #[kani::unwind(13)]
    fn every_value_reads_back_from_its_encoding() {
        let value: i64 = kani::any();
        assert!(matches!(number(&base_256(value)), Ok(read) if read == value));
        let small: u64 = kani::any();
        kani::assume(small < 8u64.pow(11));
        let mut field = [0u8; 12];
        for (index, byte) in field[..11].iter_mut().enumerate() {
            *byte = b'0' + ((small >> (3 * (10 - index))) & 7) as u8;
        }
        assert!(matches!(octal(&field), Ok(read) if read == small));
    }

    #[kani::proof]
    #[kani::unwind(13)]
    fn base_256_fields_have_one_spelling() {
        let field: [u8; 12] = kani::any();
        kani::assume(field[0] == 0x80 || field[0] == 0xff);
        if let Ok(value) = number(&field) {
            assert!(base_256(value) == field);
        }
    }
}

#[cfg(test)]
mod bzip2_stream_tests {
    use super::*;

    fn stream(bytes: usize) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        encoder.write_all(&vec![b'z'; bytes]).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn each_stream_after_the_first_needs_its_share_of_output() {
        for first in [0, 1, 4095, 4096, 8191, 8192] {
            for count in 1..=4u64 {
                let mut input = stream(first);
                for _ in 1..count {
                    input.extend(stream(0));
                }
                let mut output = Vec::new();
                let read = decode(std::io::Cursor::new(input), Compression::Bzip2)
                    .unwrap()
                    .read_to_end(&mut output);
                let allowed = count - 1 <= first as u64 / BZIP2_OUTPUT_PER_EXTRA_STREAM;
                assert_eq!(read.is_ok(), allowed, "{count} streams after {first} bytes");
                if allowed {
                    assert_eq!(output.len(), first);
                }
            }
        }
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    #[test]
    fn skippable_frames_are_passed_over_to_the_frame_that_names_the_format() {
        let skippable = |size: u32| {
            let mut frame = vec![0x5A, 0x2A, 0x4D, 0x18];
            frame.extend_from_slice(&size.to_le_bytes());
            frame.extend(std::iter::repeat_n(7, size as usize));
            frame
        };
        let lz4 = [0x04, 0x22, 0x4D, 0x18, 0x60, 0x70];
        let zstd = [0x28, 0xB5, 0x2F, 0xFD, 0, 0];
        let work = tempfile::tempdir().unwrap();
        for (bytes, expected) in [
            ([skippable(3), lz4.to_vec()].concat(), Compression::Lz4),
            (
                [skippable(0), skippable(9), lz4.to_vec()].concat(),
                Compression::Lz4,
            ),
            ([skippable(3), zstd.to_vec()].concat(), Compression::Zstd),
            (skippable(4), Compression::Zstd),
            ([skippable(1), b"ab".to_vec()].concat(), Compression::Zstd),
            (vec![0x02, 0x21, 0x4C, 0x18, 0, 0], Compression::Lz4),
            (b"plain".to_vec(), Compression::None),
            (Vec::new(), Compression::None),
        ] {
            let path = work.path().join("sample");
            fs::write(&path, &bytes).unwrap();
            assert!(
                compression_by_signature(&path).unwrap() == expected,
                "{bytes:02x?}"
            );
        }
    }
}

#[cfg(test)]
mod sniff_tests {
    use super::*;

    #[test]
    fn a_tar_in_lz4_is_found_behind_a_first_block_larger_than_a_mebibyte() {
        let mut tar = vec![0u8; 2 << 20];
        tar[..3].copy_from_slice(b"big");
        tar[257..263].copy_from_slice(b"ustar\0");
        let mut frame = 0x184D_2204u32.to_le_bytes().to_vec();
        let descriptor = [0x60, 0x70];
        frame.extend_from_slice(&descriptor);
        frame.push((crate::lz4::xxh32(&descriptor) >> 8) as u8);
        frame.extend_from_slice(&(tar.len() as u32 | 1 << 31).to_le_bytes());
        frame.extend_from_slice(&tar);
        frame.extend_from_slice(&0u32.to_le_bytes());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.tar.lz4");
        std::fs::write(&path, frame).unwrap();
        assert!(matches!(sniff(&path).unwrap(), Some(Compression::Lz4)));
    }
}

#[cfg(test)]
mod lzip_tests {
    use super::*;
    use crate::lzma::lzma_file_and_lzip_tests::{
        fixture, lzip_member, lzma_file_of_unknown_size, pax_short, sample, with_size,
    };

    fn liblzma_lzma(mut input: &[u8]) -> Result<Vec<u8>> {
        if let Some(field) = input.get(1..5) {
            let dictionary = u32::from_le_bytes(field.try_into().unwrap());
            ensure_dictionary_fits("LZMA", u64::from(dictionary))?;
        }
        let mut decoder = liblzma::bufread::XzDecoder::new_stream(
            &mut input,
            liblzma::stream::Stream::new_lzma_decoder(DECODER_MEMORY)?,
        );
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }

    fn liblzma_lzip(mut input: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            if let Some(&coded) = input.get(5) {
                ensure_dictionary_fits("lzip", crate::lzma::lzip_dictionary(coded))?;
            }
            let mut decoder = liblzma::bufread::XzDecoder::new_stream(
                input,
                liblzma::stream::Stream::new_lzip_decoder(DECODER_MEMORY, 0)?,
            );
            decoder.read_to_end(&mut out)?;
            input = decoder.into_inner();
            if input.is_empty() {
                return Ok(out);
            }
            ensure!(
                b"LZIP".starts_with(&input[..input.len().min(4)]),
                "lzip has data after its last member"
            );
        }
    }

    fn ours(stream: &[u8], compression: Compression) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        decode(std::io::Cursor::new(stream.to_vec()), compression)?.read_to_end(&mut out)?;
        Ok(out)
    }

    fn over_memory_message(result: &Result<Vec<u8>>) -> Option<String> {
        let error = result.as_ref().err()?;
        let io = error.downcast_ref::<std::io::Error>()?;
        (io.kind() == std::io::ErrorKind::OutOfMemory).then(|| io.to_string())
    }

    fn assert_decodes_as_the_liblzma_reader_did(
        stream: &[u8],
        compression: Compression,
        what: &str,
    ) -> Option<Vec<u8>> {
        let theirs = match compression {
            Compression::Lzma => liblzma_lzma(stream),
            _ => liblzma_lzip(stream),
        };
        let ours = ours(stream, compression);
        assert_eq!(
            over_memory_message(&ours),
            over_memory_message(&theirs),
            "{what}"
        );
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => {
                assert!(ours == theirs, "{what}");
                Some(ours)
            }
            (Err(_), Err(_)) => None,
            (ours, theirs) => panic!(
                "{what}: zipper {:?}, liblzma {:?}",
                ours.map(|out| out.len()),
                theirs.map(|out| out.len())
            ),
        }
    }

    #[test]
    fn every_lzma_and_lzip_stream_in_the_corpora_decodes_as_the_liblzma_reader_did() {
        let original = pax_short();
        let mut checked = 0;
        for (name, compression) in [
            ("tar-corpus/python-pax-lzip.tar.lz", Compression::Lzip),
            ("lzip/lzip-9.tar.lz", Compression::Lzip),
            ("stream/hello.txt.lz", Compression::Lzip),
            ("lzma/empty.lz", Compression::Lzip),
            ("lzma/pax-short-members.tar.lz", Compression::Lzip),
            ("parity/archives/hello.txt.lzma", Compression::Lzma),
        ]
        .map(|(name, compression)| (name.to_string(), compression))
        .into_iter()
        .chain((0..=9).map(|level| (format!("lzma/pax-short-{level}.tar.lz"), Compression::Lzip)))
        {
            let decoded =
                assert_decodes_as_the_liblzma_reader_did(&fixture(&name), compression, &name);
            let decoded = decoded.unwrap_or_else(|| panic!("{name} does not decode"));
            if name.starts_with("lzma/pax-short-") {
                assert!(decoded == original, "{name}");
            }
            checked += 1;
        }
        assert_eq!(checked, 16);
    }

    #[test]
    fn dictionaries_over_the_decoder_budget_are_refused_with_the_liblzma_readers_messages() {
        let data = sample(2_000, 21);
        let lzma = with_size(lzma_file_of_unknown_size(&data, 6), data.len() as u64);
        let mut over = 0;
        let largest = LARGEST_DICTIONARY as u32;
        for dictionary in [
            0,
            4096,
            64 << 20,
            largest - 1,
            largest,
            largest + 1,
            u32::MAX,
        ] {
            let mut changed = lzma.clone();
            changed[1..5].copy_from_slice(&dictionary.to_le_bytes());
            let what = format!("LZMA dictionary {dictionary}");
            let decoded =
                assert_decodes_as_the_liblzma_reader_did(&changed, Compression::Lzma, &what);
            assert_eq!(decoded.is_some(), dictionary <= largest, "{what}");
            over += usize::from(decoded.is_none());
        }
        let member = lzip_member(&data, 0x0C, 6);
        for coded in 0..=u8::MAX {
            for stream in [
                [&member[..5], &[coded], &member[6..]].concat(),
                [&member[..], &member[..5], &[coded], &member[6..]].concat(),
            ] {
                let what = format!(
                    "lzip coded dictionary {coded:#x} in member {}",
                    stream.len() / member.len()
                );
                let decoded =
                    assert_decodes_as_the_liblzma_reader_did(&stream, Compression::Lzip, &what);
                over += usize::from(decoded.is_none());
            }
        }
        assert!(over > 2 + 2 * 24, "{over}");
    }

    #[test]
    fn coded_dictionary_sizes_read_as_lzip_reports_them() {
        for (coded, size) in [
            (0x0C, 4 << 10),
            (0x8D, 6 << 10),
            (0x71, 104 << 10),
            (0xD4, 640 << 10),
            (0x1C, 256 << 20),
            (0x1D, 512 << 20),
        ] {
            assert_eq!(crate::lzma::lzip_dictionary(coded), size, "{coded:#x}");
        }
    }
}

#[cfg(test)]
mod xz_tests {
    use super::*;
    use liblzma::stream::{Action, Check, Filters, LzmaOptions, Status, Stream};

    fn encode(chain: &[(u64, Vec<u8>)], data: &[u8], check: Check) -> Vec<u8> {
        let mut filters = Filters::new();
        for (id, properties) in chain {
            match *id {
                0x03 => filters.delta_properties(properties),
                0x04 => filters.x86_properties(properties),
                other => panic!("not a filter here: {other:#x}"),
            }
            .unwrap();
        }
        filters.lzma2(&LzmaOptions::new_preset(0).unwrap());
        let mut encoder = Stream::new_stream_encoder(&filters, check).unwrap();
        let mut out = Vec::with_capacity(data.len() + 4096);
        encoder.process_vec(data, &mut out, Action::Run).unwrap();
        while encoder.process_vec(&[], &mut out, Action::Finish).unwrap() != Status::StreamEnd {
            out.reserve(4096);
        }
        out
    }

    fn with_dictionary_byte(mut stream: Vec<u8>, byte: u8) -> Vec<u8> {
        let block = 12;
        let size = (usize::from(stream[block]) + 1) * 4;
        let filter = (block..block + size)
            .find(|&at| stream[at..at + 2] == [0x21, 0x01])
            .unwrap();
        stream[filter + 2] = byte;
        let check = crc32fast::hash(&stream[block..block + size - 4]);
        stream[block + size - 4..block + size].copy_from_slice(&check.to_le_bytes());
        stream
    }

    fn ours(stream: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        decode(std::io::Cursor::new(stream.to_vec()), Compression::Xz)
            .unwrap()
            .read_to_end(&mut out)
            .map(|_| out)
    }

    fn liblzma_over_limit(stream: &[u8]) -> bool {
        let mut decoder = liblzma::bufread::XzDecoder::new_stream(
            stream,
            Stream::new_auto_decoder(DECODER_MEMORY, liblzma::stream::CONCATENATED).unwrap(),
        );
        let error = decoder.read_to_end(&mut Vec::new()).err();
        error.is_some_and(|error| {
            error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<liblzma::stream::Error>())
                .is_some_and(|inner| matches!(inner, liblzma::stream::Error::MemLimit))
        })
    }

    #[test]
    fn xz_dictionaries_reach_the_memory_limit_exactly_where_liblzma_does() {
        let data = b"tar member bytes ".repeat(64);
        for chain in [
            Vec::new(),
            vec![(0x03, vec![3])],
            vec![(0x04, Vec::new()), (0x03, vec![0])],
        ] {
            for byte in 0..=40 {
                let stream = with_dictionary_byte(encode(&chain, &data, Check::Crc64), byte);
                let result = ours(&stream);
                let over = result
                    .as_ref()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::OutOfMemory);
                assert_eq!(over, liblzma_over_limit(&stream), "{chain:?}, byte {byte}");
                match result {
                    Err(error) if over => assert!(
                        error.to_string().contains(&format!(
                            "the stream needs a dictionary larger than {}; {BACKEND_HINT}",
                            largest_dictionary()
                        )),
                        "{chain:?}, byte {byte}: {error}"
                    ),
                    result => assert_eq!(result.unwrap(), data, "{chain:?}, byte {byte}"),
                }
            }
        }
    }

    #[test]
    fn an_xz_check_zipper_cannot_verify_is_refused() {
        let data = b"checked bytes".repeat(10);
        let mut stream = encode(&[], &data, Check::Crc32);
        let unverifiable = 0x02;
        stream[7] = unverifiable;
        let header_check = crc32fast::hash(&stream[6..8]);
        stream[8..12].copy_from_slice(&header_check.to_le_bytes());
        let footer = stream.len() - 12;
        stream[footer + 9] = unverifiable;
        let footer_check = crc32fast::hash(&stream[footer + 4..footer + 10]);
        stream[footer..footer + 4].copy_from_slice(&footer_check.to_le_bytes());
        let error = ours(&stream).unwrap_err();
        assert!(
            error.to_string().contains("xz check type is not supported"),
            "{error}"
        );
    }
}
