use crate::inflate::{Inflate, Invalid, SMALLEST_WINDOW};
use std::io::{self, BufRead, Read};

const LARGEST_CINFO: u8 = 7;
const NO_PROGRESS: Invalid = Invalid("Deflate decoder made no progress on its input");
const FAILED_BEFORE: Invalid = Invalid("Deflate stream failed on an earlier read");

pub(crate) fn zlib_header(cmf: u8, flg: u8) -> Result<(), Invalid> {
    // SPEC(RFC 1950 2.2): CMF and FLG read as a big-endian number are a multiple of 31.
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(Invalid("zlib header check fails"));
    }
    if cmf & 0xf != 8 {
        return Err(Invalid("zlib stream uses a method other than Deflate"));
    }
    // SPEC(RFC 1950 2.2): CINFO values above 7, windows above 32 KiB, are not allowed.
    if cmf >> 4 > LARGEST_CINFO {
        return Err(Invalid("zlib stream declares a window above 32 KiB"));
    }
    if flg & 0x20 != 0 {
        return Err(Invalid("zlib stream needs a preset dictionary"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self { a: 1, b: 0 }
    }
}

const ADLER_MODULUS: u32 = 65521;
const ADLER_LANES: usize = 16;
const ADLER_BYTES_BEFORE_A_REDUCTION: usize = 5552;

impl Adler32 {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let (mut a, mut b) = (self.a, self.b);
        for run in bytes.chunks(ADLER_BYTES_BEFORE_A_REDUCTION) {
            let mut lanes = run.chunks_exact(ADLER_LANES);
            for lane in &mut lanes {
                let (mut sum, mut weighted) = (0u32, 0u32);
                for (index, &byte) in lane.iter().enumerate() {
                    sum += u32::from(byte);
                    weighted += (ADLER_LANES - index) as u32 * u32::from(byte);
                }
                b += ADLER_LANES as u32 * a + weighted;
                a += sum;
            }
            for &byte in lanes.remainder() {
                a += u32::from(byte);
                b += a;
            }
            a %= ADLER_MODULUS;
            b %= ADLER_MODULUS;
        }
        (self.a, self.b) = (a, b);
    }

    pub(crate) fn sum(self) -> u32 {
        self.b << 16 | self.a
    }
}

pub(crate) fn adler32(bytes: &[u8]) -> u32 {
    let mut adler = Adler32::default();
    adler.update(bytes);
    adler.sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub(crate) consumed: usize,
    pub(crate) written: usize,
}

pub(crate) fn inflate_block(
    window: &mut Vec<u8>,
    history: &[u8],
    input: &[u8],
    output: &mut [u8],
) -> Result<Option<Decoded>, Invalid> {
    if window.len() < SMALLEST_WINDOW {
        window.resize(SMALLEST_WINDOW, 0);
    }
    let mut inflate = Inflate::with_history(&mut window[..], history);
    let (mut consumed, mut written) = (0, 0);
    loop {
        let step = inflate.decode(&input[consumed..])?;
        consumed += step.consumed;
        let end = written + step.output.len();
        let Some(room) = output.get_mut(written..end) else {
            return Ok(None);
        };
        room.copy_from_slice(step.output);
        written = end;
        if step.finished {
            return Ok(Some(Decoded { consumed, written }));
        }
        if step.needs_input() {
            return match consumed == input.len() {
                true => Ok(None),
                false => Err(NO_PROGRESS),
            };
        }
    }
}

pub(crate) fn inflate_until_full(
    window: &mut Vec<u8>,
    input: &[u8],
    output: &mut [u8],
) -> Result<Option<usize>, Invalid> {
    match fill(window, input, output, input.len()) {
        Err(_) => fill(window, input, output, 1),
        filled => filled,
    }
}

fn fill(
    window: &mut Vec<u8>,
    input: &[u8],
    output: &mut [u8],
    piece: usize,
) -> Result<Option<usize>, Invalid> {
    if window.len() < SMALLEST_WINDOW {
        window.resize(SMALLEST_WINDOW, 0);
    }
    let mut inflate = Inflate::new(&mut window[..]);
    let (mut consumed, mut written) = (0, 0);
    loop {
        let end = consumed + piece.min(input.len() - consumed);
        let step = inflate.decode(&input[consumed..end])?;
        consumed += step.consumed;
        let count = step.output.len().min(output.len() - written);
        output[written..written + count].copy_from_slice(&step.output[..count]);
        written += count;
        if written == output.len() || step.finished {
            return Ok(Some(written));
        }
        if step.needs_input() {
            return match consumed == input.len() {
                true => Ok(None),
                false => Err(NO_PROGRESS),
            };
        }
    }
}

pub(crate) fn zlib_block(
    window: &mut Vec<u8>,
    input: &[u8],
    output: &mut [u8],
) -> Result<Option<Decoded>, Invalid> {
    let [cmf, flg, body @ ..] = input else {
        return Ok(None);
    };
    zlib_header(*cmf, *flg)?;
    let Some(inflated) = inflate_block(window, &[], body, output)? else {
        return Ok(None);
    };
    let Some(trailer) = body.get(inflated.consumed..inflated.consumed + 4) else {
        return Ok(None);
    };
    if u32::from_be_bytes(trailer.try_into().expect("four bytes"))
        != adler32(&output[..inflated.written])
    {
        return Err(Invalid("zlib stream fails its Adler-32 check"));
    }
    Ok(Some(Decoded {
        consumed: 2 + inflated.consumed + 4,
        written: inflated.written,
    }))
}

