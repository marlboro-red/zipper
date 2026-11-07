use crate::{BadField, Field, field_check};

pub(crate) const CHUNK: usize = 32 * 1024;
pub(crate) const MIN_WINDOW_BITS: u8 = 15;
pub(crate) const MAX_WINDOW_BITS: u8 = 21;
const LITERALS: usize = 256;
const LENGTH_HEADERS: usize = 8;
const MOST_SLOTS: usize = 50;
const MAIN_SYMBOLS: usize = LITERALS + MOST_SLOTS * LENGTH_HEADERS;
const LENGTH_SYMBOLS: usize = 249;
const ALIGNED_SYMBOLS: usize = 8;
const PRETREE_SYMBOLS: usize = 20;
const MIN_MATCH: usize = 2;
const LONG_LENGTH_HEADER: usize = LENGTH_HEADERS - 1;
const LONGEST_CODE: usize = 16;
const TABLE_BITS: u32 = 10;
const SYMBOL_MASK: u16 = (1 << 11) - 1;
const LENGTH_SHIFT: u32 = 11;
const REPEAT_SLOTS: usize = 3;
const ALIGNED_EXTRA: u32 = 3;
const SLOTS_WITHOUT_EXTRA: usize = 4;
const SLOTS_BELOW_LONGEST_EXTRA: usize = 36;
const LONGEST_EXTRA: u32 = 17;
const TRANSLATION_LIMIT: u32 = 1 << 30;
const TRANSLATION_RESIDUE: usize = 10;
const E8: u8 = 0xE8;
const DELTA_BASE: u8 = 17;
const ZERO_RUN: usize = 17;
const LONG_ZERO_RUN: usize = 18;
const REPEAT_RUN: usize = 19;
const TABLE_OVERHEAD: u64 = 64 * 1024;

const fn extra_bits(slot: usize) -> u32 {
    if slot < SLOTS_WITHOUT_EXTRA {
        0
    } else if slot < SLOTS_BELOW_LONGEST_EXTRA {
        (slot as u32 - 2) / 2
    } else {
        LONGEST_EXTRA
    }
}

const POSITION_BASE: [u32; MOST_SLOTS + 1] = {
    let mut base = [0u32; MOST_SLOTS + 1];
    let mut slot = 0;
    while slot < MOST_SLOTS {
        base[slot + 1] = base[slot] + (1 << extra_bits(slot));
        slot += 1;
    }
    base
};

pub(crate) fn position_slots(window_bits: u8) -> usize {
    match window_bits {
        20 => 42,
        21 => 50,
        bits => 2 * bits as usize,
    }
}

pub(crate) fn decoder_memory(window_bits: u8) -> u64 {
    (1u64 << window_bits) + CHUNK as u64 + TABLE_OVERHEAD
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Verbatim,
    Aligned,
    Uncompressed,
}

struct Huffman {
    table: Vec<u16>,
    counts: [u32; LONGEST_CODE + 1],
    first: [u32; LONGEST_CODE + 1],
    start: [usize; LONGEST_CODE + 1],
    sorted: Vec<u16>,
    empty: bool,
}

impl Huffman {
    fn new() -> Self {
        Huffman {
            table: vec![0; 1 << TABLE_BITS],
            counts: [0; LONGEST_CODE + 1],
            first: [0; LONGEST_CODE + 1],
            start: [0; LONGEST_CODE + 1],
            sorted: Vec::new(),
            empty: true,
        }
    }

