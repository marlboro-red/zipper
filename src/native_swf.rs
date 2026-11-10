//! In-process extraction of Flash SWF files, as 7-Zip extracts them. An
//! uncompressed SWF extracts each tag before its end tag as one file, named
//! by its position and tag code. A SWF compressed with zlib or LZMA extracts
//! to the one uncompressed SWF it holds. See `docs/reference/swf-reader.md`.
use crate::output_name::{archive_file_name, default_name, named, safe_output_name};
use crate::tree;
pub use crate::tree::ReadOptions;
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check, progress::Tracker};
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, BufRead, BufReader, Read, Seek, Write},
    path::Path,
};

const HEADER: usize = 8;
const LZMA_FIELDS: usize = 9;
const VERSION_LIMIT: u8 = 64;
const SHORT_TAG: usize = 2;
const LONG_LENGTH_FIELD: usize = 4;
const LONG_LENGTH: u16 = 0x3F;
const CODE_SHIFT: u16 = 6;
const END_TAG: u16 = 0;
const RECT_FIELD_BITS: usize = 5;
const RECT_FIELDS: usize = 4;
const FRAME_RATE_AND_COUNT: usize = 4;
const MOVIE_HEADER_MAX: usize = 21;
const LARGEST_WINDOW: u64 = 256 * 1024 * 1024;
const FILE_MODE: u32 = 0o666;
const MAX_TAGS: u64 = 1_000_000;
// EXTERNAL(7-Zip 26.05): an uncompressed SWF that declares more than 512 MiB does not open.
const UNCOMPRESSED_LIMIT: u32 = 1 << 29;
const ALLOCATION_UNIT: u64 = 4096;
const COPY_BUFFER: usize = 1024 * 1024;
const UNCOMPRESSED_SUFFIX: &str = "~.swf";
const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    Zlib,
    Lzma,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    compression: Compression,
    length: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LzmaFields {
    packed: u32,
    properties: u8,
    dictionary: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TagStart {
    code: u16,
    short_length: Option<u32>,
}

fn header(bytes: [u8; HEADER]) -> Field<Header> {
    let compression = match bytes[0] {
        b'F' => Compression::None,
        b'C' => Compression::Zlib,
        b'Z' => Compression::Lzma,
        _ => return Err(BadField("not a SWF file")),
    };
    field_check(&bytes[1..3] == b"WS", "not a SWF file")?;
    field_check(
        bytes[3] < VERSION_LIMIT,
        "SWF version is 64 or more, which 7-Zip does not open",
    )?;
    let length = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let shortest = match compression {
        Compression::Lzma => HEADER + LZMA_FIELDS,
        _ => HEADER,
    };
    field_check(
        length >= shortest as u32,
        "SWF declares a length shorter than its header, which 7-Zip does not open",
    )?;
    Ok(Header {
        compression,
        length,
    })
}

fn lzma_fields(bytes: [u8; LZMA_FIELDS]) -> LzmaFields {
    LzmaFields {
        packed: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        properties: bytes[4],
        dictionary: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
    }
}

// SPEC(SWF 19, SWF header and RECT): the frame size holds a 5-bit field width and four fields
// of that width, padded to a byte, and the frame rate and frame count follow in two bytes each.
fn movie_header_length(first: u8) -> usize {
    let width = usize::from(first >> 3);
    (RECT_FIELD_BITS + RECT_FIELDS * width).div_ceil(8) + FRAME_RATE_AND_COUNT
}

// SPEC(SWF 19, RECORDHEADER): a tag code takes the top 10 bits, and a length of 0x3F means a
// 32-bit length follows.
fn tag_start(pair: u16) -> TagStart {
    let length = pair & LONG_LENGTH;
    TagStart {
        code: pair >> CODE_SHIFT,
        short_length: (length != LONG_LENGTH).then_some(u32::from(length)),
    }
}

fn tag_end(position: u64, header: u64, length: u32, declared: u32) -> Field<u64> {
    position
        .checked_add(header)
        .and_then(|start| start.checked_add(u64::from(length)))
        .filter(|&end| end <= u64::from(declared))
        .ok_or(BadField("SWF tag runs past the length the file declares"))
}

fn lzma_window(fields: &LzmaFields, decoded: u64) -> u64 {
    u64::from(fields.dictionary).min(decoded)
}

/// Returns whether the file starts with a SWF signature of a version 7-Zip
/// opens: `FWS`, `CWS` or `ZWS`, and a version byte below 64.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut bytes = Vec::with_capacity(4);
    crate::volumes::open(path)?
        .take(4)
        .read_to_end(&mut bytes)?;
    Ok(matches!(bytes[..], [b'F' | b'C' | b'Z', b'W', b'S', version] if version < VERSION_LIMIT))
}