const GZIP_HEADER_CRC: u8 = 1 << 1;
const GZIP_EXTRA: u8 = 1 << 2;
const GZIP_NAME: u8 = 1 << 3;
const GZIP_COMMENT: u8 = 1 << 4;
const GZIP_RESERVED: u8 = 0xe0;
const GZIP_FIXED_HEADER: u32 = 10;
pub(crate) const LONGEST_GZIP_TEXT: u32 = 65_535;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Fixed,
    ExtraLength,
    Extra,
    Name,
    Comment,
    HeaderCrc,
    Complete,
}

fn crc32_byte(register: u32, byte: u8) -> u32 {
    let mut register = register ^ u32::from(byte);
    for _ in 0..8 {
        register = register >> 1 ^ 0xedb8_8320 & (register & 1).wrapping_neg();
    }
    register
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GzipHeader {
    field: Field,
    flags: u8,
    at: u32,
    extra: u16,
    crc: u32,
    stored_crc: u16,
}

impl GzipHeader {
    pub(crate) fn new() -> Self {
        Self {
            field: Field::Fixed,
            flags: 0,
            at: 0,
            extra: 0,
            crc: u32::MAX,
            stored_crc: 0,
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.field == Field::Complete
    }

    pub(crate) fn read(&mut self, input: &[u8]) -> Result<usize, Invalid> {
        let mut used = 0;
        while self.field != Field::Complete {
            let Some(&byte) = input.get(used) else {
                break;
            };
            used += 1;
            self.take(byte)?;
        }
        Ok(used)
    }

    fn take(&mut self, byte: u8) -> Result<(), Invalid> {
        if self.field != Field::HeaderCrc {
            self.crc = crc32_byte(self.crc, byte);
        }
        match self.field {
            Field::Fixed => {
                // SPEC(RFC 1952 2.3.1): ID1, ID2 and CM are 31, 139 and 8, and FLG bits 5 to 7 are 0.
                match (self.at, byte) {
                    (0, 0x1f) | (1, 0x8b) | (2, 8) => {}
                    (0..=2, _) => return Err(Invalid("not a gzip Deflate member header")),
                    (3, flags) if flags & GZIP_RESERVED != 0 => {
                        return Err(Invalid("gzip header sets a reserved flag"));
                    }
                    (3, flags) => self.flags = flags,
                    _ => {}
                }
                self.at += 1;
                if self.at == GZIP_FIXED_HEADER {
                    self.next_after(Field::Fixed);
                }
            }
            Field::ExtraLength => {
                self.extra |= u16::from(byte) << (8 * self.at);
                self.at += 1;
                if self.at == 2 {
                    match self.extra {
                        0 => self.next_after(Field::Extra),
                        _ => self.field = Field::Extra,
                    }
                }
            }
            Field::Extra => {
                self.extra -= 1;
                if self.extra == 0 {
                    self.next_after(Field::Extra);
                }
            }
            Field::Name | Field::Comment => match byte {
                0 => self.next_after(self.field),
                _ if self.at == LONGEST_GZIP_TEXT => {
                    return Err(Invalid("gzip header name or comment is too long"));
                }
                _ => self.at += 1,
            },
            Field::HeaderCrc => {
                self.stored_crc |= u16::from(byte) << (8 * self.at);
                self.at += 1;
                if self.at == 2 {
                    if self.stored_crc != !self.crc as u16 {
                        return Err(Invalid("gzip header fails its CRC check"));
                    }
                    self.field = Field::Complete;
                }
            }
            Field::Complete => {}
        }
        Ok(())
    }

    fn next_after(&mut self, field: Field) {
        let has = |flag: u8| self.flags & flag != 0;
        self.at = 0;
        self.field = match field {
            Field::Fixed if has(GZIP_EXTRA) => Field::ExtraLength,
            Field::Fixed | Field::ExtraLength | Field::Extra if has(GZIP_NAME) => Field::Name,
            Field::Fixed | Field::ExtraLength | Field::Extra | Field::Name if has(GZIP_COMMENT) => {
                Field::Comment
            }
            Field::Complete | Field::HeaderCrc => Field::Complete,
            _ if has(GZIP_HEADER_CRC) => Field::HeaderCrc,
            _ => Field::Complete,
        };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    Raw,
    Zlib,
    Gzip,
}

impl Framing {
    fn trailer(self) -> usize {
        match self {
            Framing::Raw => 0,
            Framing::Zlib => 4,
            Framing::Gzip => 8,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Header,
    Body,
    Trailer,
    End,
    Failed,
}

pub(crate) struct Reader<R> {
    input: R,
    inflate: Inflate<Box<[u8]>>,
    held: usize,
    framing: Framing,
    stage: Stage,
    gzip_header: GzipHeader,
    gathered: [u8; 8],
    have: usize,
    checks: Checks,
}

#[derive(Default)]
struct Checks {
    adler: Adler32,
    crc: crc32fast::Hasher,
    size: u32,
}

impl Checks {
    fn update(&mut self, framing: Framing, output: &[u8]) {
        match framing {
            Framing::Raw => {}
            Framing::Zlib => self.adler.update(output),
            Framing::Gzip => {
                self.crc.update(output);
                self.size = self.size.wrapping_add(output.len() as u32);
            }
        }
    }
}

fn invalid_data(invalid: Invalid) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, invalid)
}

fn truncated(framing: Framing) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        match framing {
            Framing::Raw => "Deflate stream is truncated",
            Framing::Zlib => "zlib stream is truncated",
            Framing::Gzip => "gzip stream is truncated",
        },
    )
}

impl<R: BufRead> Reader<R> {
    fn new(input: R, framing: Framing) -> Self {
        Self::with_window(input, framing, SMALLEST_WINDOW)
    }

    fn with_window(input: R, framing: Framing, window: usize) -> Self {
        Self {
            input,
            inflate: Inflate::new(vec![0; window].into_boxed_slice()),
            held: 0,
            framing,
            stage: match framing {
                Framing::Raw => Stage::Body,
                Framing::Zlib | Framing::Gzip => Stage::Header,
            },
            gzip_header: GzipHeader::new(),
            gathered: [0; 8],
            have: 0,
            checks: Checks::default(),
        }
    }

    pub(crate) fn raw(input: R) -> Self {
        Self::new(input, Framing::Raw)
    }

    pub(crate) fn zlib(input: R) -> Self {
        Self::new(input, Framing::Zlib)
    }

    pub(crate) fn gzip(input: R) -> Self {
        Self::new(input, Framing::Gzip)
    }

    pub(crate) fn gzip_with_window(input: R, window: usize) -> Self {
        Self::with_window(input, Framing::Gzip, window)
    }

    pub(crate) fn get_mut(&mut self) -> &mut R {
        &mut self.input
    }

    fn gather(&mut self, wanted: usize) -> io::Result<bool> {
        let input = self.input.fill_buf()?;
        if input.is_empty() {
            return Err(truncated(self.framing));
        }
        let count = input.len().min(wanted - self.have);
        self.gathered[self.have..self.have + count].copy_from_slice(&input[..count]);
        self.input.consume(count);
        self.have += count;
        Ok(self.have == wanted)
    }

    fn header(&mut self) -> io::Result<()> {
        match self.framing {
            Framing::Gzip => {
                let input = self.input.fill_buf()?;
                if input.is_empty() {
                    return Err(truncated(self.framing));
                }
                let used = self.gzip_header.read(input).map_err(invalid_data)?;
                self.input.consume(used);
                if !self.gzip_header.is_complete() {
                    return Ok(());
                }
            }
            _ => {
                if !self.gather(2)? {
                    return Ok(());
                }
                zlib_header(self.gathered[0], self.gathered[1]).map_err(invalid_data)?;
            }
        }
        self.have = 0;
        self.stage = Stage::Body;
        Ok(())
    }

    fn body(&mut self) -> io::Result<()> {
        let input = self.input.fill_buf()?;
        let at_end = input.is_empty();
        let step = self.inflate.decode(input).map_err(invalid_data)?;
        self.checks.update(self.framing, step.output);
        let (consumed, produced) = (step.consumed, step.output.len());
        let (finished, stalled) = (step.finished, step.needs_input());
        self.input.consume(consumed);
        self.held = produced;
        if finished {
            self.stage = self.after_body();
        } else if stalled {
            return Err(match at_end {
                true => truncated(self.framing),
                false => invalid_data(NO_PROGRESS),
            });
        }
        Ok(())
    }

    fn stored_body(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let input = self.input.fill_buf()?;
        let copied = self
            .inflate
            .copy_stored(input, output)
            .map_err(invalid_data)?;
        self.checks.update(self.framing, &output[..copied.written]);
        self.input.consume(copied.consumed);
        if copied.finished {
            self.stage = self.after_body();
        }
        Ok(copied.written)
    }

    fn after_body(&self) -> Stage {
        match self.framing {
            Framing::Raw => Stage::End,
            Framing::Zlib | Framing::Gzip => Stage::Trailer,
        }
    }

    fn step(&mut self) -> io::Result<()> {
        let stepped = match self.stage {
            Stage::Header => self.header(),
            Stage::Body => self.body(),
            Stage::Trailer => self.trailer(),
            Stage::End => Ok(()),
            Stage::Failed => return Err(invalid_data(FAILED_BEFORE)),
        };
        stepped.map_err(|error| self.failed_on(error))
    }

    fn failed_on(&mut self, error: io::Error) -> io::Error {
        if matches!(
            error.kind(),
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
        ) {
            self.stage = Stage::Failed;
        }
        error
    }

    fn trailer(&mut self) -> io::Result<()> {
        if self.have < self.framing.trailer() && !self.gather(self.framing.trailer())? {
            return Ok(());
        }
        let more = self.framing == Framing::Gzip && !self.input.fill_buf()?.is_empty();
        self.have = 0;
        let word = |at: usize| self.gathered[at..at + 4].try_into().expect("four bytes");
        if self.framing == Framing::Zlib {
            if u32::from_be_bytes(word(0)) != self.checks.adler.sum() {
                return Err(invalid_data(Invalid(
                    "zlib stream fails its Adler-32 check",
                )));
            }
            self.stage = Stage::End;
            return Ok(());
        }
        let (crc, size) = (u32::from_le_bytes(word(0)), u32::from_le_bytes(word(4)));
        let checks = std::mem::take(&mut self.checks);
        if crc != checks.crc.finalize() || size != checks.size {
            return Err(invalid_data(Invalid(
                "gzip member fails its CRC-32 or size check",
            )));
        }
        // SPEC(RFC 1952 2.2): a gzip file is a series of members, so input after a trailer is one more.
        if !more {
            self.stage = Stage::End;
        } else {
            self.inflate.restart();
            self.gzip_header = GzipHeader::new();
            self.stage = Stage::Header;
        }
        Ok(())
    }
}

impl<R: BufRead> BufRead for Reader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        while self.held == 0 && self.stage != Stage::End {
            self.step()?;
        }
        Ok(self.inflate.latest(self.held))
    }

    fn consume(&mut self, amount: usize) {
        self.held -= amount.min(self.held);
    }
}

