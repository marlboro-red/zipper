use crate::{BadField, Field, field_check};
use std::io::{self, BufRead, Read};

pub(crate) const EXTENSION: &str = ".b64";
// EXTERNAL(7-Zip Base64Handler.cpp, CHandler::Open): 7-Zip checks the first 4 KiB with IsArc_Base64.
pub(crate) const SNIFF_BYTES: usize = 4096;
const EQUALS: u8 = 64;
const SPACE: u8 = 65;
const OTHER: u8 = 66;
const QUAD_FULL: u32 = 1 << 24;
const TWO_SYMBOLS: u32 = 1 << 12;
const THREE_SYMBOLS: u32 = 1 << 18;
// EXTERNAL(7-Zip Base64Handler.cpp, IsArc_Base64): a space byte after 1 to 19 symbols and
// before the first `=` rules a file out, to keep ordinary text from passing for Base64.
const SYMBOLS_BEFORE_A_SPACE: usize = 20;

// EXTERNAL(7-Zip Base64Handler.cpp, k_Base64Table): tab, line feed, carriage return, space
// and 0xA0 count as spaces, and every other byte outside the alphabet as a stray byte.
const fn code(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        b'=' => EQUALS,
        b'\t' | b'\n' | b'\r' | b' ' | 0xa0 => SPACE,
        _ => OTHER,
    }
}