fn read_exactly(input: &mut impl Read, bytes: &mut [u8], what: &str) -> Result<()> {
    input.read_exact(bytes).map_err(|error| match error.kind() {
        io::ErrorKind::UnexpectedEof => anyhow::anyhow!("the SWF {what} is truncated"),
        _ => error.into(),
    })
}

struct Budget {
    limit: Option<u64>,
    free: u64,
    declared: u64,
    estimate: u64,
}

impl Budget {
    fn new(staging: Option<&crate::staging::Staging>, limit: Option<u64>) -> Result<Self> {
        let free = match (staging, limit) {
            (Some(staging), None) => crate::available_bytes(staging.path()).with_context(|| {
                format!("cannot measure free space at {}", staging.path().display())
            })?,
            _ => u64::MAX,
        };
        Ok(Budget {
            limit,
            free,
            declared: 0,
            estimate: 0,
        })
    }

    fn charge(&mut self, size: u64) -> Result<()> {
        self.declared = self.declared.saturating_add(size);
        self.estimate = self.estimate.saturating_add(
            size.div_ceil(ALLOCATION_UNIT)
                .saturating_mul(ALLOCATION_UNIT),
        );
        match self.limit {
            Some(limit) => ensure!(
                self.declared <= limit,
                crate::OverLimit(format!(
                    "the SWF file expands to more than --max-extracted-bytes {limit}"
                ))
            ),
            None => ensure!(
                self.estimate <= self.free,
                crate::OverLimit(format!(
                    "the SWF file needs more than the {} bytes free; pass --max-extracted-bytes to override",
                    self.free
                ))
            ),
        }
        Ok(())
    }
}

struct TagWriter<'a> {
    staging: Option<crate::staging::Staging>,
    selection: crate::Selection<'a>,
    tracker: Tracker<'a>,
    budget: Budget,
    every_name: Option<String>,
    entries: Vec<Entry>,
    buffer: Vec<u8>,
    mode: u32,
}

impl TagWriter<'_> {
    fn copy(
        &mut self,
        body: &mut impl Read,
        length: u32,
        mut file: Option<&mut std::fs::File>,
    ) -> Result<()> {
        let mut left = u64::from(length);
        while left > 0 {
            let want = left.min(self.buffer.len() as u64) as usize;
            let read = body.read(&mut self.buffer[..want])?;
            ensure!(read > 0, "the SWF file is truncated inside a tag");
            if let Some(file) = file.as_mut() {
                file.write_all(&self.buffer[..read])?;
            }
            self.tracker.add_bytes(read as u64)?;
            left -= read as u64;
        }
        Ok(())
    }

    fn tag(&mut self, name: String, length: u32, body: &mut BufReader<ArchiveFile>) -> Result<()> {
        if let Some(every_name) = &mut self.every_name {
            every_name.push_str(&name);
            every_name.push('\n');
        }
        if !self.selection.selects(&name) {
            body.seek_relative(i64::from(length))?;
            return Ok(());
        }
        self.budget.charge(u64::from(length))?;
        let mut body = body.take(u64::from(length));
        match &self.staging {
            Some(staging) => {
                let path = staging.path().join(&name);
                let attributes = crate::windows_attributes(0, Some(self.mode));
                let mut file = crate::create_new_file(&path, self.mode, attributes)
                    .with_context(|| format!("cannot create SWF tag {name:?}"))?;
                self.copy(&mut body, length, Some(&mut file))
                    .with_context(|| format!("cannot write SWF tag {name:?}"))?;
            }
            None => self.copy(&mut body, length, None)?,
        }
        self.entries.push(Entry {
            name,
            size: u64::from(length),
            directory: false,
        });
        self.tracker.file_done()
    }
}