    fn rebuild(&mut self, lengths: &[u8], may_be_empty: bool) -> Field<()> {
        self.counts = [0; LONGEST_CODE + 1];
        for &length in lengths {
            self.counts[usize::from(length)] += 1;
        }
        self.counts[0] = 0;
        let space: u64 = (1..=LONGEST_CODE)
            .map(|length| u64::from(self.counts[length]) << (LONGEST_CODE - length))
            .sum();
        self.empty = space == 0;
        if self.empty {
            return field_check(may_be_empty, "LZX Huffman table is empty");
        }
        field_check(
            space == 1 << LONGEST_CODE,
            "LZX Huffman table is not a complete code",
        )?;
        let mut code = 0u32;
        let mut index = 0usize;
        for length in 1..=LONGEST_CODE {
            self.first[length] = code;
            self.start[length] = index;
            code = (code + self.counts[length]) << 1;
            index += self.counts[length] as usize;
        }
        self.sorted.clear();
        self.sorted.resize(index, 0);
        let mut next = self.start;
        let mut codes = self.first;
        for (symbol, &length) in lengths.iter().enumerate() {
            let length = usize::from(length);
            if length == 0 {
                continue;
            }
            self.sorted[next[length]] = symbol as u16;
            next[length] += 1;
            let code = codes[length];
            codes[length] += 1;
            if length as u32 <= TABLE_BITS {
                let shift = TABLE_BITS - length as u32;
                let entry = symbol as u16 | ((length as u16) << LENGTH_SHIFT);
                let from = (code << shift) as usize;
                self.table[from..from + (1 << shift)].fill(entry);
            } else {
                self.table[(code >> (length as u32 - TABLE_BITS)) as usize] = 0;
            }
        }
        Ok(())
    }

    fn decode(&self, bits: &mut Bits) -> Field<usize> {
        let peek = bits.peek(LONGEST_CODE as u32);
        let entry = self.table[(peek >> (LONGEST_CODE as u32 - TABLE_BITS)) as usize];
        let length = u32::from(entry >> LENGTH_SHIFT);
        if length != 0 {
            bits.consume(length);
            return Ok(usize::from(entry & SYMBOL_MASK));
        }
        for length in TABLE_BITS as usize + 1..=LONGEST_CODE {
            let index = (peek >> (LONGEST_CODE - length)).wrapping_sub(self.first[length]);
            if index < self.counts[length] {
                bits.consume(length as u32);
                return Ok(usize::from(
                    self.sorted[self.start[length] + index as usize],
                ));
            }
        }
        Err(BadField("LZX Huffman code is not in its table"))
    }
}

struct Bits<'a> {
    data: &'a [u8],
    start: usize,
    end: usize,
    next: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn at(data: &'a [u8], start: usize) -> Self {
        let end = start + ((data.len() - start) & !1);
        Bits {
            data,
            start,
            end,
            next: start,
            buffer: 0,
            count: 0,
        }
    }

    fn refill(&mut self) {
        while self.count <= 48 {
            let word = if self.next + 2 <= self.end {
                u16::from_le_bytes([self.data[self.next], self.data[self.next + 1]])
            } else {
                0
            };
            self.buffer |= u64::from(word) << (48 - self.count);
            self.count += 16;
            self.next += 2;
        }
    }

    fn peek(&mut self, count: u32) -> u32 {
        if self.count < count {
            self.refill();
        }
        (self.buffer >> (64 - count)) as u32
    }

    fn consume(&mut self, count: u32) {
        self.buffer <<= count;
        self.count -= count;
    }

    fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let value = self.peek(count);
        self.consume(count);
        value
    }

    fn consumed(&self) -> usize {
        (self.next - self.start) * 8 - self.count as usize
    }

    fn available(&self) -> usize {
        (self.end - self.start) * 8
    }

    fn over_read(&self) -> bool {
        self.consumed() > self.available()
    }

    fn ensure_in_input(&self) -> Field<()> {
        field_check(!self.over_read(), "truncated LZX chunk")
    }
}

struct Input<'a> {
    data: &'a [u8],
    bits: Bits<'a>,
    byte: usize,
    in_bytes: bool,
}

impl<'a> Input<'a> {
    fn new(data: &'a [u8], in_bytes: bool) -> Self {
        Input {
            data,
            bits: Bits::at(data, 0),
            byte: 0,
            in_bytes,
        }
    }

    fn switch_to_bytes(&mut self) -> Field<()> {
        let skip = 16 - (self.bits.consumed() % 16) as u32;
        field_check(
            self.bits.peek(skip) == 0,
            "LZX padding before an uncompressed block is not zero",
        )?;
        self.bits.consume(skip);
        self.bits.ensure_in_input()?;
        self.byte = self.bits.start + self.bits.consumed() / 8;
        self.in_bytes = true;
        Ok(())
    }

    fn switch_to_bits(&mut self) {
        self.bits = Bits::at(self.data, self.byte);
        self.in_bytes = false;
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.byte
    }

