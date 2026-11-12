use crate::{
    BadField, Field, field_check,
    lzma::{Properties, RawLzmaReader},
    xz_filters::{Branch, Filter},
};
use std::io::{self, Read};

pub(crate) const HEADER: usize = 14;
const NO_FILTER: u8 = 0;
const X86_FILTER: u8 = 1;
const PROPERTIES_LIMIT: u8 = 9 * 5 * 5;
const UNKNOWN_SIZE: u64 = u64::MAX;
const LARGEST_KNOWN_SIZE: u64 = 1 << 56;
const LARGEST_DICTIONARY_SHIFT: u32 = 30;
const SMALLEST_WINDOW: u64 = 4096;
const LARGEST_WINDOW: u64 = 256 * 1024 * 1024;
const SMALLEST_LATER_STREAM: u64 = 4096;
const SNIFFED: usize = HEADER + 2;
const LARGEST_EMPTY_STREAM: usize = 10;
const TOP_BIT: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) x86: bool,
    pub(crate) properties: u8,
    pub(crate) dictionary: u32,
    pub(crate) size: Option<u64>,
}

// EXTERNAL(7-Zip LzmaHandler.cpp, CheckDicSize): a dictionary of 1, 2 or 3 times a power of
// two from 2^0 to 2^30, or all ones.
fn dictionary_7zip_accepts(dictionary: u32) -> bool {
    dictionary == 1
        || dictionary == u32::MAX
        || (0..=LARGEST_DICTIONARY_SHIFT)
            .any(|shift| dictionary == 2 << shift || dictionary == 3 << shift)
}

// EXTERNAL(7-Zip LzmaHandler.cpp, CHeader::Parse): the filter byte, 0 or 1 for x86 BCJ, then
// the .lzma header: properties below 225, a dictionary 7-Zip accepts and a size below 2^56 or
// all ones for unknown.
pub(crate) fn header(bytes: [u8; HEADER]) -> Field<Header> {
    let [
        filter,
        properties,
        d0,
        d1,
        d2,
        d3,
        s0,
        s1,
        s2,
        s3,
        s4,
        s5,
        s6,
        s7,
    ] = bytes;
    field_check(
        filter == NO_FILTER || filter == X86_FILTER,
        "the lzma86 filter byte is neither 0 nor 1",
    )?;
    field_check(
        properties < PROPERTIES_LIMIT,
        "the lzma86 LZMA properties are out of range",
    )?;
    let dictionary = u32::from_le_bytes([d0, d1, d2, d3]);
    field_check(
        dictionary_7zip_accepts(dictionary),
        "the lzma86 dictionary size is not one 7-Zip writes",
    )?;
    let size = u64::from_le_bytes([s0, s1, s2, s3, s4, s5, s6, s7]);
    field_check(
        size == UNKNOWN_SIZE || size < LARGEST_KNOWN_SIZE,
        "the lzma86 size is out of range",
    )?;
    Ok(Header {
        x86: filter == X86_FILTER,
        properties,
        dictionary,
        size: (size != UNKNOWN_SIZE).then_some(size),
    })
}

// EXTERNAL(7-Zip LzmaHandler.cpp, IsArc_Lzma86 and CHandler::Open): the header and these byte
// checks; 7-Zip's trial decode of the stream's start is left out, so more files pass here.
pub(crate) fn looks_like_lzma86(start: &[u8]) -> bool {
    let Some((head, &[first, second])) = start
        .first_chunk::<SNIFFED>()
        .map(|sniffed| sniffed.split_at(HEADER))
    else {
        return false;
    };
    let Ok(header) = header(head.try_into().expect("the split leaves a whole header")) else {
        return false;
    };
    let empty_with_plain_properties = header.size == Some(0) && header.properties == 0;
    first == 0
        && (header.size.is_none_or(|size| size == 0) || second & TOP_BIT == 0)
        && !(empty_with_plain_properties && start.len() - HEADER > LARGEST_EMPTY_STREAM)
}

// EXTERNAL(7-Zip LzmaHandler.cpp, CHandler::Open): 7-Zip reads the first 128 bytes to decide.
pub(crate) const SNIFF_BYTES: usize = 128;

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, BadField(message))
}

pub(crate) struct Held<R> {
    held: Vec<u8>,
    start: usize,
    inner: R,
}

impl<R: Read> Read for Held<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.start < self.held.len() {
            let count = out.len().min(self.held.len() - self.start);
            out[..count].copy_from_slice(&self.held[self.start..self.start + count]);
            self.start += count;
            return Ok(count);
        }
        self.inner.read(out)
    }
}

impl<R> Held<R> {
    fn with_more_held(mut self, leftover: Vec<u8>) -> Self {
        let mut held = leftover;
        held.extend_from_slice(&self.held[self.start..]);
        self.held = held;
        self.start = 0;
        self
    }
}