fn walk_tags(
    input: &mut BufReader<ArchiveFile>,
    declared: u32,
    mut each: impl FnMut(String, u32, &mut BufReader<ArchiveFile>) -> Result<()>,
) -> Result<()> {
    let mut movie_header = [0u8; MOVIE_HEADER_MAX];
    read_exactly(input, &mut movie_header[..1], "movie header")?;
    let length = movie_header_length(movie_header[0]);
    let mut position = tag_end(HEADER as u64, 0, length as u32, declared)?;
    read_exactly(input, &mut movie_header[1..length], "movie header")?;
    let mut index = 0u64;
    loop {
        let mut pair = [0u8; SHORT_TAG];
        read_exactly(input, &mut pair, "tag header")?;
        let start = tag_start(u16::from_le_bytes(pair));
        let (length, header) = match start.short_length {
            Some(length) => (length, SHORT_TAG as u64),
            None => {
                let mut long = [0u8; LONG_LENGTH_FIELD];
                read_exactly(input, &mut long, "tag header")?;
                (
                    u32::from_le_bytes(long),
                    (SHORT_TAG + LONG_LENGTH_FIELD) as u64,
                )
            }
        };
        if start.code == END_TAG {
            position = tag_end(position, header, 0, declared)?;
            ensure!(
                position == u64::from(declared),
                "the SWF end tag comes before the length the file declares"
            );
            return Ok(());
        }
        ensure!(
            index < MAX_TAGS,
            "the SWF file has more than {MAX_TAGS} tags"
        );
        position = tag_end(position, header, length, declared)?;
        each(format!("{index}.{}", start.code), length, input)?;
        index += 1;
    }
}

fn extract_tags(
    path: &Path,
    mut input: BufReader<ArchiveFile>,
    declared: u32,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    let placement = crate::placement::Placement::new(options.strip)?;
    if placement.strips() {
        placement.ensure_prefix_is_a_folder(std::iter::empty())?;
        placement.ensure_something_placed(false)?;
    }
    let tracker = Tracker::new(options.control, None, None);
    tracker.check()?;
    let staging = destination
        .map(|destination| {
            crate::staging::stage(path, destination, options.existing, options.metadata)
        })
        .transpose()?;
    let budget = Budget::new(staging.as_ref(), options.max_output_bytes)?;
    let start = input.stream_position()?;
    walk_tags(&mut input, declared, |_, length, body| {
        tracker.check()?;
        body.seek_relative(i64::from(length))?;
        Ok(())
    })?;
    input.seek(io::SeekFrom::Start(start))?;
    let selects_by_name = !(options.paths.is_empty() && options.include.is_empty());
    let mut writer = TagWriter {
        staging,
        selection,
        tracker,
        budget,
        every_name: selects_by_name.then(String::new),
        entries: Vec::new(),
        buffer: vec![0; COPY_BUFFER],
        mode: options
            .metadata
            .file_mode(Some(FILE_MODE))
            .unwrap_or(FILE_MODE),
    };
    walk_tags(&mut input, declared, |name, length, body| {
        writer.tracker.check()?;
        writer.tag(name, length, body)
    })?;
    let every_name = writer.every_name.take().unwrap_or_default();
    writer
        .selection
        .ensure_each_path_matches(every_name.lines())?;
    writer.tracker.finish();
    writer.tracker.check()?;
    if let Some(staging) = &writer.staging {
        staging.publish(&[])?;
    }
    Ok(writer.entries)
}

struct ZlibBody<R> {
    inflated: crate::framing::Reader<R>,
    remaining: u64,
}

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn zlib_error(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::InvalidData => data_error("the SWF zlib stream is damaged"),
        io::ErrorKind::UnexpectedEof => data_error("the SWF zlib stream is truncated"),
        _ => error,
    }
}

impl<R: BufRead> ZlibBody<R> {
    fn new(input: R, decoded: u64) -> Self {
        Self {
            inflated: crate::framing::Reader::zlib(input),
            remaining: decoded,
        }
    }