    fn take(&mut self, count: usize) -> Field<&'a [u8]> {
        field_check(
            self.remaining() >= count,
            "truncated LZX uncompressed block",
        )?;
        let bytes = &self.data[self.byte..self.byte + count];
        self.byte += count;
        Ok(bytes)
    }

    fn padding_byte(&mut self) -> Field<()> {
        field_check(
            self.take(1)? == [0],
            "LZX padding after an uncompressed block is not zero",
        )
    }

    fn finish(&mut self) -> Field<()> {
        if self.in_bytes {
            return field_check(self.remaining() == 0, "LZX chunk has bytes after its data");
        }
        self.bits.ensure_in_input()?;
        let unused = self.bits.available() - self.bits.consumed();
        field_check(
            self.bits.end == self.data.len() && unused < 16,
            "LZX chunk has bytes after its data",
        )?;
        field_check(
            unused == 0 || self.bits.peek(unused as u32) == 0,
            "LZX chunk ends with bits that are not zero",
        )
    }
}

fn delta(old: u8, symbol: usize) -> u8 {
    ((usize::from(old) + usize::from(DELTA_BASE) - symbol) % usize::from(DELTA_BASE)) as u8
}

fn read_lengths(
    bits: &mut Bits,
    pretree: &mut Huffman,
    lengths: &mut [u8],
    range: std::ops::Range<usize>,
) -> Field<()> {
    let mut pretree_lengths = [0u8; PRETREE_SYMBOLS];
    for length in &mut pretree_lengths {
        *length = bits.read(4) as u8;
    }
    pretree.rebuild(&pretree_lengths, false)?;
    let mut index = range.start;
    while index < range.end {
        let symbol = pretree.decode(bits)?;
        let (count, value) = match symbol {
            ZERO_RUN => (bits.read(4) as usize + 4, 0),
            LONG_ZERO_RUN => (bits.read(5) as usize + 20, 0),
            REPEAT_RUN => {
                let count = bits.read(1) as usize + 4;
                let symbol = pretree.decode(bits)?;
                field_check(
                    symbol < ZERO_RUN,
                    "LZX length run repeats a run code instead of a length",
                )?;
                (count, delta(lengths[index], symbol))
            }
            _ => (1, delta(lengths[index], symbol)),
        };
        field_check(
            count <= range.end - index,
            "LZX length run passes the end of its table",
        )?;
        lengths[index..index + count].fill(value);
        index += count;
    }
    bits.ensure_in_input()
}

pub(crate) fn translate_e8(data: &mut [u8], processed: u32, translation: u32) {
    if data.len() <= TRANSLATION_RESIDUE {
        return;
    }
    let limit = data.len() - TRANSLATION_RESIDUE;
    let mut index = 0;
    while index < limit {
        if data[index] != E8 {
            index += 1;
            continue;
        }
        let position = processed.wrapping_add(index as u32);
        let field = &mut data[index + 1..index + 5];
        let value = u32::from_le_bytes(field.try_into().unwrap());
        let converted = if value < translation {
            Some(value.wrapping_sub(position))
        } else if value > !position {
            Some(value.wrapping_add(translation))
        } else {
            None
        };
        if let Some(converted) = converted {
            field.copy_from_slice(&converted.to_le_bytes());
        }
        index += 5;
    }
}

pub(crate) struct Lzx {
    window: Vec<u8>,
    window_size: usize,
    slots: usize,
    position: usize,
    wrapped: bool,
    fresh: bool,
    reset_interval: Option<u32>,
    chunks: u32,
    repeats: [u32; REPEAT_SLOTS],
    kind: Kind,
    remaining: u32,
    padding: bool,
    main_lengths: [u8; MAIN_SYMBOLS],
    length_lengths: [u8; LENGTH_SYMBOLS],
    main: Huffman,
    length: Huffman,
    aligned: Huffman,
    pretree: Huffman,
    translation: u32,
    processed: u32,
    output: Vec<u8>,
}

