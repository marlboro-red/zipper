use crate::{BadField, Field, field_check};
use std::io::{self, Read};

pub(crate) const LONGEST_CODE: usize = 16;
const INPUT_BUFFER: usize = 64 * 1024;
const TREE_SYMBOLS: usize = 19;
const TREE_COUNT_BITS: u32 = 5;
const TREE_SKIP_AFTER: usize = 3;
pub(crate) const CHARACTERS: usize = 510;
const CHARACTER_COUNT_BITS: u32 = 9;
pub(crate) const POSITIONS: usize = 26;
const SHORT_MATCH: usize = 3;
const BLOCK_SIZE_BITS: u32 = 16;
const SMALL_DICTIONARY: u32 = 1 << 14;
const CRC16_POLY: u16 = 0xA001;
const CRC16_TABLE: [u16; 256] = crc16_table();

const fn crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u16;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ CRC16_POLY
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

pub(crate) fn crc16(mut crc: u16, bytes: &[u8]) -> u16 {
    for &byte in bytes {
        crc = CRC16_TABLE[usize::from((crc ^ u16::from(byte)) as u8)] ^ (crc >> 8);
    }
    crc
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct Huffman<const N: usize> {
    symbols: [u16; N],
    first: [u32; LONGEST_CODE + 1],
    limit: [u32; LONGEST_CODE + 1],
    base: [u16; LONGEST_CODE + 1],
    single: Option<u16>,
}

impl<const N: usize> Huffman<N> {
    pub(crate) fn new() -> Self {
        Huffman {
            symbols: [0; N],
            first: [0; LONGEST_CODE + 1],
            limit: [0; LONGEST_CODE + 1],
            base: [0; LONGEST_CODE + 1],
            single: Some(0),
        }
    }

    pub(crate) fn single(&mut self, symbol: u16) {
        self.single = Some(symbol);
    }

    pub(crate) fn build(&mut self, lengths: &[u8; N]) -> Field<()> {
        let mut counts = [0u32; LONGEST_CODE + 1];
        for &length in lengths {
            field_check(
                usize::from(length) <= LONGEST_CODE,
                "LZH Huffman code is longer than 16 bits",
            )?;
            counts[usize::from(length)] += 1;
        }
        let mut code = 0u32;
        let mut index = 0u32;
        for (length, &count) in counts.iter().enumerate().skip(1) {
            self.first[length] = code;
            self.base[length] = index as u16;
            code += count << (LONGEST_CODE - length);
            index += count;
            self.limit[length] = code;
        }
        field_check(
            code == 1 << LONGEST_CODE,
            "LZH Huffman table is incomplete or oversubscribed",
        )?;
        let mut next = self.base;
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = &mut next[usize::from(length)];
                self.symbols[usize::from(*slot)] = symbol as u16;
                *slot += 1;
            }
        }
        self.single = None;
        Ok(())
    }

    pub(crate) fn lookup(&self, peek: u32) -> (u16, u32) {
        if let Some(symbol) = self.single {
            return (symbol, 0);
        }
        let mut length = 1;
        while length < LONGEST_CODE && peek >= self.limit[length] {
            length += 1;
        }
        let offset = (peek - self.first[length]) >> (LONGEST_CODE - length);
        let index = usize::from(self.base[length]) + offset as usize;
        (self.symbols[index], length as u32)
    }

    fn decode<R: Read>(&self, bits: &mut Bits<R>) -> io::Result<u16> {
        let (symbol, length) = self.lookup(bits.peek(LONGEST_CODE as u32)?);
        bits.consume(length)?;
        Ok(symbol)
    }
}

pub(crate) struct Bits<R> {
    inner: R,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
    value: u64,
    count: u32,
    taken: u64,
    packed: u64,
    inner_done: bool,
}