impl<R: BufRead> Read for Reader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.held == 0 && matches!(self.stage, Stage::Header | Stage::Trailer) {
            self.step()?;
        }
        if self.held == 0 && self.stage == Stage::Body && self.inflate.may_copy_stored() {
            let written = self
                .stored_body(output)
                .map_err(|error| self.failed_on(error))?;
            if written > 0 {
                return Ok(written);
            }
        }
        let held = self.fill_buf()?;
        let count = held.len().min(output.len());
        output[..count].copy_from_slice(&held[..count]);
        self.consume(count);
        Ok(count)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const BYTES: usize = 13;

    fn reference_length(input: &[u8]) -> Option<usize> {
        let flags = *input.get(3)?;
        let mut length = GZIP_FIXED_HEADER as usize;
        if flags & GZIP_EXTRA != 0 {
            let field = input.get(length..length + 2)?;
            length += 2 + usize::from(u16::from_le_bytes([field[0], field[1]]));
        }
        for flag in [GZIP_NAME, GZIP_COMMENT] {
            if flags & flag != 0 {
                length += input.get(length..)?.iter().position(|&byte| byte == 0)? + 1;
            }
        }
        if flags & GZIP_HEADER_CRC != 0 {
            length += 2;
        }
        Some(length)
    }

    #[kani::proof]
    #[kani::unwind(15)]
    fn gzip_headers_read_alike_in_any_split_and_end_where_their_fields_end() {
        let bytes: [u8; BYTES] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= BYTES);
        let split: usize = kani::any();
        kani::assume(split <= length);
        let input = &bytes[..length];

        let mut whole = GzipHeader::new();
        let read_whole = whole.read(input);
        let mut pieces = GzipHeader::new();
        let read_in_pieces = match pieces.read(&input[..split]) {
            Ok(used) if !pieces.is_complete() => {
                assert!(used == split);
                pieces.read(&input[split..]).map(|more| used + more)
            }
            first => first,
        };
        let refused_at = |read: Result<usize, Invalid>| read.map_err(|invalid| invalid.0.len());
        assert!(refused_at(read_whole) == refused_at(read_in_pieces));
        assert!(whole.is_complete() == pieces.is_complete());

        let Ok(used) = read_whole else {
            return;
        };
        assert!(used <= length);
        if !whole.is_complete() {
            assert!(used == length);
            return;
        }
        assert!(input[0] == 0x1f && input[1] == 0x8b && input[2] == 8);
        assert!(input[3] & GZIP_RESERVED == 0);
        assert!(Some(used) == reference_length(input));
    }
}