impl Lzx {
    pub(crate) fn new(window_bits: u8, reset_interval: Option<u32>) -> Field<Self> {
        field_check(
            (MIN_WINDOW_BITS..=MAX_WINDOW_BITS).contains(&window_bits),
            "LZX window is not 2^15 to 2^21 bytes",
        )?;
        field_check(
            reset_interval != Some(0),
            "LZX reset interval is zero chunks",
        )?;
        let mut lzx = Lzx {
            window: vec![0; 1 << window_bits],
            window_size: 1 << window_bits,
            slots: position_slots(window_bits),
            position: 0,
            wrapped: false,
            fresh: true,
            reset_interval,
            chunks: 0,
            repeats: [1; REPEAT_SLOTS],
            kind: Kind::Verbatim,
            remaining: 0,
            padding: false,
            main_lengths: [0; MAIN_SYMBOLS],
            length_lengths: [0; LENGTH_SYMBOLS],
            main: Huffman::new(),
            length: Huffman::new(),
            aligned: Huffman::new(),
            pretree: Huffman::new(),
            translation: 0,
            processed: 0,
            output: Vec::with_capacity(CHUNK),
        };
        lzx.reset();
        Ok(lzx)
    }

    pub(crate) fn reset_to_window(&mut self, window_bits: u8) -> Field<()> {
        field_check(
            (MIN_WINDOW_BITS..=MAX_WINDOW_BITS).contains(&window_bits),
            "LZX window is not 2^15 to 2^21 bytes",
        )?;
        self.window_size = 1 << window_bits;
        if self.window.len() < self.window_size {
            self.window = vec![0; self.window_size];
        }
        self.slots = position_slots(window_bits);
        self.reset();
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.fresh = true;
        self.chunks = 0;
        self.repeats = [1; REPEAT_SLOTS];
        self.kind = Kind::Verbatim;
        self.remaining = 0;
        self.padding = false;
        self.main_lengths = [0; MAIN_SYMBOLS];
        self.length_lengths = [0; LENGTH_SYMBOLS];
    }

    pub(crate) fn chunk(&mut self, data: &[u8], size: usize) -> Field<&[u8]> {
        field_check(
            (1..=CHUNK).contains(&size),
            "LZX chunk is empty or larger than 32 KiB",
        )?;
        field_check(!data.is_empty(), "LZX chunk has no input")?;
        if self.reset_interval == Some(self.chunks) {
            self.reset();
        }
        if self.fresh {
            self.position = 0;
            self.wrapped = false;
        } else if self.position == self.window_size {
            self.position = 0;
            self.wrapped = true;
        }
        field_check(
            size <= self.window_size - self.position,
            "LZX chunk runs past the end of its window",
        )?;
        let mut input = Input::new(data, !self.fresh && self.kind == Kind::Uncompressed);
        if self.fresh {
            self.fresh = false;
            self.translation = if input.bits.read(1) == 1 {
                (input.bits.read(16) << 16) | input.bits.read(16)
            } else {
                0
            };
            self.processed = 0;
        }
        let start = self.position;
        let end = start + size;
        while self.position < end {
            if self.remaining == 0 {
                self.next_block(&mut input)?;
                continue;
            }
            let run = (self.remaining as usize).min(end - self.position);
            if self.kind == Kind::Uncompressed {
                let bytes = input.take(run)?;
                self.window[self.position..self.position + run].copy_from_slice(bytes);
            } else {
                self.decode_run(&mut input.bits, run)?;
            }
            self.remaining -= run as u32;
            self.position += run;
        }
        // EXTERNAL(7-Zip LzxDecoder): the padding byte of an uncompressed block that ends a
        // chunk sits either in that chunk, when one byte is left, or first in the next one.
        if self.kind == Kind::Uncompressed
            && self.remaining == 0
            && self.padding
            && input.remaining() == 1
        {
            input.padding_byte()?;
            self.padding = false;
        }
        input.finish()?;
        self.chunks += 1;
        if self.translation == 0 {
            return Ok(&self.window[start..end]);
        }
        self.output.clear();
        self.output.extend_from_slice(&self.window[start..end]);
        translate_e8(&mut self.output, self.processed, self.translation);
        self.processed = self.processed.wrapping_add(size as u32);
        if self.processed >= TRANSLATION_LIMIT {
            self.translation = 0;
        }
        Ok(&self.output)
    }

