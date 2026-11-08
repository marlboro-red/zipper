//! In-process extraction of Flash Video (FLV) files, as 7-Zip extracts them.
//! A file of one audio or video stream extracts to that stream's raw data. A
//! file of both extracts to one FLV file for each. See
//! `docs/reference/swf-reader.md`.
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree;
use crate::native_swf::{
    archive_file_name, default_name, named, safe_output_name, without_trailing_spaces,
};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, BufRead, BufReader, Read, Seek},
    path::Path,
};

const SIGNATURE: &[u8; 4] = b"FLV\x01";
const HEADER: usize = 13;
const HEADER_LENGTH: u32 = 9;
const TAG_HEADER: usize = 11;
const PREVIOUS_SIZE: usize = 4;
const TAG_OVERHEAD: u64 = (TAG_HEADER + PREVIOUS_SIZE) as u64;
const AUDIO: u8 = 8;
const VIDEO: u8 = 9;
const SCRIPT: u8 = 18;
const FLAG_AUDIO: u8 = 4;
const FLAG_VIDEO: u8 = 1;
const FLAGS_7ZIP_OPENS: u8 = FLAG_AUDIO | FLAG_VIDEO;
// EXTERNAL(7-Zip 26.05): an FLV with more than 2^23 audio and video tags does not open.
const MEDIA_TAGS_LIMIT: u64 = 1 << 23;
const TAGS_PER_CHECK: u64 = 4096;
const FILE_MODE: u32 = 0o666;
const COPY_BUFFER: usize = 1024 * 1024;
const AUDIO_CODECS: [&str; 16] = [
    "pcm",
    "adpcm",
    "mp3",
    "pcm_le",
    "nellymoser16",
    "nellymoser8",
    "nellymoser",
    "g711a",
    "g711m",
    "audio9",
    "aac",
    "speex",
    "audio12",
    "audio13",
    "mp3",
    "audio15",
];
const VIDEO_CODECS: [&str; 16] = [
    "video0", "jpeg", "h263", "screen", "vp6", "vp6alpha", "screen2", "avc", "video8", "video9",
    "video10", "video11", "video12", "video13", "video14", "video15",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tag {
    kind: u8,
    size: u32,
}

fn file_header(bytes: &[u8; HEADER]) -> Field<()> {
    field_check(&bytes[..4] == SIGNATURE, "not an FLV file")?;
    field_check(
        bytes[4] & !FLAGS_7ZIP_OPENS == 0,
        "the FLV header sets flags 7-Zip does not open",
    )?;
    field_check(
        u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) == HEADER_LENGTH,
        "the FLV header length is not 9, which 7-Zip does not open",
    )?;
    field_check(
        bytes[9..] == [0; PREVIOUS_SIZE],
        "the FLV header's first previous tag size is not zero",
    )
}

// SPEC(FLV 10.1, FLVTAG): a tag holds its type, a 24-bit data size, a 32-bit timestamp and a
// 24-bit stream ID, which is always zero.
fn tag_header(bytes: &[u8; TAG_HEADER]) -> Field<Tag> {
    let size = u32::from_be_bytes([0, bytes[1], bytes[2], bytes[3]]);
    field_check(size > 0, "an FLV tag holds no data")?;
    field_check(bytes[8..] == [0; 3], "an FLV tag has a stream ID")?;
    field_check(
        matches!(bytes[0], AUDIO | VIDEO | SCRIPT),
        "an FLV tag is neither audio, video nor script data",
    )?;
    Ok(Tag {
        kind: bytes[0],
        size,
    })
}

fn previous_size_matches(tag: Tag, previous: [u8; PREVIOUS_SIZE]) -> bool {
    u32::from_be_bytes(previous) == TAG_HEADER as u32 + tag.size
}

fn tag_end(at: u64, tag: Tag, length: u64) -> Field<u64> {
    at.checked_add(TAG_OVERHEAD + u64::from(tag.size))
        .filter(|&end| end <= length)
        .ok_or(BadField("an FLV tag runs past the end of the file"))
}

// SPEC(FLV 10.1, AUDIODATA and VIDEODATA): audio keeps its format in the high nibble of its
// first byte, and video keeps its codec in the low nibble.
fn codec(kind: u8, first: u8) -> u8 {
    match kind {
        AUDIO => first >> 4,
        _ => first & 0x0F,
    }
}

