use crate::{BadField, Field, field_check};
use anyhow::{Result, ensure};
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
};

const RECORD_MARK: u8 = b':';
pub(crate) const MOST_DATA: usize = 255;
pub(crate) const RECORD_FIELDS: usize = 5;
const MOST_DIGITS: usize = 2 * (MOST_DATA + RECORD_FIELDS);
const MOST_BREAKS: usize = 16;
pub(crate) const SNIFFED_RECORDS: u64 = 3;
const SNIFF_BYTES: u64 = 1 + SNIFFED_RECORDS * (MOST_DIGITS + MOST_BREAKS + 1) as u64;
const DATA: u8 = 0;
const END: u8 = 1;
const SEGMENT: u8 = 2;
const START_SEGMENT: u8 = 3;
const LINEAR: u8 = 4;
const START_LINEAR: u8 = 5;
const ADDRESS_LENGTH: u8 = 2;
const START_LENGTH: u8 = 4;
const SEGMENT_SHIFT: u32 = 4;
const LINEAR_SHIFT: u32 = 16;
const OPEN_BUFFER: usize = 1 << 16;
const AFTER_THE_END: &str = "the file has data after the Intel HEX end record";
const MOST_PADDING: u64 = 64 << 10;
const DOS_END: u8 = 0x1A;