#[cfg(test)]
pub(crate) mod oracle {
    use std::io::{BufRead, Read, Write};

    pub(crate) struct Random(pub(crate) u64);

    impl Random {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        pub(crate) fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }

    pub(crate) fn sample(seed: u64, size: usize) -> Vec<u8> {
        let mut random = Random(seed * 2 + 1);
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            match random.below(4) {
                0 => {
                    let byte = random.next() as u8;
                    data.extend(std::iter::repeat_n(byte, 1 + random.below(300)));
                }
                1 if !data.is_empty() => {
                    let distance = 1 + random.below(data.len().min(40_000));
                    for _ in 0..3 + random.below(300) {
                        data.push(data[data.len() - distance]);
                    }
                }
                2 => data.extend((0..random.below(200)).map(|_| random.next() as u8)),
                _ => {
                    for _ in 0..random.below(40) {
                        let word = random.below(50) as u8;
                        data.extend_from_slice(&[b'a' + word % 26, b'e', b' ', b't' - word % 7]);
                    }
                }
            }
        }
        data.truncate(size);
        data
    }

    pub(crate) fn raw(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    pub(crate) fn zlib(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    pub(crate) fn gzip(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    pub(crate) fn damaged(stream: &[u8], seed: u64) -> Vec<Vec<u8>> {
        let mut variants = vec![stream.to_vec()];
        let length = stream.len();
        let positions: Vec<usize> = match length <= 160 {
            true => (0..length).collect(),
            false => (0..48)
                .chain(length - 48..length)
                .chain((0..64).map(|index| 48 + index * (length - 96) / 64))
                .collect(),
        };
        let mut random = Random(seed | 1);
        for &at in &positions {
            variants.push(stream[..at].to_vec());
            let mut flipped = stream.to_vec();
            flipped[at] ^= 1 << random.below(8);
            variants.push(flipped);
        }
        for tail in [&[0u8][..], &[0; 8], b"x", &[0x1f, 0x8b]] {
            variants.push([stream, tail].concat());
        }
        variants
    }

    pub(crate) fn block_cases(encode: fn(&[u8], u32) -> Vec<u8>) -> Vec<(Vec<u8>, usize)> {
        let mut cases = Vec::new();
        for (seed, size) in [(21, 0), (22, 1), (23, 600), (24, 4096), (25, 70_000)] {
            let data = sample(seed, size);
            for level in [0, 1, 6, 9] {
                let stream = encode(&data, level);
                for room in [size.saturating_sub(1), size + 1] {
                    cases.push((stream.clone(), room));
                }
                for variant in damaged(&stream, seed + u64::from(level)) {
                    cases.push((variant, size));
                }
            }
        }
        cases
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Outcome {
        Decoded { bytes: Vec<u8>, left: usize },
        Refused,
    }

    fn read_out<R: Read>(mut reader: R, left: impl FnOnce(&mut R) -> usize) -> Outcome {
        let mut bytes = Vec::new();
        match reader.read_to_end(&mut bytes) {
            Ok(_) => Outcome::Decoded {
                left: left(&mut reader),
                bytes,
            },
            Err(_) => Outcome::Refused,
        }
    }

    fn left<R: BufRead>(input: &mut R) -> usize {
        input.fill_buf().unwrap().len()
    }

    pub(crate) fn flate2_reader(stream: &[u8], framing: &str) -> Outcome {
        match framing {
            "raw" => read_out(flate2::bufread::DeflateDecoder::new(stream), |reader| {
                left(reader.get_mut())
            }),
            "zlib" => read_out(flate2::bufread::ZlibDecoder::new(stream), |reader| {
                left(reader.get_mut())
            }),
            _ => read_out(flate2::bufread::MultiGzDecoder::new(stream), |reader| {
                left(reader.get_mut())
            }),
        }
    }

    pub(crate) fn flate2_block(
        zlib: bool,
        history: &[u8],
        input: &[u8],
        size: usize,
    ) -> Result<(super::Decoded, Vec<u8>), ()> {
        let mut inflater = flate2::Decompress::new(zlib);
        if !history.is_empty() {
            inflater.set_dictionary(history).unwrap();
        }
        let mut output = vec![0u8; size + 1];
        match inflater.decompress(input, &mut output, flate2::FlushDecompress::Finish) {
            Ok(flate2::Status::StreamEnd) if inflater.total_out() as usize <= size => {
                output.truncate(inflater.total_out() as usize);
                let decoded = super::Decoded {
                    consumed: inflater.total_in() as usize,
                    written: output.len(),
                };
                Ok((decoded, output))
            }
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::*;
    use super::*;
    use std::io::BufReader;

    fn ours(stream: &[u8], framing: &str, input_piece: usize, output_piece: usize) -> Outcome {
        let input = BufReader::with_capacity(input_piece, stream);
        let mut reader = match framing {
            "raw" => Reader::raw(input),
            "zlib" => Reader::zlib(input),
            _ => Reader::gzip(input),
        };
        let mut bytes = Vec::new();
        let mut piece = vec![0u8; output_piece];
        loop {
            match reader.read(&mut piece) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&piece[..count]),
                Err(_) => {
                    assert!(
                        reader.read(&mut piece).is_err(),
                        "a refused stream read again"
                    );
                    return Outcome::Refused;
                }
            }
        }
        let input = reader.get_mut();
        let left = input.buffer().len() + input.get_ref().len();
        Outcome::Decoded { bytes, left }
    }

    fn ours_held_in_a_window(stream: &[u8], input_piece: usize, window: usize) -> Outcome {
        let mut reader =
            Reader::gzip_with_window(BufReader::with_capacity(input_piece, stream), window);
        let mut bytes = Vec::new();
        loop {
            match reader.fill_buf() {
                Ok([]) => break,
                Ok(held) => {
                    assert!(held.len() <= window);
                    bytes.extend_from_slice(held);
                    let count = held.len();
                    reader.consume(count);
                }
                Err(_) => {
                    assert!(reader.fill_buf().is_err(), "a refused stream read again");
                    return Outcome::Refused;
                }
            }
        }
        let input = reader.get_mut();
        let left = input.buffer().len() + input.get_ref().len();
        Outcome::Decoded { bytes, left }
    }

    #[test]
    fn a_gzip_reader_with_any_window_hands_out_what_flate2_reads_whole_cut_and_flipped() {
        let windows = [SMALLEST_WINDOW, SMALLEST_WINDOW + 1, 100_000, 544 * 1024];
        for (seed, size) in [(31, 0), (32, 900), (33, 70_000), (34, 1_300_000)] {
            let data = sample(seed, size);
            for level in [0, 1, 6] {
                let one = gzip(&data, level);
                let two = [one.clone(), gzip(&sample(seed + 50, 40_000), level)].concat();
                let mut streams = vec![two];
                if size <= 900 {
                    streams.extend(damaged(&one, seed));
                }
                for (index, stream) in streams.iter().enumerate() {
                    let expected = flate2_reader(stream, "gzip");
                    for window in windows {
                        for input_piece in [5, 1 << 12, 1 << 20] {
                            if stream.len() > 20_000 && input_piece < 64 {
                                continue;
                            }
                            assert_eq!(
                                ours_held_in_a_window(stream, input_piece, window),
                                expected,
                                "seed {seed} level {level} stream {index}: a {window}-byte window and {input_piece}-byte input pieces"
                            );
                        }
                    }
                }
            }
        }
    }

    const PIECES: [(usize, usize); 4] = [(1, 1), (3, 7), (64, 4096), (1 << 16, 1 << 20)];

    fn agrees(stream: &[u8], framing: &str, label: &str) -> Outcome {
        let expected = flate2_reader(stream, framing);
        for (input_piece, output_piece) in PIECES {
            if stream.len() > 20_000 && input_piece < 64 {
                continue;
            }
            assert_eq!(
                ours(stream, framing, input_piece, output_piece),
                expected,
                "{label}: {framing} in {input_piece}-byte input and {output_piece}-byte output pieces"
            );
        }
        expected
    }

    fn framed(data: &[u8], level: u32) -> [(&'static str, Vec<u8>); 3] {
        [
            ("raw", raw(data, level)),
            ("zlib", zlib(data, level)),
            ("gzip", gzip(data, level)),
        ]
    }

    #[test]
    fn every_framing_of_generated_data_reads_as_flate2_reads_it_in_every_piece_size() {
        for (seed, size) in [
            (0, 0),
            (1, 1),
            (2, 300),
            (3, 5_000),
            (4, 70_000),
            (5, 300_000),
        ] {
            let data = sample(seed, size);
            for level in [0, 1, 6, 9] {
                for (framing, stream) in framed(&data, level) {
                    let label = format!("seed {seed} size {size} level {level}");
                    let decoded = Outcome::Decoded {
                        bytes: data.clone(),
                        left: 0,
                    };
                    assert_eq!(agrees(&stream, framing, &label), decoded, "{label}");
                }
            }
        }
    }

    #[test]
    fn cut_and_flipped_streams_read_as_flate2_reads_them() {
        for (seed, size) in [(7, 0), (8, 20), (9, 700), (10, 9_000)] {
            let data = sample(seed, size);
            for level in [0, 1, 9] {
                for (framing, stream) in framed(&data, level) {
                    for (index, variant) in damaged(&stream, seed).iter().enumerate() {
                        let label = format!("seed {seed} level {level} variant {index}");
                        agrees(variant, framing, &label);
                    }
                }
            }
        }
    }

    fn header(flags: u8, extra: &[u8], text: [&[u8]; 2], good_crc: bool) -> Vec<u8> {
        let mut header = vec![0x1f, 0x8b, 8, flags, 1, 2, 3, 4, 0, 3];
        if flags & GZIP_EXTRA != 0 {
            header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            header.extend_from_slice(extra);
        }
        for (flag, text) in [GZIP_NAME, GZIP_COMMENT].into_iter().zip(text) {
            if flags & flag != 0 {
                header.extend_from_slice(text);
                header.push(0);
            }
        }
        if flags & GZIP_HEADER_CRC != 0 {
            let crc = crc32fast::hash(&header) as u16 ^ u16::from(!good_crc);
            header.extend_from_slice(&crc.to_le_bytes());
        }
        header
    }

    fn member(header: &[u8], data: &[u8], level: u32) -> Vec<u8> {
        let mut member = header.to_vec();
        member.extend_from_slice(&raw(data, level));
        member.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        member.extend_from_slice(&(data.len() as u32).to_le_bytes());
        member
    }

    #[test]
    fn every_gzip_header_flag_and_member_count_reads_as_multigzdecoder_reads_it() {
        let data = sample(11, 2_000);
        let fields: [(&[u8], [&[u8]; 2]); 3] = [
            (b"", [b"", b""]),
            (b"\x01", [b"n", b"c"]),
            (b"ab\x00cd", [b"name.tar", b"a comment"]),
        ];
        for flags in 0..=u8::MAX {
            for (extra, text) in fields {
                for good_crc in [true, false] {
                    let one = member(&header(flags, extra, text, good_crc), &data, 6);
                    agrees(&one, "gzip", &format!("flags {flags:#x}"));
                    if flags & GZIP_RESERVED != 0 || !good_crc {
                        continue;
                    }
                    let second = header(flags ^ GZIP_NAME, b"", [b"x", b""], true);
                    let two = [one.clone(), member(&second, b"", 1)].concat();
                    for (index, variant) in damaged(&two, u64::from(flags)).iter().enumerate() {
                        agrees(
                            variant,
                            "gzip",
                            &format!("flags {flags:#x} variant {index}"),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn gzip_names_and_comments_stop_at_flate2s_limit() {
        let longest = LONGEST_GZIP_TEXT as usize;
        for flag in [GZIP_NAME, GZIP_COMMENT] {
            for length in [longest - 1, longest, longest + 1] {
                let text = vec![b'n'; length];
                let stream = member(&header(flag, b"", [&text, &text], true), b"data", 6);
                let outcome = agrees(&stream, "gzip", &format!("text of {length} bytes"));
                assert_eq!(outcome == Outcome::Refused, length > longest);
            }
        }
    }

    #[test]
    fn an_empty_input_and_a_member_with_trailing_bytes_read_as_flate2_reads_them() {
        for framing in ["raw", "zlib", "gzip"] {
            assert_eq!(agrees(&[], framing, "empty"), Outcome::Refused);
        }
        let one = gzip(b"hello", 6);
        for tail in [
            &b"\0"[..],
            b"\x1f",
            b"\x1f\x8b\x08\0\0\0\0\0\0\x03",
            b"garbage!!!!",
        ] {
            agrees(&[&one[..], tail].concat(), "gzip", "trailing bytes");
        }
    }

    #[test]
    fn every_zlib_header_is_accepted_exactly_when_zlib_accepts_it() {
        let mut window = Vec::new();
        let mut accepted = 0;
        for cmf in 0..=u8::MAX {
            for flg in 0..=u8::MAX {
                let stream = [cmf, flg, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
                let expected = flate2_block(true, &[], &stream, 0).is_ok();
                assert_eq!(zlib_header(cmf, flg).is_ok(), expected, "{cmf:#x} {flg:#x}");
                let block = zlib_block(&mut window, &stream, &mut []);
                assert_eq!(matches!(block, Ok(Some(_))), expected);
                let read = ours(&stream, "zlib", 1, 1);
                assert_eq!(read == Outcome::Refused, !expected, "{cmf:#x} {flg:#x}");
                accepted += usize::from(expected);
            }
        }
        assert_eq!(accepted, 32);
    }

    fn agrees_as_a_block(zlib: bool, history: &[u8], input: &[u8], size: usize, label: &str) {
        let expected = flate2_block(zlib, history, input, size);
        let mut window = Vec::new();
        let mut output = vec![0x5a; size];
        let actual = match zlib {
            true => zlib_block(&mut window, input, &mut output),
            false => inflate_block(&mut window, history, input, &mut output),
        };
        match (actual, expected) {
            (Ok(Some(decoded)), Ok((wanted, bytes))) => {
                assert_eq!(decoded, wanted, "{label}");
                assert_eq!(&output[..decoded.written], &bytes[..], "{label}");
            }
            (Ok(None) | Err(_), Err(())) => {}
            (actual, expected) => {
                panic!(
                    "{label}: ours {actual:?}, flate2 {:?}",
                    expected.map(|e| e.0)
                )
            }
        }
    }

    fn with_history(data: &[u8], history: &[u8], level: u32) -> Vec<u8> {
        let mut compress = flate2::Compress::new(flate2::Compression::new(level), false);
        compress.set_dictionary(history).unwrap();
        let mut stream = Vec::with_capacity(data.len() + 1024);
        compress
            .compress_vec(data, &mut stream, flate2::FlushCompress::Finish)
            .unwrap();
        stream
    }

    #[test]
    fn blocks_decode_as_flate2_decodes_them_into_every_output_size() {
        for (seed, size) in [(12, 0), (13, 1), (14, 500), (15, 40_000), (16, 140_000)] {
            let data = sample(seed, size);
            for level in [0, 6, 9] {
                for (is_zlib, stream) in [(true, zlib(&data, level)), (false, raw(&data, level))] {
                    for room in [size.saturating_sub(1), size, size + 1, size + 300] {
                        let label = format!("{size} into {room}");
                        agrees_as_a_block(is_zlib, &[], &stream, room, &label);
                    }
                    if size > 40_000 {
                        continue;
                    }
                    for (index, variant) in damaged(&stream, seed).iter().enumerate() {
                        let label = format!("{size} variant {index}");
                        agrees_as_a_block(is_zlib, &[], variant, size, &label);
                    }
                }
                let history = sample(seed + 100, 32_768);
                let stream = with_history(&data, &history, level);
                for cut in [0, 1, 4096, 32_767] {
                    let label = format!("history from {cut}");
                    agrees_as_a_block(false, &history[cut..], &stream, size, &label);
                }
                for (index, variant) in damaged(&stream, seed).iter().enumerate() {
                    let label = format!("history variant {index}");
                    agrees_as_a_block(false, &history, variant, size, &label);
                }
            }
        }
    }

    #[test]
    fn gzip_fixtures_read_as_flate2_reads_them_whole_cut_and_flipped() {
        let fixtures = [
            "tests/data/tar-corpus/gnutar-gnu-gzip.tar.gz",
            "tests/data/tar-corpus/python-pax-gzip.tar.gz",
            "tests/data/tar-corpus/bsdtar-pax-gzip.tgz",
            "tests/data/tar-corpus/python-gnu-gzip-members.tar.gz",
            "tests/data/parity/archives/tar.tar.gz",
            "tests/data/parity/archives/hello.txt.gz",
        ];
        for fixture in fixtures {
            let stream = std::fs::read(fixture).unwrap();
            let whole = agrees(&stream, "gzip", fixture);
            assert!(matches!(whole, Outcome::Decoded { .. }), "{fixture}");
            for at in 0..stream.len() {
                agrees(&stream[..at], "gzip", fixture);
                let mut flipped = stream.clone();
                flipped[at] ^= 1 << (at % 8);
                agrees(&flipped, "gzip", fixture);
            }
        }
    }

    #[test]
    fn adler32_matches_a_byte_at_a_time_reference_in_every_split() {
        fn reference(bytes: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &byte in bytes {
                a = (a + u32::from(byte)) % 65521;
                b = (b + a) % 65521;
            }
            b << 16 | a
        }
        let bytes: Vec<u8> = (0..70_000u32)
            .map(|index| (255 - index % 7) as u8 ^ (index >> 9) as u8)
            .collect();
        for length in [0, 1, 15, 16, 17, 5551, 5552, 5553, 11_104, 70_000] {
            let expected = reference(&bytes[..length]);
            assert_eq!(adler32(&bytes[..length]), expected, "{length}");
            for split in [1, 16, 5000, 5552] {
                let split = split.min(length);
                let mut adler = Adler32::default();
                adler.update(&bytes[..split]);
                adler.update(&bytes[split..length]);
                assert_eq!(adler.sum(), expected, "{length} split at {split}");
            }
        }
        assert_eq!(adler32(&[255; 70_000]), reference(&[255; 70_000]));
    }

    struct InterruptsOnce<'a> {
        bytes: &'a [u8],
        at: usize,
        interrupt_at: usize,
        interrupted: bool,
    }

    impl Read for InterruptsOnce<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = self.fill_buf()?.len().min(output.len());
            output[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for InterruptsOnce<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.at == self.interrupt_at && !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let end = match self.at < self.interrupt_at {
                true => self.interrupt_at,
                false => self.bytes.len(),
            };
            Ok(&self.bytes[self.at..end])
        }

        fn consume(&mut self, amount: usize) {
            self.at += amount;
        }
    }

    #[test]
    fn an_interrupted_read_at_any_byte_is_retried_without_losing_a_member() {
        let first = gzip(&sample(18, 300), 6);
        let stream = [first.clone(), gzip(b"second member", 1)].concat();
        let expected = [sample(18, 300), b"second member".to_vec()].concat();
        for interrupt_at in 0..=stream.len() {
            for framing in ["gzip", "zlib", "raw"] {
                let input = match framing {
                    "gzip" => &stream[..],
                    "zlib" => &zlib(&expected, 6)[..],
                    _ => &raw(&expected, 6)[..],
                };
                if interrupt_at > input.len() {
                    continue;
                }
                let input = InterruptsOnce {
                    bytes: input,
                    at: 0,
                    interrupt_at,
                    interrupted: false,
                };
                let mut reader = match framing {
                    "gzip" => Reader::gzip(input),
                    "zlib" => Reader::zlib(input),
                    _ => Reader::raw(input),
                };
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes).unwrap_or_else(|error| {
                    panic!("{framing} interrupted at {interrupt_at}: {error}")
                });
                assert_eq!(bytes, expected, "{framing} interrupted at {interrupt_at}");
            }
        }
    }

    struct InterruptsEverySecondFill<'a> {
        bytes: &'a [u8],
        fills: usize,
    }

    impl Read for InterruptsEverySecondFill<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = self.fill_buf()?.len().min(output.len());
            output[..count].copy_from_slice(&self.bytes[..count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for InterruptsEverySecondFill<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            self.fills += 1;
            if self.fills.is_multiple_of(2) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(self.bytes)
        }

        fn consume(&mut self, amount: usize) {
            self.bytes = &self.bytes[amount..];
        }
    }

    #[test]
    fn a_source_that_interrupts_every_second_fill_still_makes_progress() {
        let data = [sample(19, 40_000), vec![7; 70_000], sample(20, 3_000)].concat();
        for level in [0, 6] {
            for (framing, stream) in framed(&data, level) {
                let input = InterruptsEverySecondFill {
                    bytes: &stream,
                    fills: 0,
                };
                let mut reader = match framing {
                    "raw" => Reader::raw(input),
                    "zlib" => Reader::zlib(input),
                    _ => Reader::gzip(input),
                };
                let (mut bytes, mut piece) = (Vec::new(), vec![0u8; 4096]);
                let mut interrupted_in_a_row = 0;
                loop {
                    match reader.read(&mut piece) {
                        Ok(0) => break,
                        Ok(count) => {
                            bytes.extend_from_slice(&piece[..count]);
                            interrupted_in_a_row = 0;
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            interrupted_in_a_row += 1;
                            assert!(interrupted_in_a_row < 100, "{framing} level {level} stalls");
                        }
                        Err(error) => panic!("{framing} level {level}: {error}"),
                    }
                }
                assert_eq!(bytes, data, "{framing} level {level}");
            }
        }
    }

    #[test]
    fn a_failed_trailer_stays_failed_when_the_right_trailer_follows() {
        let stream = zlib(b"checked", 6);
        let (body, trailer) = stream.split_at(stream.len() - 4);
        let wrong: Vec<u8> = trailer.iter().map(|byte| byte ^ 1).collect();
        let damaged = [body, &wrong, trailer].concat();
        let mut reader = Reader::zlib(&damaged[..]);
        let mut bytes = Vec::new();
        assert!(reader.read_to_end(&mut bytes).is_err());
        assert!(reader.read(&mut [0u8; 64]).is_err());
    }

    #[test]
    fn the_header_crc_matches_crc32fast() {
        let bytes = sample(17, 1_000);
        let register = bytes
            .iter()
            .fold(u32::MAX, |register, &byte| crc32_byte(register, byte));
        assert_eq!(!register, crc32fast::hash(&bytes));
    }

    #[test]
    fn stored_blocks_fill_a_large_read_in_one_call() {
        let data: Vec<u8> = (0..1u32 << 20)
            .map(|index| ((index * 7) >> 3) as u8)
            .collect();
        for (framing, stream) in framed(&data, 0) {
            let mut reader = match framing {
                "raw" => Reader::raw(BufReader::with_capacity(1 << 21, &stream[..])),
                "zlib" => Reader::zlib(BufReader::with_capacity(1 << 21, &stream[..])),
                _ => Reader::gzip(BufReader::with_capacity(1 << 21, &stream[..])),
            };
            let mut output = vec![0u8; 1 << 20];
            let first = reader.read(&mut output).unwrap();
            assert!(first > SMALLEST_WINDOW, "{framing}: {first} bytes");
            assert_eq!(output[..first], data[..first], "{framing}");
        }
    }

    #[test]
    fn matches_reaching_back_across_stored_blocks_read_as_flate2_reads_them() {
        for case in crate::inflate::tests::matches_reaching_back_across_stored_blocks() {
            let expected = match case.decoded {
                Some(bytes) => Outcome::Decoded { bytes, left: 0 },
                None => Outcome::Refused,
            };
            assert_eq!(agrees(&case.stream, "raw", &case.label), expected);
            for (input_piece, output_piece) in [(1 << 20, 1), (1 << 20, 299), (7, 1 << 20)] {
                assert_eq!(
                    ours(&case.stream, "raw", input_piece, output_piece),
                    expected,
                    "{}: {input_piece}-byte input and {output_piece}-byte output pieces",
                    case.label
                );
            }
        }
    }
}