    fn next_block(&mut self, input: &mut Input) -> Field<()> {
        if self.padding {
            input.padding_byte()?;
            self.padding = false;
        }
        if input.in_bytes {
            input.switch_to_bits();
        }
        let bits = &mut input.bits;
        let kind = match bits.read(3) {
            1 => Kind::Verbatim,
            2 => Kind::Aligned,
            3 => Kind::Uncompressed,
            _ => return Err(BadField("LZX block type is not 1, 2 or 3")),
        };
        let high = bits.read(16);
        self.remaining = (high << 8) | bits.read(8);
        self.kind = kind;
        bits.ensure_in_input()?;
        if kind == Kind::Uncompressed {
            self.padding = self.remaining % 2 == 1;
            input.switch_to_bytes()?;
            let stored = input.take(4 * REPEAT_SLOTS)?;
            for (repeat, bytes) in self.repeats.iter_mut().zip(stored.chunks_exact(4)) {
                let value = u32::from_le_bytes(bytes.try_into().unwrap());
                field_check(
                    value != 0 && value as usize <= self.window_size - REPEAT_SLOTS,
                    "LZX uncompressed block stores a match distance outside its window",
                )?;
                *repeat = value;
            }
            return Ok(());
        }
        if kind == Kind::Aligned {
            let mut lengths = [0u8; ALIGNED_SYMBOLS];
            for length in &mut lengths {
                *length = bits.read(3) as u8;
            }
            self.aligned.rebuild(&lengths, false)?;
        }
        let main_symbols = LITERALS + self.slots * LENGTH_HEADERS;
        read_lengths(bits, &mut self.pretree, &mut self.main_lengths, 0..LITERALS)?;
        read_lengths(
            bits,
            &mut self.pretree,
            &mut self.main_lengths,
            LITERALS..main_symbols,
        )?;
        self.main
            .rebuild(&self.main_lengths[..main_symbols], false)?;
        read_lengths(
            bits,
            &mut self.pretree,
            &mut self.length_lengths,
            0..LENGTH_SYMBOLS,
        )?;
        self.length.rebuild(&self.length_lengths, true)
    }

    fn distance(&mut self, bits: &mut Bits, slot: usize) -> Field<u32> {
        let distance = match slot {
            0 => self.repeats[0],
            1 | 2 => {
                let distance = self.repeats[slot];
                self.repeats[slot] = self.repeats[0];
                distance
            }
            _ => {
                let extra = extra_bits(slot);
                let base = POSITION_BASE[slot] - 2;
                let distance = if self.kind == Kind::Aligned && extra >= ALIGNED_EXTRA {
                    let verbatim = bits.read(extra - ALIGNED_EXTRA) << ALIGNED_EXTRA;
                    base + verbatim + self.aligned.decode(bits)? as u32
                } else {
                    base + bits.read(extra)
                };
                self.repeats[2] = self.repeats[1];
                self.repeats[1] = self.repeats[0];
                distance
            }
        };
        self.repeats[0] = distance;
        Ok(distance)
    }

    fn decode_run(&mut self, bits: &mut Bits, run: usize) -> Field<()> {
        let window_size = self.window_size;
        let end = self.position + run;
        let mut at = self.position;
        while at < end {
            let symbol = self.main.decode(bits)?;
            if symbol < LITERALS {
                self.window[at] = symbol as u8;
                at += 1;
                continue;
            }
            let element = symbol - LITERALS;
            let mut length = (element % LENGTH_HEADERS) + MIN_MATCH;
            if element % LENGTH_HEADERS == LONG_LENGTH_HEADER {
                field_check(
                    !self.length.empty,
                    "LZX match needs a length table the block left empty",
                )?;
                length += self.length.decode(bits)?;
            }
            let distance = self.distance(bits, element / LENGTH_HEADERS)? as usize;
            field_check(
                length <= end - at,
                "LZX match runs past the end of its block or chunk",
            )?;
            let source = if distance <= at {
                at - distance
            } else {
                field_check(
                    self.wrapped && distance <= window_size,
                    "LZX match reaches before the start of its stream",
                )?;
                at + window_size - distance
            };
            if distance >= length && source + length <= window_size {
                self.window.copy_within(source..source + length, at);
            } else {
                for offset in 0..length {
                    self.window[at + offset] = self.window[(source + offset) % window_size];
                }
            }
            at += length;
        }
        bits.ensure_in_input()
    }
}