impl<R: Read> Bits<R> {
    pub(crate) fn new(inner: R, packed: u64) -> Self {
        Bits {
            inner,
            buffer: vec![0; INPUT_BUFFER].into_boxed_slice(),
            start: 0,
            end: 0,
            value: 0,
            count: 0,
            taken: 0,
            packed,
            inner_done: false,
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        while self.count <= 48 {
            if self.start == self.end {
                if self.inner_done || self.taken == self.packed {
                    return Ok(());
                }
                let room = (self.packed - self.taken).min(self.buffer.len() as u64) as usize;
                let read = self.inner.read(&mut self.buffer[..room])?;
                if read == 0 {
                    self.inner_done = true;
                    return Ok(());
                }
                self.taken += read as u64;
                self.start = 0;
                self.end = read;
            }
            self.value = self.value << 8 | u64::from(self.buffer[self.start]);
            self.start += 1;
            self.count += 8;
        }
        Ok(())
    }

    pub(crate) fn peek(&mut self, count: u32) -> io::Result<u32> {
        if self.count < count {
            self.refill()?;
        }
        let bits = if self.count >= count {
            self.value >> (self.count - count)
        } else {
            self.value << (count - self.count)
        };
        Ok((bits & ((1u64 << count) - 1)) as u32)
    }

    pub(crate) fn consume(&mut self, count: u32) -> io::Result<()> {
        if self.count < count {
            self.refill()?;
        }
        if self.count < count {
            return Err(invalid("compressed data ends early"));
        }
        self.count -= count;
        self.value &= (1u64 << self.count) - 1;
        Ok(())
    }

    pub(crate) fn bits(&mut self, count: u32) -> io::Result<u32> {
        let value = self.peek(count)?;
        self.consume(count)?;
        Ok(value)
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        let padding = self.count % 8;
        if self.bits(padding)? != 0 {
            return Err(invalid("compressed data ends with bits that are not zero"));
        }
        self.refill()?;
        if self.count != 0 || self.taken != self.packed {
            return Err(invalid("compressed data does not end at its packed size"));
        }
        Ok(())
    }
}

pub(crate) struct Window {
    bytes: Box<[u8]>,
    mask: usize,
    position: usize,
    decoded: u64,
}

impl Window {
    pub(crate) fn new(dictionary: u32) -> Self {
        let size = (dictionary as usize).next_power_of_two();
        Window {
            bytes: vec![0; size].into_boxed_slice(),
            mask: size - 1,
            position: 0,
            decoded: 0,
        }
    }

    pub(crate) fn put(&mut self, byte: u8) -> u8 {
        self.bytes[self.position] = byte;
        self.position = (self.position + 1) & self.mask;
        self.decoded += 1;
        byte
    }

    pub(crate) fn reaches(&self, distance: u32) -> bool {
        u64::from(distance) < self.decoded
    }