    fn input(&mut self) -> &mut R {
        self.inflated.get_mut()
    }
}

impl<R: BufRead> Read for ZlibBody<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut probe = [0u8; 1];
        let output = match self.remaining {
            0 => &mut probe[..],
            remaining => {
                let want = buffer
                    .len()
                    .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                &mut buffer[..want]
            }
        };
        let produced = self.inflated.read(output).map_err(zlib_error)?;
        if self.remaining == 0 && produced > 0 {
            return Err(data_error(
                "the SWF zlib stream decodes past the length the file declares",
            ));
        }
        if produced == 0 && self.remaining > 0 {
            return Err(data_error(
                "the SWF zlib stream ends before the length the file declares",
            ));
        }
        self.remaining -= produced as u64;
        Ok(produced)
    }
}

type LzmaBody = crate::lzma::RawLzmaReader<io::Take<BufReader<ArchiveFile>>>;

enum Body {
    Zlib(ZlibBody<BufReader<ArchiveFile>>),
    Lzma(LzmaBody),
}

struct Uncompressed {
    header: [u8; HEADER],
    sent: usize,
    body: Body,
}

impl Read for Uncompressed {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.sent < HEADER {
            let count = buffer.len().min(HEADER - self.sent);
            buffer[..count].copy_from_slice(&self.header[self.sent..self.sent + count]);
            self.sent += count;
            return Ok(count);
        }
        match &mut self.body {
            Body::Zlib(body) => body.read(buffer),
            Body::Lzma(body) => body.read(buffer),
        }
    }
}

impl Uncompressed {
    fn finish(mut self) -> Result<()> {
        let mut probe = [0u8; 1];
        ensure!(
            self.read(&mut probe)? == 0,
            "the SWF stream decodes past the length the file declares"
        );
        if let Body::Zlib(body) = &mut self.body {
            ensure!(
                body.input().fill_buf()?.is_empty(),
                "the SWF file has data after its zlib stream"
            );
        }
        Ok(())
    }
}

fn opened_body(
    mut input: BufReader<ArchiveFile>,
    header: &Header,
    file_length: u64,
) -> Result<Body> {
    let decoded = u64::from(header.length) - HEADER as u64;
    if header.compression == Compression::Zlib {
        return Ok(Body::Zlib(ZlibBody::new(input, decoded)));
    }
    let mut bytes = [0u8; LZMA_FIELDS];
    read_exactly(&mut input, &mut bytes, "LZMA header")?;
    let fields = lzma_fields(bytes);
    let end = (HEADER + LZMA_FIELDS) as u64 + u64::from(fields.packed);
    ensure!(
        file_length >= end,
        "the SWF LZMA stream is shorter than its packed size"
    );
    ensure!(
        file_length == end,
        "the SWF file has data after its LZMA stream"
    );
    let properties = crate::lzma::Properties::from_byte(fields.properties)
        .context("the SWF LZMA properties are out of range")?;
    let window = lzma_window(&fields, decoded);
    ensure!(
        window <= LARGEST_WINDOW,
        crate::OverLimit(format!(
            "the SWF LZMA stream needs a {window}-byte window, more than the {} MiB Zipper accepts; {BACKEND_HINT}",
            LARGEST_WINDOW >> 20
        ))
    );
    Ok(Body::Lzma(
        crate::lzma::RawLzmaReader::new(
            input.take(u64::from(fields.packed)),
            properties,
            fields.dictionary,
            decoded,
        )
        .requiring_a_clean_end()
        .refusing_input_after_its_end("the SWF LZMA stream ends before its packed size"),
    ))
}