fn read_exactly(input: &mut impl Read, bytes: &mut [u8]) -> Result<()> {
    input.read_exact(bytes).map_err(|error| match error.kind() {
        io::ErrorKind::UnexpectedEof => anyhow::anyhow!("the FLV file is truncated"),
        _ => error.into(),
    })
}

struct Tags<R> {
    input: R,
    at: u64,
    length: u64,
}

struct Started {
    tag: Tag,
    header: [u8; TAG_HEADER],
    first: u8,
}

impl<R: BufRead + Seek> Tags<R> {
    fn open(mut input: R, length: u64) -> Result<Self> {
        let mut header = [0u8; HEADER];
        read_exactly(&mut input, &mut header)?;
        file_header(&header)?;
        Ok(Tags {
            input,
            at: HEADER as u64,
            length,
        })
    }

    fn start(&mut self) -> Result<Option<Started>> {
        if self.at == self.length {
            return Ok(None);
        }
        ensure!(
            self.length - self.at >= TAG_HEADER as u64,
            "the FLV file ends inside a tag header"
        );
        let mut header = [0u8; TAG_HEADER];
        read_exactly(&mut self.input, &mut header)?;
        let tag = tag_header(&header)?;
        self.at = tag_end(self.at, tag, self.length)?;
        let mut first = [0u8; 1];
        read_exactly(&mut self.input, &mut first)?;
        Ok(Some(Started {
            tag,
            header,
            first: first[0],
        }))
    }

    fn skip_data(&mut self, tag: Tag) -> Result<()> {
        self.input.seek_relative(i64::from(tag.size) - 1)?;
        Ok(())
    }

    fn end(&mut self, tag: Tag) -> Result<[u8; PREVIOUS_SIZE]> {
        let mut previous = [0u8; PREVIOUS_SIZE];
        read_exactly(&mut self.input, &mut previous)?;
        ensure!(
            previous_size_matches(tag, previous),
            "an FLV tag's previous tag size does not match it"
        );
        Ok(previous)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stream {
    kind: u8,
    codec: u8,
    one_codec: bool,
    tags: u64,
    data: u64,
}

impl Stream {
    fn raw_size(&self) -> u64 {
        self.data - self.tags
    }

    fn flv_size(&self) -> u64 {
        HEADER as u64 + self.data + self.tags * TAG_OVERHEAD
    }

    fn extension(&self, raw: bool) -> &'static str {
        match (raw, self.kind) {
            (true, AUDIO) => AUDIO_CODECS[usize::from(self.codec)],
            (true, _) => VIDEO_CODECS[usize::from(self.codec)],
            (false, AUDIO) => "audio.flv",
            (false, _) => "video.flv",
        }
    }
}

fn streams<R: BufRead + Seek>(
    tags: &mut Tags<R>,
    check: impl Fn() -> Result<()>,
) -> Result<Vec<Stream>> {
    let mut streams: Vec<Stream> = Vec::new();
    let mut media = 0u64;
    let mut walked = 0u64;
    while let Some(started) = tags.start()? {
        walked += 1;
        if walked.is_multiple_of(TAGS_PER_CHECK) {
            check()?;
        }
        let Started { tag, first, .. } = started;
        if tag.kind != SCRIPT {
            media += 1;
            ensure!(
                media <= MEDIA_TAGS_LIMIT,
                "the FLV file has more than {MEDIA_TAGS_LIMIT} audio and video tags, which 7-Zip does not open"
            );
            let codec = codec(tag.kind, first);
            match streams.iter_mut().find(|stream| stream.kind == tag.kind) {
                Some(stream) => {
                    stream.one_codec &= stream.codec == codec;
                    stream.tags += 1;
                    stream.data += u64::from(tag.size);
                }
                None => streams.push(Stream {
                    kind: tag.kind,
                    codec,
                    one_codec: true,
                    tags: 1,
                    data: u64::from(tag.size),
                }),
            }
        }
        tags.skip_data(tag)?;
        tags.end(tag)?;
    }
    ensure!(!streams.is_empty(), "the FLV file holds no audio or video");
    Ok(streams)
}

enum Pending {
    Bytes {
        bytes: [u8; HEADER],
        at: usize,
        end: usize,
    },
    Data {
        tag: Tag,
        left: u64,
    },
    Trailer(Tag),
    Next,
}

struct StreamBytes<R> {
    tags: Tags<R>,
    kind: u8,
    raw: bool,
    pending: Pending,
    after_bytes: Option<Tag>,
}

impl<R: BufRead + Seek> StreamBytes<R> {
    fn new(tags: Tags<R>, kind: u8, raw: bool) -> Self {
        let mut bytes = [0u8; HEADER];
        bytes[..4].copy_from_slice(SIGNATURE);
        bytes[4] = if kind == AUDIO {
            FLAG_AUDIO
        } else {
            FLAG_VIDEO
        };
        bytes[5..9].copy_from_slice(&HEADER_LENGTH.to_be_bytes());
        let pending = match raw {
            true => Pending::Next,
            false => Pending::Bytes {
                bytes,
                at: 0,
                end: HEADER,
            },
        };
        StreamBytes {
            tags,
            kind,
            raw,
            pending,
            after_bytes: None,
        }
    }