#[cfg(test)]
#[path = "../tests/common/lzx_encoder.rs"]
mod encoder;

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }

    fn decode_all(
        window_bits: u8,
        reset_interval: Option<u32>,
        chunks: &[encoder::Chunk],
    ) -> Field<Vec<u8>> {
        let mut lzx = Lzx::new(window_bits, reset_interval)?;
        let mut output = Vec::new();
        for chunk in chunks {
            output.extend_from_slice(lzx.chunk(&chunk.bytes, chunk.size)?);
        }
        Ok(output)
    }

    fn sample(random: &mut Random, length: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            match random.below(5) {
                0 => data.extend((0..random.below(40)).map(|_| random.next() as u8)),
                1 if !data.is_empty() => {
                    let back = 1 + random.below(data.len().min(70_000));
                    let length = 2 + random.below(300);
                    for _ in 0..length {
                        data.push(data[data.len() - back]);
                    }
                }
                2 => data.extend([E8, random.next() as u8, 0, 0, 0]),
                3 => data.extend(std::iter::repeat_n(
                    random.next() as u8 % 4,
                    random.below(60),
                )),
                _ => data.extend_from_slice(b"the quick brown fox jumps "),
            }
        }
        data.truncate(length);
        data
    }

    #[test]
    fn every_encoded_plan_decodes_to_its_input() {
        let mut random = Random(0x1234_5678_9abc_def1);
        for case in 0..160 {
            let window_bits = MIN_WINDOW_BITS + random.below(7) as u8;
            let length = match case % 4 {
                0 => 1 + random.below(40),
                1 => 1 + random.below(CHUNK * 2),
                _ => 1 + random.below(CHUNK * 5),
            };
            let data = sample(&mut random, length);
            let options = encoder::Options::random(&mut || random.next(), window_bits);
            let chunks = encoder::encode(&data, &options);
            assert_eq!(
                decode_all(window_bits, options.reset_interval, &chunks).unwrap(),
                data,
                "case {case} with {options:?}"
            );
        }
    }

    #[test]
    fn a_match_across_a_reset_is_refused_and_a_reset_forgets_the_tables() {
        let data: Vec<u8> = b"0123456789abcdef".repeat(CHUNK / 8);
        let options = encoder::Options {
            reset_interval: Some(1),
            ..encoder::Options::plain(16)
        };
        let chunks = encoder::encode(&data, &options);
        assert_eq!(decode_all(16, Some(1), &chunks).unwrap(), data);
        let mut across = encoder::Options::plain(16);
        across.reset_interval = Some(1);
        across.encode_resets = false;
        let chunks = encoder::encode(&data, &across);
        assert!(decode_all(16, Some(1), &chunks).is_err());
        assert_eq!(decode_all(16, None, &chunks).unwrap(), data);
    }

    #[test]
    fn every_short_chunk_decodes_or_is_refused_without_panicking() {
        for length in 1..=2usize {
            for value in 0..1u32 << (8 * length) {
                let bytes = &value.to_le_bytes()[..length];
                for size in [1, 2, 7, CHUNK] {
                    let _ = Lzx::new(15, None).unwrap().chunk(bytes, size);
                }
            }
        }
        let mut random = Random(0xfeed_beef_dead_cafe);
        for _ in 0..200_000 {
            let bytes: Vec<u8> = (0..1 + random.below(48))
                .map(|_| random.next() as u8)
                .collect();
            let mut lzx = Lzx::new(15 + random.below(7) as u8, None).unwrap();
            for _ in 0..3 {
                let _ = lzx.chunk(&bytes, 1 + random.below(CHUNK));
            }
        }
    }

    #[test]
    fn every_damaged_byte_and_cut_is_refused_or_decodes_without_panicking() {
        let mut random = Random(0x0bad_f00d_1234_4321);
        for case in 0..24 {
            let window_bits = MIN_WINDOW_BITS + (case % 7) as u8;
            let length = 1 + random.below(3000);
            let data = sample(&mut random, length);
            let options = encoder::Options::random(&mut || random.next(), window_bits);
            let chunks = encoder::encode(&data, &options);
            for which in 0..chunks.len() {
                let stride = chunks[which].bytes.len().div_ceil(1500);
                for index in (0..chunks[which].bytes.len()).step_by(stride) {
                    let mut damaged = chunks.clone();
                    let bit = random.below(8);
                    damaged[which].bytes[index] ^= 1 << bit;
                    let _ = decode_all(window_bits, options.reset_interval, &damaged);
                    let mut cut = chunks.clone();
                    cut[which].bytes.truncate(index);
                    let only_final_padding_cut = which == chunks.len() - 1
                        && index == chunks[which].bytes.len() - 1
                        && chunks[which].bytes[index] == 0;
                    if let Ok(output) = decode_all(window_bits, options.reset_interval, &cut) {
                        assert!(
                            only_final_padding_cut && output == data,
                            "case {case} accepted a chunk cut to {index} bytes"
                        );
                    }
                }
                let mut longer = chunks.clone();
                longer[which].bytes.extend_from_slice(&[0, 0]);
                assert!(decode_all(window_bits, options.reset_interval, &longer).is_err());
            }
        }
    }

    fn reference_e8(data: &mut [u8], processed: u32, translation: i32) {
        if data.len() <= 10 {
            return;
        }
        let mut position = 0;
        while position + 10 < data.len() {
            if data[position] != 0xE8 {
                position += 1;
                continue;
            }
            let current = processed as i64 + position as i64;
            let absolute = i32::from_le_bytes(data[position + 1..position + 5].try_into().unwrap());
            if i64::from(absolute) >= -current && absolute < translation {
                let relative = if absolute >= 0 {
                    i64::from(absolute) - current
                } else {
                    i64::from(absolute) + i64::from(translation)
                };
                data[position + 1..position + 5].copy_from_slice(&(relative as i32).to_le_bytes());
            }
            position += 5;
        }
    }

    #[test]
    fn e8_translation_matches_a_signed_reference_on_every_short_layout() {
        let mut random = Random(0x5eed_5eed_5eed_5eed);
        let values = [
            0u32,
            1,
            4,
            5,
            99,
            0x7FFF_FFFF,
            0x8000_0000,
            u32::MAX,
            u32::MAX - 3,
        ];
        for length in 0..=24usize {
            for case in 0..2000 {
                let mut data: Vec<u8> = (0..length)
                    .map(|_| {
                        if random.below(3) == 0 {
                            E8
                        } else {
                            random.next() as u8
                        }
                    })
                    .collect();
                if length >= 5 && case % 2 == 0 {
                    let at = random.below(length - 4);
                    data[at] = E8;
                    let value = values[random.below(values.len())];
                    data[at + 1..at + 5].copy_from_slice(&value.to_le_bytes());
                }
                let processed = [0u32, 1, 6, 1000, (1 << 30) - 5][random.below(5)];
                let translation = [1i32, 12, 1000, 12_000_000, i32::MAX][random.below(5)];
                let mut expected = data.clone();
                reference_e8(&mut expected, processed, translation);
                translate_e8(&mut data, processed, translation as u32);
                assert_eq!(data, expected, "length {length} case {case}");
            }
        }
    }

    #[test]
    fn huffman_tables_must_be_complete_codes() {
        let mut table = Huffman::new();
        assert!(table.rebuild(&[1, 1], false).is_ok());
        assert!(table.rebuild(&[2, 2, 1], false).is_ok());
        for lengths in [&[1u8][..], &[1, 2], &[1, 1, 1], &[2, 2, 2], &[16, 16]] {
            assert!(table.rebuild(lengths, false).is_err(), "{lengths:?}");
            assert!(table.rebuild(lengths, true).is_err(), "{lengths:?}");
        }
        assert!(table.rebuild(&[0, 0], false).is_err());
        assert!(table.rebuild(&[0, 0], true).is_ok() && table.empty);
        let mut deep = vec![0u8; 17];
        for (symbol, length) in deep.iter_mut().enumerate().take(16) {
            *length = symbol as u8 + 1;
        }
        deep[16] = 16;
        assert!(table.rebuild(&deep, false).is_ok());
        let mut bits = Bits::at(&[0xFF, 0xFF, 0xFF, 0xFF], 0);
        assert_eq!(table.decode(&mut bits).unwrap(), 16);
        assert_eq!(bits.consumed(), 16);
    }

    #[test]
    fn a_length_run_past_the_end_of_its_table_is_refused() {
        let run = |count: u32, range: std::ops::Range<usize>| {
            let mut writer = encoder::Writer::new();
            for symbol in 0..PRETREE_SYMBOLS {
                writer.bits(u32::from(symbol == 0 || symbol == LONG_ZERO_RUN), 4);
            }
            writer.bits(1, 1);
            writer.bits(count - 20, 5);
            writer.bits(0, 16);
            let bytes = writer.finish();
            let mut lengths = [7u8; 64];
            let result = read_lengths(
                &mut Bits::at(&bytes, 0),
                &mut Huffman::new(),
                &mut lengths,
                range,
            );
            (result, lengths)
        };
        assert!(run(51, 0..20).0.is_err());
        assert!(run(21, 0..20).0.is_err());
        let (result, lengths) = run(20, 0..20);
        assert!(result.is_ok());
        assert!(lengths[..20].iter().all(|&l| l == 0) && lengths[20] == 7);
    }

    fn uncompressed_chunk(repeats: [u32; 3], alignment: u32, data: &[u8], padding: u8) -> Vec<u8> {
        let mut writer = encoder::Writer::new();
        writer.bits(0, 1);
        writer.bits(3, 3);
        writer.bits(data.len() as u32 >> 8, 16);
        writer.bits(data.len() as u32 & 0xFF, 8);
        let pad = 16 - writer.pending_bits;
        writer.bits(alignment, pad);
        for repeat in repeats {
            writer.raw(&repeat.to_le_bytes());
        }
        writer.raw(data);
        if data.len() % 2 == 1 {
            writer.raw(&[padding]);
        }
        writer.finish()
    }

    #[test]
    fn uncompressed_blocks_with_bad_distances_or_padding_are_refused() {
        let decode = |bytes: &[u8]| {
            Lzx::new(15, None)
                .unwrap()
                .chunk(bytes, 3)
                .map(<[u8]>::to_vec)
        };
        assert_eq!(
            decode(&uncompressed_chunk([1, 2, 3], 0, b"abc", 0)).unwrap(),
            b"abc"
        );
        let largest = (1 << 15) - 3;
        assert!(decode(&uncompressed_chunk([largest, 1, 1], 0, b"abc", 0)).is_ok());
        for repeats in [[0, 1, 1], [1, 0, 1], [1, 1, 0], [largest + 1, 1, 1]] {
            assert!(
                decode(&uncompressed_chunk(repeats, 0, b"abc", 0)).is_err(),
                "{repeats:?}"
            );
        }
        assert!(decode(&uncompressed_chunk([1, 1, 1], 0, b"abc", 1)).is_err());
        assert!(decode(&uncompressed_chunk([1, 1, 1], 1, b"abc", 0)).is_err());
    }

    #[test]
    fn a_match_that_runs_past_its_block_or_reaches_before_the_stream_is_refused() {
        use encoder::Token::{Literal, Match};
        let decode = |block: usize, tokens: &[encoder::Token], size: usize| {
            let bytes = encoder::verbatim_chunk_of_exact_tokens(15, block, tokens);
            Lzx::new(15, None)
                .unwrap()
                .chunk(&bytes, size)
                .map(<[u8]>::to_vec)
        };
        let repeat = [
            Literal(b'a'),
            Match {
                length: 3,
                distance: 1,
            },
        ];
        assert_eq!(decode(4, &repeat, 4).unwrap(), b"aaaa");
        assert!(decode(3, &repeat, 3).is_err(), "the match passes its block");
        assert!(decode(4, &repeat, 3).is_err(), "the match passes its chunk");
        let pair = [
            Literal(b'a'),
            Literal(b'b'),
            Match {
                length: 2,
                distance: 2,
            },
        ];
        assert_eq!(decode(4, &pair, 4).unwrap(), b"abab");
        let early = [
            Literal(b'a'),
            Match {
                length: 2,
                distance: 2,
            },
        ];
        assert!(
            decode(3, &early, 3).is_err(),
            "the match reaches before the stream"
        );
        let far = [
            Literal(b'a'),
            Match {
                length: 2,
                distance: (1 << 15) - 3,
            },
        ];
        assert!(
            decode(3, &far, 3).is_err(),
            "the match reaches before the stream"
        );
    }

    #[test]
    fn position_tables_reach_exactly_each_window() {
        for bits in MIN_WINDOW_BITS..=MAX_WINDOW_BITS {
            let slots = position_slots(bits);
            assert_eq!(
                POSITION_BASE[slots],
                1 << bits,
                "the slots of a 2^{bits} window end at the window"
            );
        }
    }
}