fn read_header(input: &mut impl Read) -> io::Result<(usize, [u8; HEADER])> {
    let mut bytes = [0u8; HEADER];
    let mut filled = 0;
    while filled < HEADER {
        match input.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok((filled, bytes))
}

pub(crate) fn window(header: &Header) -> u64 {
    u64::from(header.dictionary)
        .max(SMALLEST_WINDOW)
        .min(header.size.unwrap_or(UNKNOWN_SIZE).max(1))
}

pub(crate) fn ensure_window_fits(header: &Header) -> io::Result<()> {
    if window(header) <= LARGEST_WINDOW {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!(
            "the lzma86 stream needs a {}-byte window, more than the {} MiB Zipper accepts",
            window(header),
            LARGEST_WINDOW >> 20
        ),
    ))
}

struct Stream<R> {
    lzma: RawLzmaReader<Held<R>>,
    x86: Option<Filter>,
}

pub(crate) struct Lzma86Streams<R> {
    input: Option<Held<R>>,
    stream: Option<Stream<R>>,
    streams: u64,
    decoded: u64,
}

impl<R: Read> Lzma86Streams<R> {
    pub(crate) fn new(input: R) -> Self {
        Lzma86Streams {
            input: Some(Held {
                held: Vec::new(),
                start: 0,
                inner: input,
            }),
            stream: None,
            streams: 0,
            decoded: 0,
        }
    }

    fn start_stream(&mut self) -> io::Result<bool> {
        let mut input = self.input.take().expect("input is held between streams");
        let (filled, bytes) = read_header(&mut input)?;
        let first = self.streams == 0;
        let header = match (filled, header(bytes)) {
            (0, _) if !first => return Ok(false),
            (HEADER, Ok(header)) => header,
            (HEADER, Err(BadField(message))) if first => return Err(data_error(message)),
            _ if first => return Err(data_error("the lzma86 header is truncated")),
            _ => return Err(data_error("the lzma86 file has data after its last stream")),
        };
        ensure_window_fits(&header)?;
        let properties = Properties::from_byte(header.properties)
            .ok_or_else(|| data_error("the lzma86 LZMA properties are out of range"))?;
        let lzma = match header.size {
            Some(size) => RawLzmaReader::new(input, properties, header.dictionary, size)
                .requiring_a_clean_end(),
            None => RawLzmaReader::until_its_end_marker(input, properties, header.dictionary),
        };
        self.stream = Some(Stream {
            lzma,
            x86: header.x86.then(|| Filter::branch(Branch::X86, 0)),
        });
        self.streams += 1;
        self.decoded = 0;
        Ok(true)
    }

    fn end_stream(&mut self) -> io::Result<()> {
        if self.streams > 1 && self.decoded < SMALLEST_LATER_STREAM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "lzma86 stream {} brings fewer than {SMALLEST_LATER_STREAM} bytes",
                    self.streams
                ),
            ));
        }
        let stream = self.stream.take().expect("a stream is open");
        let (input, leftover) = stream.lzma.into_input();
        self.input = Some(input.with_more_held(leftover));
        Ok(())
    }
}

impl<R: Read> Read for Lzma86Streams<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.stream.is_none() && (self.input.is_none() || !self.start_stream()?) {
                self.input = None;
                return Ok(0);
            }
            let stream = self.stream.as_mut().expect("a stream is open");
            let read = match &mut stream.x86 {
                Some(filter) => filter.read(&mut stream.lzma, out)?,
                None => stream.lzma.read(out)?,
            };
            if read > 0 {
                self.decoded += read as u64;
                return Ok(read);
            }
            self.end_stream()?;
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn closed_form(dictionary: u32) -> bool {
        let third = dictionary / 3;
        dictionary == 1
            || dictionary == u32::MAX
            || (dictionary.is_power_of_two() && dictionary >= 2)
            || (dictionary % 3 == 0 && third.is_power_of_two() && third <= 1 << 30)
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn lzma86_headers_accept_exactly_what_7zip_accepts() {
        let bytes: [u8; HEADER] = kani::any();
        let dictionary = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        let size = u64::from_le_bytes(bytes[6..].try_into().unwrap());
        let accepted = bytes[0] <= 1
            && bytes[1] < 225
            && closed_form(dictionary)
            && (size == u64::MAX || size < 1 << 56);
        match header(bytes) {
            Ok(header) => {
                assert!(accepted);
                assert!(header.x86 == (bytes[0] == 1) && header.properties == bytes[1]);
                assert!(header.dictionary == dictionary);
                assert!(header.size == (size != u64::MAX).then_some(size));
                assert!(window(&header) >= SMALLEST_WINDOW.min(size.max(1)));
            }
            Err(_) => assert!(!accepted),
        }
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn sniffing_any_short_start_never_panics_and_needs_a_header_and_a_zero() {
        let bytes: [u8; SNIFFED + 1] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if looks_like_lzma86(&bytes[..length]) {
            assert!(length >= SNIFFED && bytes[HEADER] == 0);
            assert!(header(bytes[..HEADER].try_into().unwrap()).is_ok());
        }
    }
}