/// Verify a SWF file, and extract it into `destination` when given. An
/// uncompressed SWF writes each tag's body as a file named `N.CODE`, where
/// `N` counts tags from 0 and `CODE` is the tag code. A compressed SWF
/// writes the uncompressed SWF it holds, named after the archive with
/// `~.swf` in place of its last extension.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let file = crate::volumes::open(path)?;
    let file_length = file.len()?;
    let mut input = BufReader::with_capacity(COPY_BUFFER, file);
    let mut bytes = [0u8; HEADER];
    read_exactly(&mut input, &mut bytes, "header")?;
    let header = header(bytes)?;
    if header.compression == Compression::None {
        ensure!(
            header.length <= UNCOMPRESSED_LIMIT,
            "the uncompressed SWF declares more than 512 MiB, which 7-Zip does not open"
        );
        ensure!(
            file_length >= u64::from(header.length),
            "the SWF file is shorter than the length it declares"
        );
        ensure!(
            file_length == u64::from(header.length),
            "the SWF file has data after the length it declares"
        );
        return extract_tags(path, input, header.length, destination, options);
    }
    let archive_name = archive_file_name(&named(path))?;
    let name = safe_output_name(
        &default_name(&archive_name, UNCOMPRESSED_SUFFIX),
        &archive_name,
    );
    let mut uncompressed = bytes;
    uncompressed[0] = b'F';
    let body = opened_body(input, &header, file_length)?;
    tree::extract("SWF", path, destination, options, |tree| {
        let mut decoded = Uncompressed {
            header: uncompressed,
            sent: 0,
            body,
        };
        tree.file(
            &name,
            u64::from(header.length),
            FILE_MODE,
            None,
            &mut decoded,
        )?;
        decoded.finish()
    })
}

#[cfg(test)]
mod zlib_tests {
    use super::*;

    struct Flate2Body<R> {
        input: R,
        inflate: flate2::Decompress,
        remaining: u64,
        ended: bool,
    }

    impl<R: BufRead> Flate2Body<R> {
        fn step(&mut self, output: &mut [u8]) -> io::Result<usize> {
            loop {
                let input = self.input.fill_buf()?;
                let at_end = input.is_empty();
                let (before_in, before_out) = (self.inflate.total_in(), self.inflate.total_out());
                let flush = match at_end {
                    true => flate2::FlushDecompress::Finish,
                    false => flate2::FlushDecompress::None,
                };
                let status = self
                    .inflate
                    .decompress(input, output, flush)
                    .map_err(|_| data_error("damaged"))?;
                let consumed = (self.inflate.total_in() - before_in) as usize;
                self.input.consume(consumed);
                let produced = (self.inflate.total_out() - before_out) as usize;
                self.ended = status == flate2::Status::StreamEnd;
                if produced > 0 || self.ended {
                    return Ok(produced);
                }
                if at_end && consumed == 0 {
                    return Err(data_error("truncated"));
                }
            }
        }
    }