    fn advance(&mut self) -> Result<bool> {
        loop {
            let Some(Started { tag, header, first }) = self.tags.start()? else {
                return Ok(false);
            };
            if tag.kind != self.kind {
                self.tags.skip_data(tag)?;
                self.tags.end(tag)?;
                continue;
            }
            if self.raw {
                self.pending = Pending::Data {
                    tag,
                    left: u64::from(tag.size) - 1,
                };
            } else {
                let mut bytes = [0u8; HEADER];
                bytes[..TAG_HEADER].copy_from_slice(&header);
                bytes[TAG_HEADER] = first;
                self.pending = Pending::Bytes {
                    bytes,
                    at: 0,
                    end: TAG_HEADER + 1,
                };
                self.after_bytes = Some(tag);
            }
            return Ok(true);
        }
    }

    fn fill(&mut self, buffer: &mut [u8]) -> Result<usize> {
        loop {
            match &mut self.pending {
                Pending::Bytes { bytes, at, end } => {
                    let count = buffer.len().min(*end - *at);
                    buffer[..count].copy_from_slice(&bytes[*at..*at + count]);
                    *at += count;
                    if *at == *end {
                        self.pending = match self.after_bytes.take() {
                            Some(tag) => Pending::Data {
                                tag,
                                left: u64::from(tag.size) - 1,
                            },
                            None => Pending::Next,
                        };
                    }
                    return Ok(count);
                }
                Pending::Data { tag, left: 0 } => {
                    let tag = *tag;
                    self.pending = Pending::Trailer(tag);
                }
                Pending::Data { left, .. } => {
                    let want = buffer
                        .len()
                        .min(usize::try_from(*left).unwrap_or(usize::MAX));
                    let read = self.tags.input.read(&mut buffer[..want])?;
                    ensure!(read > 0, "the FLV file is truncated");
                    *left -= read as u64;
                    return Ok(read);
                }
                Pending::Trailer(tag) => {
                    let tag = *tag;
                    let previous = self.tags.end(tag)?;
                    self.pending = match self.raw {
                        true => Pending::Next,
                        false => {
                            let mut bytes = [0u8; HEADER];
                            bytes[..PREVIOUS_SIZE].copy_from_slice(&previous);
                            Pending::Bytes {
                                bytes,
                                at: 0,
                                end: PREVIOUS_SIZE,
                            }
                        }
                    };
                }
                Pending::Next => {
                    if !self.advance()? {
                        return Ok(0);
                    }
                }
            }
        }
    }
}

impl<R: BufRead + Seek> Read for StreamBytes<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.fill(buffer).map_err(io::Error::other)
    }
}

/// Returns whether the file starts with the FLV signature and version 1.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut bytes = Vec::with_capacity(SIGNATURE.len());
    crate::volumes::open(path)?
        .take(SIGNATURE.len() as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes == SIGNATURE)
}

fn opened(path: &Path) -> Result<Tags<BufReader<ArchiveFile>>> {
    let file = crate::volumes::open(path)?;
    let length = file.len()?;
    Tags::open(BufReader::with_capacity(COPY_BUFFER, file), length)
}