    pub(crate) fn repeat(&mut self, distance: u32) -> u8 {
        let from = self.position.wrapping_sub(distance as usize + 1) & self.mask;
        self.put(self.bytes[from])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Lzss {
    pub(crate) dictionary: u32,
}

impl Lzss {
    fn position_count_bits(self) -> u32 {
        if self.dictionary <= SMALL_DICTIONARY {
            4
        } else {
            5
        }
    }
}

struct Pending {
    distance: u32,
    left: usize,
}

pub(crate) struct Decoder<R> {
    bits: Bits<R>,
    window: Window,
    lzss: Lzss,
    remaining: u64,
    block_left: u32,
    trees: Huffman<TREE_SYMBOLS>,
    characters: Box<Huffman<CHARACTERS>>,
    positions: Huffman<POSITIONS>,
    pending: Option<Pending>,
    finished: bool,
}

impl<R: Read> Decoder<R> {
    pub(crate) fn new(inner: R, packed: u64, size: u64, lzss: Lzss) -> Self {
        Decoder {
            bits: Bits::new(inner, packed),
            window: Window::new(lzss.dictionary),
            lzss,
            remaining: size,
            block_left: 0,
            trees: Huffman::new(),
            characters: Box::new(Huffman::new()),
            positions: Huffman::new(),
            pending: None,
            finished: false,
        }
    }

    fn read_lengths<const N: usize>(
        bits: &mut Bits<R>,
        table: &mut Huffman<N>,
        count_bits: u32,
        skip_after: Option<usize>,
    ) -> io::Result<()> {
        let count = bits.bits(count_bits)? as usize;
        if count == 0 {
            let symbol = bits.bits(count_bits)? as usize;
            if symbol >= N {
                return Err(invalid("LZH Huffman table names a symbol it lacks"));
            }
            table.single(symbol as u16);
            return Ok(());
        }
        if count > N {
            return Err(invalid("LZH Huffman table has too many lengths"));
        }
        let mut lengths = [0u8; N];
        let mut index = 0;
        while index < count {
            let mut length = bits.bits(3)?;
            if length == 7 {
                while bits.bits(1)? == 1 {
                    length += 1;
                    if length > LONGEST_CODE as u32 {
                        return Err(invalid("LZH Huffman code is longer than 16 bits"));
                    }
                }
            }
            lengths[index] = length as u8;
            index += 1;
            if Some(index) == skip_after {
                index += bits.bits(2)? as usize;
            }
        }
        table.build(&lengths).map_err(field_error)
    }

    fn read_characters(&mut self) -> io::Result<()> {
        let count = self.bits.bits(CHARACTER_COUNT_BITS)? as usize;
        if count == 0 {
            let symbol = self.bits.bits(CHARACTER_COUNT_BITS)? as usize;
            if symbol >= CHARACTERS {
                return Err(invalid("LZH Huffman table names a symbol it lacks"));
            }
            self.characters.single(symbol as u16);
            return Ok(());
        }
        if count > CHARACTERS {
            return Err(invalid("LZH Huffman table has too many lengths"));
        }
        let mut lengths = [0u8; CHARACTERS];
        let mut index = 0;
        while index < count {
            let code = self.trees.decode(&mut self.bits)?;
            let zeros = match code {
                0 => 1,
                1 => self.bits.bits(4)? as usize + 3,
                2 => self.bits.bits(CHARACTER_COUNT_BITS)? as usize + 20,
                _ => {
                    lengths[index] = (code - 2) as u8;
                    index += 1;
                    continue;
                }
            };
            if index + zeros > count {
                return Err(invalid("LZH run of zero lengths passes its table"));
            }
            index += zeros;
        }
        self.characters.build(&lengths).map_err(field_error)
    }

    fn start_block(&mut self) -> io::Result<()> {
        self.block_left = self.bits.bits(BLOCK_SIZE_BITS)?;
        if self.block_left == 0 {
            return Err(invalid("LZH block holds no symbols"));
        }
        Self::read_lengths(
            &mut self.bits,
            &mut self.trees,
            TREE_COUNT_BITS,
            Some(TREE_SKIP_AFTER),
        )?;
        self.read_characters()?;
        Self::read_lengths(
            &mut self.bits,
            &mut self.positions,
            self.lzss.position_count_bits(),
            None,
        )
    }

    fn next_symbol(&mut self) -> io::Result<Option<u8>> {
        if self.block_left == 0 {
            self.start_block()?;
        }
        self.block_left -= 1;
        let symbol = self.characters.decode(&mut self.bits)?;
        if symbol < 256 {
            self.remaining -= 1;
            return Ok(Some(self.window.put(symbol as u8)));
        }
        let length = usize::from(symbol) - 256 + SHORT_MATCH;
        let slot = u32::from(self.positions.decode(&mut self.bits)?);
        let distance = if slot > 1 {
            (1 << (slot - 1)) + self.bits.bits(slot - 1)?
        } else {
            slot
        };
        if distance >= self.lzss.dictionary || !self.window.reaches(distance) {
            return Err(invalid("LZH match reaches before its window"));
        }
        if length as u64 > self.remaining {
            return Err(invalid("LZH match runs past the declared size"));
        }
        self.remaining -= length as u64;
        self.pending = Some(Pending {
            distance,
            left: length,
        });
        Ok(None)
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.remaining != 0 || self.pending.is_some() {
            return Err(invalid("LZH stream is shorter than its declared size"));
        }
        if self.block_left != 0 {
            return Err(invalid("LZH block holds more symbols than the file"));
        }
        self.bits.finish()?;
        self.finished = true;
        Ok(())
    }
}

fn field_error(error: BadField) -> io::Error {
    invalid(error.0)
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < output.len() {
            if let Some(pending) = &mut self.pending {
                let run = pending.left.min(output.len() - written);
                for byte in &mut output[written..written + run] {
                    *byte = self.window.repeat(pending.distance);
                }
                written += run;
                pending.left -= run;
                if pending.left == 0 {
                    self.pending = None;
                }
                continue;
            }
            if self.remaining == 0 {
                break;
            }
            if let Some(byte) = self.next_symbol()? {
                output[written] = byte;
                written += 1;
            }
        }
        if self.remaining == 0 && self.pending.is_none() {
            self.finish()?;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_crc16(bytes: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &byte in bytes {
            crc ^= u16::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xA001
                } else {
                    crc >> 1
                };
            }
        }
        crc
    }

    #[test]
    fn crc16_matches_a_bit_at_a_time_reference_and_its_check_value() {
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
        let mut bytes = Vec::new();
        for value in 0..2048u32 {
            bytes.push((value.wrapping_mul(2_654_435_761) >> 13) as u8);
            assert_eq!(crc16(0, &bytes), reference_crc16(&bytes));
        }
        let (left, right) = bytes.split_at(777);
        assert_eq!(crc16(crc16(0, left), right), reference_crc16(&bytes));
    }

    fn reference_lookup(lengths: &[u8], peek: u32) -> Option<(u16, u32)> {
        let mut code = 0u32;
        for length in 1..=16u32 {
            for (symbol, &l) in lengths.iter().enumerate() {
                if u32::from(l) == length {
                    if peek >> (16 - length) == code {
                        return Some((symbol as u16, length));
                    }
                    code += 1;
                }
            }
            code <<= 1;
        }
        None
    }

    fn kraft(lengths: &[u8]) -> u32 {
        lengths
            .iter()
            .filter(|&&length| length != 0)
            .map(|&length| 1u32 << (16 - u32::from(length)))
            .sum()
    }

    #[test]
    fn every_table_of_four_short_lengths_builds_exactly_when_complete_and_decodes_as_the_reference()
    {
        let mut tables = 0;
        for packed in 0..6u32.pow(4) {
            let mut lengths = [0u8; 4];
            let mut rest = packed;
            for length in &mut lengths {
                *length = (rest % 6) as u8;
                rest /= 6;
            }
            let mut table = Huffman::<4>::new();
            let built = table.build(&lengths).is_ok();
            assert_eq!(built, kraft(&lengths) == 1 << 16, "{lengths:?}");
            if built {
                tables += 1;
                for peek in (0..1u32 << 16).step_by(97).chain([0, 0xFFFF]) {
                    assert_eq!(
                        Some(table.lookup(peek)),
                        reference_lookup(&lengths, peek),
                        "{lengths:?} {peek:#x}"
                    );
                }
            }
        }
        assert!(tables > 20, "{tables}");
    }

    #[test]
    fn long_and_wide_tables_decode_as_the_reference() {
        let mut lengths = [0u8; CHARACTERS];
        let mut seed = 1u32;
        for round in 0..200 {
            let used = 2 + round % 300;
            let mut weights: Vec<u64> = (0..used)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    u64::from(seed >> 16) % 1000 + 1
                })
                .collect();
            if round % 7 == 0 {
                for (index, weight) in weights.iter_mut().enumerate() {
                    *weight = 1 << index.min(20);
                }
            }
            lengths.fill(0);
            let assigned = limited_lengths(&weights);
            for (index, length) in assigned.into_iter().enumerate() {
                lengths[(index * 7 + round) % CHARACTERS] = length;
            }
            let mut table = Huffman::<CHARACTERS>::new();
            table.build(&lengths).unwrap();
            for peek in (0..1u32 << 16).step_by(31) {
                assert_eq!(Some(table.lookup(peek)), reference_lookup(&lengths, peek));
            }
        }
    }