pub(crate) fn hex_digit(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => Some(character - b'0'),
        b'A'..=b'F' => Some(character - b'A' + 10),
        b'a'..=b'f' => Some(character - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn hex_byte(pair: [u8; 2]) -> Field<u8> {
    match (hex_digit(pair[0]), hex_digit(pair[1])) {
        (Some(high), Some(low)) => Ok(high << 4 | low),
        _ => Err(BadField(
            "an Intel HEX record holds a character that is not a hex digit",
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) kind: u8,
    pub(crate) address: u16,
    pub(crate) length: u8,
}

pub(crate) fn record(digits: &[u8], data: &mut [u8]) -> Field<Header> {
    let pair = |index: usize| hex_byte([digits[2 * index], digits[2 * index + 1]]);
    field_check(digits.len() >= 2, "an Intel HEX record has no count")?;
    let length = pair(0)?;
    let bytes = usize::from(length) + RECORD_FIELDS;
    field_check(
        digits.len() == 2 * bytes,
        "an Intel HEX record's digits do not match its count",
    )?;
    field_check(
        data.len() >= usize::from(length),
        "an Intel HEX record holds more data than its buffer",
    )?;
    let mut sum = length;
    let mut fields = [0u8; 3];
    for (index, field) in fields.iter_mut().enumerate() {
        *field = pair(1 + index)?;
        sum = sum.wrapping_add(*field);
    }
    for (index, byte) in data[..usize::from(length)].iter_mut().enumerate() {
        *byte = pair(4 + index)?;
        sum = sum.wrapping_add(*byte);
    }
    sum = sum.wrapping_add(pair(bytes - 1)?);
    field_check(sum == 0, "an Intel HEX record fails its checksum")?;
    Ok(Header {
        kind: fields[2],
        address: u16::from_be_bytes([fields[0], fields[1]]),
        length,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Data { offset: u64, starts_run: bool },
    End,
    Address,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) base: u32,
    pub(crate) records: u64,
    pub(crate) run_end: Option<u64>,
}

impl Layout {
    // EXTERNAL(7-Zip 26.05, run as a black box): its content check refuses an address or start
    // record among the first three records.
    pub(crate) fn step(&mut self, header: Header, data: &[u8]) -> Field<Step> {
        field_check(
            self.records >= SNIFFED_RECORDS || matches!(header.kind, DATA | END),
            "an Intel HEX file holds an address or start record among its first three records, which 7-Zip does not open",
        )?;
        self.records = self.records.saturating_add(1);
        if header.kind == DATA {
            field_check(header.length > 0, "an Intel HEX data record is empty")?;
            let offset = u64::from(self.base) + u64::from(header.address);
            let starts_run = self.run_end != Some(offset);
            self.run_end = Some(offset + u64::from(header.length));
            return Ok(Step::Data { offset, starts_run });
        }
        field_check(
            header.address == 0,
            "an Intel HEX record other than data has an address",
        )?;
        match header.kind {
            END => {
                field_check(
                    header.length == 0,
                    "an Intel HEX end-of-file record holds data",
                )?;
                Ok(Step::End)
            }
            SEGMENT | LINEAR => {
                field_check(
                    header.length == ADDRESS_LENGTH && data.len() >= 2,
                    "an Intel HEX address record does not hold 2 bytes",
                )?;
                let value = u32::from(u16::from_be_bytes([data[0], data[1]]));
                self.base = match header.kind {
                    SEGMENT => value << SEGMENT_SHIFT,
                    _ => value << LINEAR_SHIFT,
                };
                Ok(Step::Address)
            }
            START_SEGMENT | START_LINEAR => {
                field_check(
                    header.length == START_LENGTH,
                    "an Intel HEX start record does not hold 4 bytes",
                )?;
                Ok(Step::Address)
            }
            _ => Err(BadField("an Intel HEX record has a type above 5")),
        }
    }
}

fn is_break(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n')
}

fn is_blank(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n' | b' ' | b'\t')
}

// EXTERNAL(7-Zip 26.05): it extracts a file with any data after the end record and warns. Line
// breaks, spaces, tabs and one DOS end-of-file byte are accepted here, and anything else refused.
pub(crate) fn is_padding(tail: &[u8]) -> bool {
    let blanks = tail.iter().take_while(|&&byte| is_blank(byte)).count();
    let rest = &tail[blanks..];
    let rest = rest.strip_prefix(&[DOS_END]).unwrap_or(rest);
    rest.iter().all(|&byte| is_blank(byte))
}

pub(crate) struct Records<R> {
    input: R,
    digits: [u8; MOST_DIGITS],
    pub(crate) data: [u8; MOST_DATA],
    length: usize,
    layout: Layout,
    pub(crate) ended: bool,
}

fn read_digits(input: &mut impl Read, digits: &mut [u8]) -> Result<()> {
    input
        .read_exact(digits)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => {
                anyhow::anyhow!("the Intel HEX file ends inside a record")
            }
            _ => error.into(),
        })
}

fn next_byte(input: &mut impl BufRead) -> Result<Option<u8>> {
    let byte = input.fill_buf()?.first().copied();
    if byte.is_some() {
        input.consume(1);
    }
    Ok(byte)
}

impl<R: BufRead> Records<R> {
    pub(crate) fn new(mut input: R) -> Result<Self> {
        ensure!(
            next_byte(&mut input)? == Some(RECORD_MARK),
            "the Intel HEX file does not start with a colon"
        );
        Ok(Records {
            input,
            digits: [0; MOST_DIGITS],
            data: [0; MOST_DATA],
            length: 0,
            layout: Layout::default(),
            ended: false,
        })
    }

    fn read_record(&mut self) -> Result<Step> {
        read_digits(&mut self.input, &mut self.digits[..2])?;
        self.length = usize::from(hex_byte([self.digits[0], self.digits[1]])?);
        let digits = 2 * (self.length + RECORD_FIELDS);
        read_digits(&mut self.input, &mut self.digits[2..digits])?;
        let header = record(&self.digits[..digits], &mut self.data)?;
        let step = self.layout.step(header, &self.data[..self.length])?;
        match step {
            Step::End => {
                self.end_line()?;
                self.ended = true;
            }
            _ => self.next_mark()?,
        }
        Ok(step)
    }

    // EXTERNAL(7-Zip 26.05, run as a black box): up to 16 CR or LF bytes may part two records,
    // and the end-of-file record takes one LF, CR or CRLF with it.
    fn next_mark(&mut self) -> Result<()> {
        for _ in 0..=MOST_BREAKS {
            match next_byte(&mut self.input)? {
                Some(RECORD_MARK) => return Ok(()),
                Some(byte) if is_break(byte) => {}
                Some(_) => anyhow::bail!(
                    "an Intel HEX record is followed by a byte that is not a line break"
                ),
                None => anyhow::bail!("the Intel HEX file ends without an end-of-file record"),
            }
        }
        anyhow::bail!("more than {MOST_BREAKS} line breaks part two Intel HEX records")
    }

    fn end_line(&mut self) -> Result<()> {
        let first = self.input.fill_buf()?.first().copied();
        if first.is_some_and(is_break) {
            self.input.consume(1);
        }
        if first == Some(b'\r') && self.input.fill_buf()?.first() == Some(&b'\n') {
            self.input.consume(1);
        }
        Ok(())
    }

    pub(crate) fn next_data(&mut self) -> Result<Option<(bool, usize)>> {
        while !self.ended {
            if let Step::Data { starts_run, .. } = self.read_record()? {
                return Ok(Some((starts_run, self.length)));
            }
        }
        Ok(None)
    }

    pub(crate) fn ensure_nothing_follows(&mut self) -> Result<()> {
        let mut padding = Vec::new();
        (&mut self.input)
            .take(MOST_PADDING + 1)
            .read_to_end(&mut padding)?;
        ensure!(
            padding.len() as u64 <= MOST_PADDING && is_padding(&padding),
            AFTER_THE_END
        );
        Ok(())
    }
}

pub(crate) fn sniff(path: &Path) -> Result<bool> {
    let mut start = Vec::with_capacity(SNIFF_BYTES as usize);
    crate::volumes::open(path)?
        .take(SNIFF_BYTES)
        .read_to_end(&mut start)?;
    Ok(starts_as_intel_hex(&start))
}

fn starts_as_intel_hex(start: &[u8]) -> bool {
    let Ok(mut records) = Records::new(start) else {
        return false;
    };
    for _ in 0..SNIFFED_RECORDS {
        match records.read_record() {
            Ok(Step::End) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    true
}

// EXTERNAL(7-Zip 26.05, run as a black box): a file that passes the content check but fails a
// record further on is not Intel HEX to 7-Zip, which goes on to other formats.
pub(crate) fn opens(path: &Path) -> Result<bool> {
    if !sniff(path)? {
        return Ok(false);
    }
    let mut records = Records::new(BufReader::with_capacity(
        OPEN_BUFFER,
        crate::volumes::open(path)?,
    ))?;
    while let Ok(Some(_)) = records.next_data() {}
    Ok(records.ended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_padding(tail: &[u8]) -> bool {
        let blank = |byte: &u8| b"\r\n \t".contains(byte);
        let ends = tail.iter().filter(|&&byte| byte == DOS_END).count();
        ends <= 1 && tail.iter().all(|byte| blank(byte) || *byte == DOS_END)
    }

    #[test]
    fn every_short_tail_is_padding_exactly_when_a_plain_reference_says_so() {
        let alphabet = [b'\r', b'\n', b' ', b'\t', DOS_END, b'x', 0, 0x0C, b':'];
        let mut tails = vec![Vec::new()];
        let mut checked = 0;
        for _ in 0..6 {
            tails = tails
                .iter()
                .flat_map(|tail| alphabet.map(|byte| [tail.as_slice(), &[byte]].concat()))
                .collect();
            for tail in &tails {
                assert_eq!(is_padding(tail), reference_padding(tail), "{tail:?}");
                checked += 1;
            }
        }
        assert!(checked > 500_000);
    }

    #[test]
    fn every_pair_of_characters_reads_as_rust_reads_two_hex_digits() {
        for high in 0..=u8::MAX {
            for low in 0..=u8::MAX {
                let pair = [high, low];
                let expected = std::str::from_utf8(&pair)
                    .ok()
                    .filter(|text| text.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .and_then(|text| u8::from_str_radix(text, 16).ok());
                assert_eq!(hex_byte(pair).ok(), expected, "{pair:?}");
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const DIGITS: usize = 2 * (RECORD_FIELDS + 2);

    fn pair(digits: &[u8], index: usize) -> u8 {
        hex_byte([digits[2 * index], digits[2 * index + 1]]).unwrap()
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn records_never_panic_and_keep_their_count_fields_and_checksum() {
        let digits: [u8; DIGITS] = kani::any();
        let given: usize = kani::any_where(|&given| given <= DIGITS);
        let mut data = [0u8; 2];
        if let Ok(header) = record(&digits[..given], &mut data) {
            let length = usize::from(header.length);
            assert!(given == 2 * (length + RECORD_FIELDS));
            assert!(pair(&digits, 0) == header.length);
            assert!(u16::from_be_bytes([pair(&digits, 1), pair(&digits, 2)]) == header.address);
            assert!(pair(&digits, 3) == header.kind);
            let mut sum = 0u8;
            for index in 0..given / 2 {
                sum = sum.wrapping_add(pair(&digits, index));
            }
            assert!(sum == 0);
            for (index, byte) in data[..length].iter().enumerate() {
                assert!(*byte == pair(&digits, 4 + index));
            }
        }
    }

    #[kani::proof]
    fn steps_never_panic_and_grow_a_run_only_from_where_it_ended() {
        let before = Layout {
            base: kani::any(),
            records: kani::any(),
            run_end: kani::any(),
        };
        let header = Header {
            kind: kani::any(),
            address: kani::any(),
            length: kani::any(),
        };
        let data: [u8; 4] = kani::any();
        let given = usize::from(header.length).min(data.len());
        let mut layout = before;
        let step = layout.step(header, &data[..given]);
        if before.records < SNIFFED_RECORDS && !matches!(header.kind, DATA | END) {
            assert!(step.is_err());
        }
        match step {
            Ok(Step::Data { offset, starts_run }) => {
                assert!(header.kind == DATA && header.length > 0);
                assert!(offset == u64::from(before.base) + u64::from(header.address));
                assert!(starts_run == (before.run_end != Some(offset)));
                assert!(layout.run_end == Some(offset + u64::from(header.length)));
                assert!(layout.base == before.base);
            }
            Ok(Step::End) => {
                assert!(header.kind == END && header.length == 0 && header.address == 0);
                assert!(layout.base == before.base && layout.run_end == before.run_end);
            }
            Ok(Step::Address) => {
                assert!(header.address == 0 && layout.run_end == before.run_end);
                let value = u32::from(u16::from_be_bytes([data[0], data[1]]));
                match header.kind {
                    SEGMENT => assert!(header.length == 2 && layout.base == value << 4),
                    LINEAR => assert!(header.length == 2 && layout.base == value << 16),
                    START_SEGMENT | START_LINEAR => {
                        assert!(header.length == 4 && layout.base == before.base)
                    }
                    _ => panic!("a record of an unknown type was accepted"),
                }
            }
            Err(_) => assert!(layout.base == before.base && layout.run_end == before.run_end),
        }
    }
}