/// Verify an FLV file, and extract it into `destination` when given. A file
/// of one stream writes its raw data, named after the archive with the
/// codec as its extension, such as `movie.mp3`. A file of audio and video
/// writes `movie.audio.flv` and `movie.video.flv`.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let archive_name = archive_file_name(&named(path))?;
    let base = default_name(&archive_name, "");
    let base = without_trailing_spaces(&base);
    tree::extract("FLV", path, destination, options, |tree| {
        let streams = streams(&mut opened(path)?, || tree.check())?;
        let raw = streams.len() == 1;
        ensure!(
            !raw || streams[0].one_codec,
            "the FLV stream changes codec, which 7-Zip does not open in a file of one stream"
        );
        for stream in &streams {
            let name =
                safe_output_name(&format!("{base}.{}", stream.extension(raw)), &archive_name);
            let size = match raw {
                true => stream.raw_size(),
                false => stream.flv_size(),
            };
            let mut bytes = StreamBytes::new(opened(path)?, stream.kind, raw);
            tree.file(&name, size, FILE_MODE, None, &mut bytes)?;
            let mut probe = [0u8; 1];
            ensure!(
                bytes.read(&mut probe).context("cannot read the FLV file")? == 0,
                "the FLV file changed while it was read"
            );
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repeated {
        tag: Vec<u8>,
        count: u64,
        at: u64,
    }

    impl Repeated {
        fn length(&self) -> u64 {
            HEADER as u64 + self.tag.len() as u64 * self.count
        }

        fn byte(&self, at: u64) -> u8 {
            let header = [b'F', b'L', b'V', 1, FLAG_AUDIO, 0, 0, 0, 9, 0, 0, 0, 0];
            match at.checked_sub(HEADER as u64) {
                None => header[at as usize],
                Some(rest) => self.tag[(rest % self.tag.len() as u64) as usize],
            }
        }
    }

    impl Read for Repeated {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = (buffer.len() as u64).min(self.length() - self.at) as usize;
            for (index, slot) in buffer[..count].iter_mut().enumerate() {
                *slot = self.byte(self.at + index as u64);
            }
            self.at += count as u64;
            Ok(count)
        }
    }

    impl Seek for Repeated {
        fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
            self.at = match position {
                io::SeekFrom::Start(at) => at,
                io::SeekFrom::Current(step) => self.at.checked_add_signed(step).unwrap(),
                io::SeekFrom::End(step) => self.length().checked_add_signed(step).unwrap(),
            };
            Ok(self.at)
        }
    }

    fn walk(count: u64) -> Result<Vec<Stream>> {
        let tag = vec![AUDIO, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x2F, 0, 0, 0, 12];
        let source = Repeated { tag, count, at: 0 };
        let length = source.length();
        let mut tags = Tags::open(BufReader::with_capacity(1 << 16, source), length)?;
        streams(&mut tags, || Ok(()))
    }

    #[test]
    fn the_media_tag_limit_is_7zips() {
        let streams = walk(MEDIA_TAGS_LIMIT).unwrap();
        assert_eq!(streams[0].tags, MEDIA_TAGS_LIMIT);
        let error = walk(MEDIA_TAGS_LIMIT + 1).unwrap_err().to_string();
        assert!(error.contains("more than"), "{error}");
    }

    #[test]
    fn codecs_come_from_the_nibble_each_kind_keeps_them_in() {
        for first in 0..=255u8 {
            assert_eq!(codec(AUDIO, first), first >> 4);
            assert_eq!(codec(VIDEO, first), first & 15);
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn file_headers_accept_exactly_what_7zip_opens() {
        let bytes: [u8; HEADER] = kani::any();
        let accepted = bytes[..4] == *SIGNATURE
            && bytes[4] & 0xFA == 0
            && bytes[5..9] == [0, 0, 0, 9]
            && bytes[9..] == [0, 0, 0, 0];
        assert!(file_header(&bytes).is_ok() == accepted);
    }

    #[kani::proof]
    fn tag_headers_keep_their_size_and_kind_and_ends_stay_inside_the_file() {
        let bytes: [u8; TAG_HEADER] = kani::any();
        if let Ok(tag) = tag_header(&bytes) {
            assert!(tag.size > 0 && tag.size < 1 << 24);
            assert!(tag.kind == bytes[0] && matches!(tag.kind, AUDIO | VIDEO | SCRIPT));
            assert!(bytes[8] == 0 && bytes[9] == 0 && bytes[10] == 0);
            let previous: [u8; PREVIOUS_SIZE] = kani::any();
            if previous_size_matches(tag, previous) {
                assert!(u32::from_be_bytes(previous) == tag.size + 11);
            }
            let at: u64 = kani::any();
            let length: u64 = kani::any();
            if let Ok(end) = tag_end(at, tag, length) {
                assert!(end <= length && end - at == u64::from(tag.size) + 15);
            }
            assert!(codec(tag.kind, kani::any()) < 16);
        }
    }
}