    fn limited_lengths(weights: &[u64]) -> Vec<u8> {
        let mut weights = weights.to_vec();
        loop {
            let mut nodes: Vec<(u64, Vec<usize>)> = weights
                .iter()
                .enumerate()
                .map(|(index, &weight)| (weight, vec![index]))
                .collect();
            let mut lengths = vec![0u8; weights.len()];
            while nodes.len() > 1 {
                nodes.sort_by_key(|node| std::cmp::Reverse(node.0));
                let (wa, a) = nodes.pop().unwrap();
                let (wb, b) = nodes.pop().unwrap();
                for &leaf in a.iter().chain(&b) {
                    lengths[leaf] += 1;
                }
                nodes.push((wa + wb, [a, b].concat()));
            }
            if lengths.iter().all(|&length| length <= 16) {
                return lengths;
            }
            for weight in &mut weights {
                *weight = weight.div_ceil(2);
            }
        }
    }

    #[test]
    fn bits_refuse_a_read_past_the_end_and_a_short_or_long_stream() {
        let data = [0b1010_0000u8, 0xFF];
        let mut bits = Bits::new(&data[..], 2);
        assert_eq!(bits.bits(3).unwrap(), 0b101);
        assert!(bits.finish().is_err());
        let mut bits = Bits::new(&data[..], 2);
        assert_eq!(bits.bits(16).unwrap(), 0xA0FF);
        assert!(bits.consume(1).is_err());
        let mut bits = Bits::new(&data[..1], 1);
        bits.bits(3).unwrap();
        bits.finish().unwrap();
        let mut bits = Bits::new(&data[..1], 2);
        bits.bits(3).unwrap();
        assert!(
            bits.finish().is_err(),
            "a stream cut before its packed size"
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const SYMBOLS: usize = 5;

    fn kraft(lengths: &[u8; SYMBOLS]) -> u32 {
        let mut sum = 0u32;
        let mut index = 0;
        while index < SYMBOLS {
            if lengths[index] != 0 && lengths[index] <= 16 {
                sum += 1u32 << (16 - u32::from(lengths[index]));
            }
            index += 1;
        }
        sum
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn tables_build_exactly_when_complete_and_every_peek_decodes_a_used_symbol() {
        let lengths: [u8; SYMBOLS] = kani::any();
        let mut table = Huffman::<SYMBOLS>::new();
        let built = table.build(&lengths).is_ok();
        let short_enough = lengths.iter().all(|&length| length <= 16);
        assert_eq!(built, short_enough && kraft(&lengths) == 1 << 16);
        if built {
            let peek: u32 = kani::any();
            kani::assume(peek < 1 << 16);
            let (symbol, length) = table.lookup(peek);
            assert!(usize::from(symbol) < SYMBOLS);
            assert_eq!(u32::from(lengths[usize::from(symbol)]), length);
            assert!(length >= 1);
        }
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn codes_that_share_a_prefix_decode_to_the_same_symbol() {
        let lengths: [u8; SYMBOLS] = kani::any();
        let mut table = Huffman::<SYMBOLS>::new();
        if table.build(&lengths).is_ok() {
            let peek: u32 = kani::any();
            let other: u32 = kani::any();
            kani::assume(peek < 1 << 16 && other < 1 << 16);
            let (symbol, length) = table.lookup(peek);
            kani::assume(peek >> (16 - length) == other >> (16 - length));
            assert_eq!(table.lookup(other), (symbol, length));
        }
    }

    #[kani::proof]
    fn a_single_symbol_table_reads_no_bits() {
        let symbol: u16 = kani::any();
        let peek: u32 = kani::any();
        let mut table = Huffman::<SYMBOLS>::new();
        table.single(symbol);
        assert_eq!(table.lookup(peek), (symbol, 0));
    }
}
