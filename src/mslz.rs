use crate::{BadField, Field, field_check};
use std::io::{self, BufRead, Read};

pub(crate) const SIGNATURE: [u8; 9] = *b"SZDD\x88\xf0\x27\x33\x41";
pub(crate) const HEADER: usize = 14;
// EXTERNAL(7-Zip 26.06 MslzHandler.cpp, kUnpackSizeMax): Windows' LZ expand stops at 2 GiB.
const LARGEST_SIZE: u32 = 1 << 31;
const WINDOW: usize = 4096;
const WINDOW_MASK: u16 = WINDOW as u16 - 1;
// EXTERNAL(7-Zip 26.05 MslzHandler.cpp, MslzDec): a copy reads window slot code + 16, and a copy
// longer than 16 bytes, which Microsoft's encoder never writes, is refused.
const SLOT_SHIFT: u16 = 16;
const SHORTEST_COPY: u8 = 3;
const LONGEST_COPY: u8 = 16;
const FLAG_SENTINEL: u16 = 0x100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) last_character: u8,
    pub(crate) size: u32,
}

pub(crate) fn header(bytes: [u8; HEADER]) -> Field<Header> {
    let [.., last_character, s0, s1, s2, s3] = bytes;
    field_check(
        bytes[..SIGNATURE.len()] == SIGNATURE,
        "the file does not start with the SZDD signature",
    )?;
    let size = u32::from_le_bytes([s0, s1, s2, s3]);
    field_check(
        size <= LARGEST_SIZE,
        "the SZDD file declares more than 2 GiB of output, which Windows refuses",
    )?;
    Ok(Header {
        last_character,
        size,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Flags,
    Token,
    SecondCodeByte(u8),
}

pub(crate) struct Decoder {
    window: [u8; WINDOW],
    written: u32,
    size: u32,
    flags: u16,
    next: Next,
    copy_from: u16,
    copy_left: u8,
}

impl Decoder {
    pub(crate) fn new(size: u32) -> Self {
        Decoder {
            window: [b' '; WINDOW],
            written: 0,
            size,
            flags: 0,
            next: Next::Flags,
            copy_from: 0,
            copy_left: 0,
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.written == self.size && self.copy_left == 0
    }

    // EXTERNAL(7-Zip 26.06 MslzHandler.cpp): the flag bits after the last token must be clear.
    pub(crate) fn ends_cleanly(&self) -> bool {
        self.finished() && (self.next == Next::Flags || self.flags.is_power_of_two())
    }

    fn push(&mut self, byte: u8) {
        self.window[(self.written as usize) & (WINDOW - 1)] = byte;
        self.written += 1;
    }

    pub(crate) fn decode(&mut self, input: &[u8], out: &mut [u8]) -> Field<(usize, usize)> {
        let (mut consumed, mut produced) = (0, 0);
        loop {
            while self.copy_left > 0 && produced < out.len() {
                let byte = self.window[usize::from(self.copy_from & WINDOW_MASK)];
                self.copy_from = (self.copy_from + 1) & WINDOW_MASK;
                self.copy_left -= 1;
                self.push(byte);
                out[produced] = byte;
                produced += 1;
            }
            if produced == out.len() || self.finished() || consumed == input.len() {
                return Ok((consumed, produced));
            }
            let byte = input[consumed];
            consumed += 1;
            match self.next {
                Next::Flags => {
                    self.flags = u16::from(byte) | FLAG_SENTINEL;
                    self.next = Next::Token;
                }
                Next::Token if self.flags & 1 == 1 => {
                    self.take_flag();
                    self.push(byte);
                    out[produced] = byte;
                    produced += 1;
                }
                Next::Token => self.next = Next::SecondCodeByte(byte),
                Next::SecondCodeByte(first) => {
                    self.take_flag();
                    let length = (byte & 0x0f) + SHORTEST_COPY;
                    field_check(
                        length <= LONGEST_COPY,
                        "an SZDD copy is longer than 16 bytes",
                    )?;
                    field_check(
                        u32::from(length) <= self.size - self.written,
                        "an SZDD copy runs past the declared size",
                    )?;
                    let code = u16::from(byte & 0xf0) << 4 | u16::from(first);
                    self.copy_from = (code + SLOT_SHIFT) & WINDOW_MASK;
                    self.copy_left = length;
                }
            }
        }
    }

    fn take_flag(&mut self) {
        self.flags >>= 1;
        self.next = if self.flags > 1 {
            Next::Token
        } else {
            Next::Flags
        };
    }
}

fn data_error(error: BadField) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

pub(crate) struct SzddReader<R> {
    input: R,
    decoder: Decoder,
    checked_end: bool,
}

impl<R: BufRead> SzddReader<R> {
    pub(crate) fn new(input: R, size: u32) -> Self {
        SzddReader {
            input,
            decoder: Decoder::new(size),
            checked_end: false,
        }
    }
}

impl<R: BufRead> Read for SzddReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.checked_end {
            return Ok(0);
        }
        loop {
            let input = self.input.fill_buf()?;
            let ended = input.is_empty();
            let (consumed, produced) = self.decoder.decode(input, out).map_err(data_error)?;
            self.input.consume(consumed);
            if produced > 0 {
                return Ok(produced);
            }
            if self.decoder.finished() {
                if !self.decoder.ends_cleanly() {
                    return Err(data_error(BadField(
                        "the SZDD stream sets flag bits after its last token",
                    )));
                }
                if !self.input.fill_buf()?.is_empty() {
                    return Err(data_error(BadField(
                        "the SZDD file has data after its stream",
                    )));
                }
                self.checked_end = true;
                return Ok(0);
            }
            if ended {
                return Err(data_error(BadField("the SZDD stream is truncated")));
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn szdd_headers_keep_their_fields_and_refuse_sizes_past_2_gib() {
        let bytes: [u8; HEADER] = kani::any();
        let size = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        match header(bytes) {
            Ok(header) => {
                assert!(bytes[..9] == SIGNATURE);
                assert!(header.size == size && size <= 1 << 31);
                assert!(header.last_character == bytes[9]);
            }
            Err(_) => assert!(bytes[..9] != SIGNATURE || size > 1 << 31),
        }
    }

    #[kani::proof]
    #[kani::unwind(25)]
    fn decoding_from_any_state_stays_in_the_window_and_the_declared_size() {
        let mut decoder = Decoder::new(kani::any());
        kani::assume(decoder.size <= LARGEST_SIZE);
        decoder.written = kani::any();
        decoder.flags = kani::any();
        decoder.copy_from = kani::any();
        decoder.copy_left = kani::any();
        decoder.next = match kani::any::<u8>() % 3 {
            0 => Next::Flags,
            1 => Next::Token,
            _ => Next::SecondCodeByte(kani::any()),
        };
        kani::assume(decoder.copy_left <= LONGEST_COPY && decoder.copy_from <= WINDOW_MASK);
        kani::assume(
            u64::from(decoder.written) + u64::from(decoder.copy_left) <= u64::from(decoder.size),
        );
        let before = decoder.written;
        let input: [u8; 2] = kani::any();
        let mut out = [0u8; 20];
        if let Ok((consumed, produced)) = decoder.decode(&input, &mut out) {
            assert!(consumed <= input.len() && produced <= out.len());
            assert!(decoder.written == before + produced as u32);
            assert!(decoder.copy_left <= LONGEST_COPY && decoder.copy_from <= WINDOW_MASK);
            assert!(
                u64::from(decoder.written) + u64::from(decoder.copy_left)
                    <= u64::from(decoder.size)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(body: &[u8], size: u32) -> Option<Vec<u8>> {
        let mut ring = [b' '; 4096];
        let mut out = Vec::new();
        let mut at = 0;
        let mut next = || {
            let byte = body.get(at).copied();
            at += 1;
            byte
        };
        let mut unused_flags = 0;
        while (out.len() as u32) < size {
            let flags = next()?;
            unused_flags = 0;
            for bit in 0..8 {
                if out.len() as u32 == size {
                    unused_flags = flags >> bit;
                    break;
                }
                let first = next()?;
                if flags >> bit & 1 == 1 {
                    ring[out.len() % 4096] = first;
                    out.push(first);
                    continue;
                }
                let second = next()?;
                let length = usize::from(second & 0x0f) + 3;
                if length > 16 || out.len() + length > size as usize {
                    return None;
                }
                let from = ((usize::from(second & 0xf0) << 4) | usize::from(first)) + 16;
                for offset in 0..length {
                    let byte = ring[(from + offset) % 4096];
                    ring[out.len() % 4096] = byte;
                    out.push(byte);
                }
            }
        }
        (unused_flags == 0 && at == body.len()).then_some(out)
    }

    fn decoded(body: &[u8], size: u32, input_chunk: usize, output_chunk: usize) -> Option<Vec<u8>> {
        let input = std::io::BufReader::with_capacity(input_chunk, body);
        let mut reader = SzddReader::new(input, size);
        let mut out = Vec::new();
        let mut buffer = vec![0; output_chunk];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return Some(out),
                Ok(read) => out.extend_from_slice(&buffer[..read]),
                Err(_) => return None,
            }
            assert!(out.len() <= size as usize, "decoded past the declared size");
        }
    }

    #[test]
    fn every_pseudo_random_stream_decodes_as_the_reference_does_at_every_split() {
        let mut state = 0x5a5a_1234_u64;
        let mut random = |limit: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % limit
        };
        let (mut accepted, mut refused) = (0, 0);
        for case in 0..20_000 {
            let tokens = random(if case % 10 == 0 { 300 } else { 12 });
            let (mut body, mut size) = (Vec::new(), 0u32);
            for group in 0..tokens.div_ceil(8) {
                let flags = random(256) as u8;
                body.push(flags);
                for bit in 0..(tokens - group * 8).min(8) {
                    if flags >> bit & 1 == 1 {
                        body.push(random(256) as u8);
                        size += 1;
                    } else {
                        let second = random(256) as u8;
                        body.extend([random(256) as u8, second]);
                        size += u32::from(second & 0x0f) + 3;
                    }
                }
            }
            match random(6) {
                0 if !body.is_empty() => {
                    let at = random(body.len() as u64) as usize;
                    body[at] ^= 1 << random(8);
                }
                1 => size = size.saturating_sub(random(4) as u32),
                2 => size += random(4) as u32,
                3 => body.push(random(256) as u8),
                4 => body.truncate(random(body.len() as u64 + 1) as usize),
                _ => {}
            }
            let expected = reference(&body, size);
            for (input_chunk, output_chunk) in [(1, 1), (2, 3), (7, 4096), (65536, 17)] {
                assert_eq!(
                    decoded(&body, size, input_chunk, output_chunk),
                    expected,
                    "case {case}: {body:?} to {size} bytes, reads of {input_chunk} and {output_chunk}"
                );
            }
            if expected.is_some() {
                accepted += 1;
            } else {
                refused += 1;
            }
        }
        assert!(
            accepted > 1000 && refused > 1000,
            "{accepted} accepted, {refused} refused"
        );
    }
}