// EXTERNAL(7-Zip Base64Handler.cpp, IsArc_Base64): the first run of symbols must end at `=`
// after two or three symbols of a quad, with only spaces after it, or run past the bytes given.
pub(crate) fn looks_like_base64(start: &[u8]) -> bool {
    let mut bytes = start.iter();
    let (mut symbols, mut first_space) = (0usize, 0usize);
    loop {
        let Some(&byte) = bytes.next() else {
            return true;
        };
        match code(byte) {
            EQUALS => break,
            SPACE => {
                if byte == b' ' && first_space == 0 {
                    first_space = symbols;
                }
            }
            OTHER => return false,
            _ => symbols += 1,
        }
    }
    if first_space != 0 && first_space < symbols && first_space < SYMBOLS_BEFORE_A_SPACE {
        return false;
    }
    match symbols % 4 {
        0 | 1 => return false,
        2 => match bytes.next() {
            None => return true,
            Some(&byte) if code(byte) != EQUALS => return false,
            Some(_) => {}
        },
        _ => {}
    }
    bytes.all(|&byte| code(byte) == SPACE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Symbols,
    SecondEquals,
    Finished,
}

pub(crate) struct Decoder {
    value: u32,
    phase: Phase,
    pending: [u8; 3],
    pending_start: usize,
    pending_end: usize,
    produced_any: bool,
}

impl Decoder {
    pub(crate) fn new() -> Self {
        Decoder {
            value: 1,
            phase: Phase::Symbols,
            pending: [0; 3],
            pending_start: 0,
            pending_end: 0,
            produced_any: false,
        }
    }

    fn hold(&mut self, bytes: &[u8]) {
        self.pending[..bytes.len()].copy_from_slice(bytes);
        self.pending_start = 0;
        self.pending_end = bytes.len();
        self.produced_any |= !bytes.is_empty();
    }

    // EXTERNAL(7-Zip Base64Handler.cpp, Base64ToBin): `=` ends the stream after two or three
    // symbols of a quad, and after two a second `=` must follow at once.
    pub(crate) fn decode(&mut self, input: &[u8], out: &mut [u8]) -> Field<(usize, usize)> {
        let (mut consumed, mut produced) = (0, 0);
        loop {
            while self.pending_start < self.pending_end && produced < out.len() {
                out[produced] = self.pending[self.pending_start];
                self.pending_start += 1;
                produced += 1;
            }
            if self.pending_start < self.pending_end || consumed == input.len() {
                return Ok((consumed, produced));
            }
            let symbol = code(input[consumed]);
            consumed += 1;
            match (self.phase, symbol) {
                (Phase::Finished, SPACE) | (Phase::Symbols, SPACE) => {}
                (Phase::Finished, _) => {
                    return Err(BadField("the Base64 file has data after its end"));
                }
                (Phase::Symbols, EQUALS) if self.value >= THREE_SYMBOLS => {
                    let value = self.value;
                    self.hold(&[(value >> 10) as u8, (value >> 2) as u8]);
                    self.phase = Phase::Finished;
                }
                (Phase::Symbols, EQUALS) => {
                    field_check(
                        self.value >= TWO_SYMBOLS,
                        "a Base64 `=` comes before the second symbol of a quad",
                    )?;
                    self.phase = Phase::SecondEquals;
                }
                (Phase::Symbols, OTHER) => {
                    return Err(BadField(
                        "the Base64 file holds a byte outside the alphabet",
                    ));
                }
                (Phase::Symbols, _) => {
                    self.value = self.value << 6 | u32::from(symbol);
                    if self.value & QUAD_FULL != 0 {
                        let value = self.value;
                        self.hold(&[(value >> 16) as u8, (value >> 8) as u8, value as u8]);
                        self.value = 1;
                    }
                }
                (Phase::SecondEquals, EQUALS) => {
                    let value = self.value;
                    self.hold(&[(value >> 4) as u8]);
                    self.phase = Phase::Finished;
                }
                (Phase::SecondEquals, _) => {
                    return Err(BadField("a Base64 `=` after two symbols is not doubled"));
                }
            }
        }
    }

    pub(crate) fn end(&self) -> Field<()> {
        field_check(
            self.phase == Phase::Finished || self.phase == Phase::Symbols && self.value == 1,
            "the Base64 stream is truncated inside a quad",
        )?;
        field_check(self.produced_any, "the Base64 file decodes to nothing")
    }
}

fn data_error(error: BadField) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

pub(crate) struct Base64Reader<R> {
    input: R,
    decoder: Decoder,
    checked_end: bool,
}

impl<R: BufRead> Base64Reader<R> {
    pub(crate) fn new(input: R) -> Self {
        Base64Reader {
            input,
            decoder: Decoder::new(),
            checked_end: false,
        }
    }
}

impl<R: BufRead> Read for Base64Reader<R> {
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
            if ended {
                self.decoder.end().map_err(data_error)?;
                self.checked_end = true;
                return Ok(0);
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn the_base64_check_never_panics_on_any_short_start() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if looks_like_base64(&bytes[..length]) {
            assert!(bytes[..length].iter().all(|&byte| code(byte) != OTHER));
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn decoding_from_any_state_never_panics_and_keeps_its_pending_bytes_in_range() {
        let mut decoder = Decoder::new();
        decoder.value = kani::any();
        kani::assume(decoder.value >= 1 && matches!(decoder.value.ilog2(), 0 | 6 | 12 | 18));
        decoder.phase = match kani::any::<u8>() % 3 {
            0 => Phase::Symbols,
            1 => Phase::SecondEquals,
            _ => Phase::Finished,
        };
        decoder.pending_start = kani::any();
        decoder.pending_end = kani::any();
        kani::assume(decoder.pending_start <= decoder.pending_end && decoder.pending_end <= 3);
        let input: [u8; 3] = kani::any();
        let mut out = [0u8; 2];
        if let Ok((consumed, produced)) = decoder.decode(&input, &mut out) {
            assert!(consumed <= input.len() && produced <= out.len());
            assert!(decoder.pending_start <= decoder.pending_end && decoder.pending_end <= 3);
            assert!(decoder.value >= 1 && matches!(decoder.value.ilog2(), 0 | 6 | 12 | 18));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE_SPACE: [u8; 5] = [9, 10, 13, 32, 0xa0];

    fn table(byte: u8) -> u32 {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        match alphabet.iter().position(|&symbol| symbol == byte) {
            Some(at) => at as u32,
            None if byte == b'=' => 64,
            None if TABLE_SPACE.contains(&byte) => 65,
            None => 77,
        }
    }

    fn reference_is_arc(p: &[u8]) -> bool {
        let (mut at, mut num, mut first_space) = (0, 0, 0);
        loop {
            let Some(&byte) = p.get(at) else { return true };
            at += 1;
            let c = table(byte);
            if c < 64 {
                num += 1;
                continue;
            }
            if c == 65 {
                if byte == b' ' && first_space == 0 {
                    first_space = num;
                }
                continue;
            }
            if c != 64 {
                return false;
            }
            break;
        }
        if first_space != 0 && first_space < num && first_space < 20 {
            return false;
        }
        num &= 3;
        if num <= 1 {
            return false;
        }
        if num != 3 {
            let Some(&byte) = p.get(at) else { return true };
            at += 1;
            if table(byte) != 64 {
                return false;
            }
        }
        p[at..].iter().all(|&byte| table(byte) == 65)
    }

    fn reference_decode(p: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut val: u32 = 1;
        let mut at = 0;
        let mut finished_at = None;
        while at < p.len() {
            let c = table(p[at]);
            at += 1;
            if c < 64 {
                val = val << 6 | c;
                if val & 1 << 24 != 0 {
                    out.extend([(val >> 16) as u8, (val >> 8) as u8, val as u8]);
                    val = 1;
                }
                continue;
            }
            if c == 65 {
                continue;
            }
            if c != 64 || val < 1 << 12 {
                return None;
            }
            if val & 1 << 18 == 0 {
                if p.get(at).map(|&byte| table(byte)) != Some(64) {
                    return None;
                }
                at += 1;
                out.push((val >> 4) as u8);
            } else {
                out.extend([(val >> 10) as u8, (val >> 2) as u8]);
            }
            val = 1;
            finished_at = Some(at);
            break;
        }
        if let Some(end) = finished_at
            && !p[end..].iter().all(|&byte| table(byte) == 65)
        {
            return None;
        }
        (val == 1 && !out.is_empty()).then_some(out)
    }

    fn decoded(text: &[u8], input_chunk: usize, output_chunk: usize) -> Option<Vec<u8>> {
        let mut reader = Base64Reader::new(std::io::BufReader::with_capacity(input_chunk, text));
        let mut out = Vec::new();
        let mut buffer = vec![0; output_chunk];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return Some(out),
                Ok(read) => out.extend_from_slice(&buffer[..read]),
                Err(_) => return None,
            }
        }
    }

    #[test]
    fn every_pseudo_random_text_checks_and_decodes_as_the_reference_does_at_every_split() {
        let mut state = 0xb64_cafe_u64;
        let mut random = |limit: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % limit
        };
        let bytes = b"QUFBQUFBQUFBQUFB+/09az== \n\t\r\xa0!\0";
        let (mut accepted, mut refused, mut sniffed) = (0, 0, 0);
        for _ in 0..50_000 {
            let length = random(30) as usize;
            let text: Vec<u8> = (0..length)
                .map(|_| bytes[random(bytes.len() as u64) as usize])
                .collect();
            assert_eq!(
                looks_like_base64(&text),
                reference_is_arc(&text),
                "{text:?}"
            );
            sniffed += usize::from(reference_is_arc(&text));
            let expected = reference_decode(&text);
            for (input_chunk, output_chunk) in [(1, 1), (2, 2), (5, 64)] {
                assert_eq!(
                    decoded(&text, input_chunk, output_chunk),
                    expected,
                    "{text:?}"
                );
            }
            if expected.is_some() {
                accepted += 1;
            } else {
                refused += 1;
            }
        }
        assert!(
            accepted > 1000 && refused > 1000 && sniffed > 1000,
            "{accepted} accepted, {refused} refused, {sniffed} sniffed"
        );
    }
}