    impl<R: BufRead> Read for Flate2Body<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.is_empty() || self.ended {
                return Ok(0);
            }
            let mut probe = [0u8; 1];
            let output = match self.remaining {
                0 => &mut probe[..],
                remaining => {
                    let want = buffer.len().min(remaining as usize);
                    &mut buffer[..want]
                }
            };
            let produced = self.step(output)?;
            if self.remaining == 0 && produced > 0 {
                return Err(data_error("past"));
            }
            self.remaining -= produced as u64;
            if self.ended && self.remaining > 0 {
                return Err(data_error("short"));
            }
            Ok(produced)
        }
    }

    fn read_whole(mut body: impl Read) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut piece = [0u8; 999];
        loop {
            match body.read(&mut piece).ok()? {
                0 => break,
                count => bytes.extend_from_slice(&piece[..count]),
            }
        }
        (body.read(&mut [0u8; 1]).ok()? == 0).then_some(bytes)
    }

    #[test]
    fn zlib_bodies_read_as_the_flate2_body_reads_them_whole_cut_and_flipped() {
        for (stream, size) in crate::framing::oracle::block_cases(crate::framing::oracle::zlib) {
            let ours = ZlibBody::new(&stream[..], size as u64);
            let theirs = Flate2Body {
                input: &stream[..],
                inflate: flate2::Decompress::new(true),
                remaining: size as u64,
                ended: false,
            };
            let ours = read_whole(ours);
            let theirs = read_whole(theirs);
            let mut ours_rest = ZlibBody::new(&stream[..], size as u64);
            let mut theirs_rest = Flate2Body {
                input: &stream[..],
                inflate: flate2::Decompress::new(true),
                remaining: size as u64,
                ended: false,
            };
            if ours.is_some() {
                io::copy(&mut ours_rest, &mut io::sink()).unwrap();
                io::copy(&mut theirs_rest, &mut io::sink()).unwrap();
                assert_eq!(ours_rest.input().len(), theirs_rest.input.len(), "{size}");
            }
            assert_eq!(ours, theirs, "{size}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safe_path;

    #[test]
    fn default_names_follow_7zip_on_every_short_name() {
        for (name, added, expected) in [
            ("movie.swf", "~.swf", "movie~.swf"),
            ("movie", "~.swf", "movie~.swf"),
            (".swf", "~.swf", ".swf~.swf"),
            ("a.b.swf", "~.swf", "a.b~.swf"),
            ("movie~.swf", "~.swf", "movie~~.swf"),
            ("movie.flv", "", "movie"),
            ("movie", "", "movie~"),
            (".flv", "", ".flv~"),
        ] {
            assert_eq!(default_name(name, added), expected, "{name:?}");
        }
        let mut names = vec![String::new()];
        let mut checked = 0;
        for _ in 0..6 {
            names = names
                .iter()
                .flat_map(|name| ['a', '.', 's', '~', ' ', '\\', ':'].map(|c| format!("{name}{c}")))
                .collect();
            for name in &names {
                for added in ["", UNCOMPRESSED_SUFFIX, ".audio.flv"] {
                    let output = safe_output_name(&default_name(name, added), name);
                    assert!(
                        safe_path(&output).is_ok() && !output.eq_ignore_ascii_case(name),
                        "{name:?} gave {output:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 300_000, "{checked}");
    }

    #[test]
    fn movie_headers_and_tag_starts_split_as_the_specification_lays_them_out() {
        for first in 0..=255u8 {
            let width = usize::from(first >> 3);
            let bits = 5 + 4 * width;
            assert_eq!(
                movie_header_length(first),
                bits / 8 + usize::from(bits % 8 != 0) + 4
            );
        }
        for pair in 0..=u16::MAX {
            let start = tag_start(pair);
            assert_eq!(start.code, pair / 64);
            assert_eq!(
                start.short_length,
                (pair % 64 != 63).then_some(u32::from(pair % 64))
            );
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn headers_accept_only_the_three_signatures_below_version_64() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(header) = header(bytes) {
            assert!(matches!(bytes[0], b'F' | b'C' | b'Z'));
            assert!(bytes[1] == b'W' && bytes[2] == b'S' && bytes[3] < VERSION_LIMIT);
            assert!(header.length >= HEADER as u32);
            assert!(header.compression != Compression::Lzma || header.length >= 17);
            assert!(header.length == u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
            assert!((header.compression == Compression::None) == (bytes[0] == b'F'));
        }
    }

    #[kani::proof]
    fn lzma_fields_keep_their_bytes() {
        let bytes: [u8; LZMA_FIELDS] = kani::any();
        let fields = lzma_fields(bytes);
        assert!(fields.packed.to_le_bytes() == [bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(fields.properties == bytes[4]);
        assert!(fields.dictionary.to_le_bytes() == [bytes[5], bytes[6], bytes[7], bytes[8]]);
        let decoded: u64 = kani::any();
        let window = lzma_window(&fields, decoded);
        assert!(window <= decoded && window <= u64::from(fields.dictionary));
    }

    #[kani::proof]
    fn movie_headers_and_tag_starts_fit_their_fields() {
        let first: u8 = kani::any();
        let length = movie_header_length(first);
        assert!((5..=21).contains(&length));
        let start = tag_start(kani::any());
        assert!(start.code < 1024);
        assert!(start.short_length.is_none_or(|length| length < 63));
    }

    #[kani::proof]
    fn tag_ends_never_overflow_and_stay_inside_the_declared_length() {
        let position: u64 = kani::any();
        let header: u64 = kani::any();
        let length: u32 = kani::any();
        let declared: u32 = kani::any();
        if let Ok(end) = tag_end(position, header, length, declared) {
            assert!(end <= u64::from(declared));
            assert!(end - u64::from(length) - header == position);
        }
    }
}
